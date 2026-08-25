use std::io::{self, Read};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use crossterm::{
    execute,
    terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    event::{read, Event, KeyCode, KeyEvent, KeyModifiers, poll},
};
use ratatui::{
    prelude::*,
    widgets::*,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceMode {
    Adb,
    Fastboot,
}

#[derive(Clone)]
struct DeviceInfo {
    serial: String,
    name: String,
    fingerprint: String,
    release: String,
    sdk: String,
    users: Option<String>,
    loading: bool,
    mode: DeviceMode,
}

enum AppEvent {
    Key(KeyEvent),
    NewDevices(Vec<String>),
    NewFastbootDevices(Vec<String>),
    ResolvedInfo(String, DeviceInfo),
    ResolvedFastbootInfo(String, DeviceInfo),
}

fn get_device_users(serial: &str) -> Option<String> {
    let output = Command::new("adb")
        .args(&["-s", serial, "shell", "pm list users"])
        .output()
        .ok()?;
        
    if !output.status.success() {
        return None;
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut profiles = Vec::new();
    
    for line in stdout.lines() {
        if let (Some(start), Some(end)) = (line.find('{'), line.find('}')) {
            let inner = &line[start + 1..end];
            let parts: Vec<&str> = inner.split(':').collect();
            if parts.len() >= 2 {
                let id = parts[0].trim().to_string();
                let name = parts[1].trim().to_string();
                profiles.push(format!("{} ({})", id, name));
            }
        }
    }
    
    if profiles.len() > 1 {
        Some(format!("Users: {}", profiles.join(", ")))
    } else {
        None
    }
}

fn get_device_info(serial: &str) -> DeviceInfo {
    let name = Command::new("adb")
        .args(&["-s", serial, "shell", "settings", "get", "global", "device_name"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| serial.to_string());

    let name = if name == "null" || name.is_empty() {
        serial.to_string()
    } else {
        name
    };

    let props_output = Command::new("adb")
        .args(&["-s", serial, "shell", "getprop ro.product.build.fingerprint && getprop ro.build.version.release && getprop ro.build.version.sdk"])
        .output();

    let (fingerprint, release, sdk) = if let Ok(output) = props_output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let props: Vec<&str> = stdout.trim().split('\n').collect();
        (
            props.get(0).unwrap_or(&"Unknown").trim().to_string(),
            props.get(1).unwrap_or(&"Unknown").trim().to_string(),
            props.get(2).unwrap_or(&"Unknown").trim().to_string(),
        )
    } else {
        ("Could not retrieve properties".to_string(), "Unknown".to_string(), "Unknown".to_string())
    };

    let users = get_device_users(serial);

    DeviceInfo {
        serial: serial.to_string(),
        name,
        fingerprint,
        release,
        sdk,
        users,
        loading: false,
        mode: DeviceMode::Adb,
    }
}

fn get_fastboot_device_info(serial: &str) -> DeviceInfo {
    let product_output = Command::new("fastboot")
        .args(&["-s", serial, "getvar", "product"])
        .output();
        
    let product = if let Ok(output) = product_output {
        let text = String::from_utf8_lossy(&output.stderr).to_string()
            + &String::from_utf8_lossy(&output.stdout);
        text.lines()
            .find(|line| line.starts_with("product:"))
            .map(|line| line.strip_prefix("product:").unwrap().trim().to_string())
            .unwrap_or_else(|| "Unknown Product".to_string())
    } else {
        "Unknown Product".to_string()
    };

    DeviceInfo {
        serial: serial.to_string(),
        name: product,
        fingerprint: String::new(),
        release: String::new(),
        sdk: String::new(),
        users: None,
        loading: false,
        mode: DeviceMode::Fastboot,
    }
}

enum AppState {
    SelectingDevice,
    ConfirmAbort {
        current_serial: Option<String>,
        selected_option: usize, // 0 = Keep, 1 = Clear
    },
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {

    let (tx, rx) = mpsc::channel();

    // Thread for reading keys
    let key_tx = tx.clone();
    thread::spawn(move || {
        loop {
            if poll(Duration::from_millis(100)).unwrap() {
                if let Ok(Event::Key(key)) = read() {
                    key_tx.send(AppEvent::Key(key)).unwrap();
                }
            }
        }
    });

    // Thread for tracking devices
    let track_tx = tx.clone();
    thread::spawn(move || {
        let mut child = Command::new("adb")
            .arg("track-devices")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to start adb track-devices");
            
        let mut stdout = child.stdout.take().expect("Failed to open stdout");
        
        loop {
            let mut len_buf = [0; 4];
            if stdout.read_exact(&mut len_buf).is_err() {
                break; // Stream ended
            }
            let len_str = std::str::from_utf8(&len_buf).unwrap();
            let len = usize::from_str_radix(len_str, 16).unwrap();
            
            let mut data_buf = vec![0; len];
            stdout.read_exact(&mut data_buf).unwrap();
            
            let data_str = String::from_utf8(data_buf).unwrap();
            let lines: Vec<&str> = data_str.trim().split('\n').collect();
            let mut serials = Vec::new();
            
            for line in lines {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some((serial, status)) = line.split_once('\t') {
                    if status.trim() == "device" {
                        serials.push(serial.trim().to_string());
                    }
                }
            }
            track_tx.send(AppEvent::NewDevices(serials)).unwrap();
        }
    });

    // Thread for tracking fastboot devices
    let fastboot_tx = tx.clone();
    thread::spawn(move || {
        loop {
            let output = Command::new("fastboot")
                .arg("devices")
                .output();
                
            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let mut serials = Vec::new();
                    for line in stdout.lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Some((serial, mode)) = line.split_once('\t') {
                            let mode_trimmed = mode.trim();
                            if mode_trimmed == "fastboot" || mode_trimmed == "fastbootd" {
                                serials.push(serial.trim().to_string());
                            }
                        }
                    }
                    if fastboot_tx.send(AppEvent::NewFastbootDevices(serials)).is_err() {
                        break; // Main thread hung up
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    break; // fastboot is not installed, exit thread
                }
                Err(_) => {
                    // Ignore other errors and retry
                }
            }
            thread::sleep(Duration::from_millis(1000));
        }
    });

    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let mut adb_state = ListState::default();
    let mut fastboot_state = ListState::default();

    let mut app_state = AppState::SelectingDevice;

    let mut adb_devices: Vec<DeviceInfo> = Vec::new();
    let mut active_adb_serials: Vec<String> = Vec::new();

    let mut fastboot_devices: Vec<DeviceInfo> = Vec::new();
    let mut active_fastboot_serials: Vec<String> = Vec::new();

    let mut selected_index: usize = 0;

    let selected_serial = loop {
        let adb_len = adb_devices.len();
        let fastboot_len = fastboot_devices.len();
        let total_len = adb_len + fastboot_len;

        if total_len == 0 {
            selected_index = 0;
            adb_state.select(None);
            fastboot_state.select(None);
        } else {
            if selected_index >= total_len {
                selected_index = total_len - 1;
            }
            if selected_index < adb_len {
                adb_state.select(Some(selected_index));
                fastboot_state.select(None);
            } else {
                adb_state.select(None);
                fastboot_state.select(Some(selected_index - adb_len));
            }
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                ])
                .split(f.size());

            let header = Paragraph::new("Select a device to use as ADB_SERIAL")
                .block(Block::default().borders(Borders::ALL).title("adbs"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::White).bg(Color::Blue));
            f.render_widget(header, chunks[0]);

            let content_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .split(chunks[1]);

            let adb_items: Vec<ListItem> = adb_devices.iter().map(|dev| {
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(dev.name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::raw(" ("),
                        Span::styled(dev.serial.clone(), Style::default().fg(Color::Green)),
                        Span::raw(")"),
                    ]),
                ];

                if dev.loading {
                    lines.push(Line::from(vec![
                        Span::styled("    Loading properties...", Style::default().fg(Color::LightBlue).add_modifier(Modifier::ITALIC)),
                    ]));
                    lines.push(Line::from(vec![Span::raw("")]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(format!("    Fingerprint: {}", dev.fingerprint), Style::default().fg(Color::LightBlue)),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled(format!("    Android {} (SDK {})", dev.release, dev.sdk), Style::default().fg(Color::Yellow)),
                    ]));
                    if let Some(ref users) = dev.users {
                        lines.push(Line::from(vec![
                            Span::styled(format!("    {}", users), Style::default().fg(Color::LightMagenta)),
                        ]));
                    }
                }

                lines.push(Line::from(vec![
                    Span::styled("    ".to_string() + &"-".repeat(40), Style::default().fg(Color::DarkGray)),
                ]));
                
                ListItem::new(lines)
            }).collect();

            let adb_list = List::new(adb_items)
                .block(Block::default().borders(Borders::ALL).title("ADB Devices"))
                .highlight_style(Style::default().bg(Color::Rgb(50, 50, 50)))
                .highlight_symbol("* ");

            f.render_stateful_widget(adb_list, content_chunks[0], &mut adb_state);

            let fastboot_items: Vec<ListItem> = fastboot_devices.iter().map(|dev| {
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(dev.name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::raw(" ("),
                        Span::styled(dev.serial.clone(), Style::default().fg(Color::Green)),
                        Span::raw(")"),
                    ]),
                ];

                if dev.loading {
                    lines.push(Line::from(vec![
                        Span::styled("    Loading properties...", Style::default().fg(Color::LightBlue).add_modifier(Modifier::ITALIC)),
                    ]));
                    lines.push(Line::from(vec![Span::raw("")]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("    Mode: Fastboot", Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
                    ]));
                    lines.push(Line::from(vec![Span::raw("")]));
                }

                lines.push(Line::from(vec![
                    Span::styled("    ".to_string() + &"-".repeat(40), Style::default().fg(Color::DarkGray)),
                ]));
                
                ListItem::new(lines)
            }).collect();

            let fastboot_list = List::new(fastboot_items)
                .block(Block::default().borders(Borders::ALL).title("Fastboot Devices"))
                .highlight_style(Style::default().bg(Color::Rgb(50, 50, 50)))
                .highlight_symbol("* ");

            f.render_stateful_widget(fastboot_list, content_chunks[1], &mut fastboot_state);

            if let AppState::ConfirmAbort { ref current_serial, selected_option } = app_state {
                let area = centered_rect(75, 50, f.size());
                f.render_widget(Clear, area);

                // Draw block frame with red border for abort confirmation
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title("Confirm Abort")
                    .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
                
                let inner_area = block.inner(area);
                f.render_widget(block, area);

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1), // Title text
                        Constraint::Length(1), // Space
                        Constraint::Min(4),    // Device description
                        Constraint::Length(1), // Space
                        Constraint::Length(1), // Question
                        Constraint::Length(1), // Space
                        Constraint::Length(1), // Buttons
                    ])
                    .split(inner_area);

                // 1. Title line
                let title_p = Paragraph::new("The following device is currently selected as your ADB_SERIAL:")
                    .style(Style::default().add_modifier(Modifier::BOLD));
                f.render_widget(title_p, chunks[0]);

                // 2. Device description
                let mut desc_lines = Vec::new();
                if let Some(ref serial) = current_serial {
                    let dev = adb_devices.iter().find(|d| &d.serial == serial)
                        .or_else(|| fastboot_devices.iter().find(|d| &d.serial == serial));
                    if let Some(dev) = dev {
                        if dev.loading {
                            desc_lines.push(Line::from(vec![
                                Span::styled(dev.name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                                Span::raw(" ("),
                                Span::styled(dev.serial.clone(), Style::default().fg(Color::Green)),
                                Span::raw(")"),
                            ]));
                            desc_lines.push(Line::from(vec![
                                Span::styled("  Loading properties...", Style::default().fg(Color::LightBlue).add_modifier(Modifier::ITALIC)),
                            ]));
                        } else if dev.mode == DeviceMode::Fastboot {
                            desc_lines.push(Line::from(vec![
                                Span::styled(dev.name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                                Span::raw(" ("),
                                Span::styled(dev.serial.clone(), Style::default().fg(Color::Green)),
                                Span::raw(")"),
                            ]));
                            desc_lines.push(Line::from(vec![
                                Span::styled("  Mode: Fastboot", Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
                            ]));
                        } else {
                            desc_lines.push(Line::from(vec![
                                Span::styled(dev.name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                                Span::raw(" ("),
                                Span::styled(dev.serial.clone(), Style::default().fg(Color::Green)),
                                Span::raw(")"),
                            ]));
                            desc_lines.push(Line::from(vec![
                                Span::styled(format!("  Fingerprint: {}", dev.fingerprint), Style::default().fg(Color::LightBlue)),
                            ]));
                            desc_lines.push(Line::from(vec![
                                Span::styled(format!("  Android {} (SDK {})", dev.release, dev.sdk), Style::default().fg(Color::Yellow)),
                            ]));
                            if let Some(ref users) = dev.users {
                                desc_lines.push(Line::from(vec![
                                    Span::styled(format!("  {}", users), Style::default().fg(Color::LightMagenta)),
                                ]));
                            }
                        }
                    } else {
                        desc_lines.push(Line::from(vec![
                            Span::styled(format!("(Serial: {})", serial), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        ]));
                    }
                } else {
                    desc_lines.push(Line::from(vec![
                        Span::styled("(None)", Style::default().fg(Color::DarkGray)),
                    ]));
                }

                let desc_p = Paragraph::new(desc_lines);
                f.render_widget(desc_p, chunks[2]);

                // 4. Question line
                let question_p = Paragraph::new("What would you like to do?")
                    .style(Style::default().add_modifier(Modifier::BOLD));
                f.render_widget(question_p, chunks[4]);

                // 6. Buttons side-by-side
                let button_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(50),
                        Constraint::Percentage(50),
                    ])
                    .split(chunks[6]);

                let keep_style = if selected_option == 0 {
                    Style::default().bg(Color::Green).fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                };
                let clear_style = if selected_option == 1 {
                    Style::default().bg(Color::Red).fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Red)
                };

                let keep_button = Paragraph::new("[ Keep It ]")
                    .alignment(Alignment::Center)
                    .style(keep_style);
                let clear_button = Paragraph::new("[ Clear ADB Serial ]")
                    .alignment(Alignment::Center)
                    .style(clear_style);

                f.render_widget(keep_button, button_chunks[0]);
                f.render_widget(clear_button, button_chunks[1]);
            }
        })?;

        if let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) {
            match event {
                AppEvent::Key(KeyEvent { code, modifiers, .. }) => {
                    match app_state {
                        AppState::SelectingDevice => {
                            match code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let total_len = adb_devices.len() + fastboot_devices.len();
                                    if total_len > 0 {
                                        selected_index = if selected_index == 0 {
                                            total_len - 1
                                        } else {
                                            selected_index - 1
                                        };
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let total_len = adb_devices.len() + fastboot_devices.len();
                                    if total_len > 0 {
                                        selected_index = if selected_index >= total_len - 1 {
                                            0
                                        } else {
                                            selected_index + 1
                                        };
                                    }
                                }
                                KeyCode::Enter => {
                                    let adb_len = adb_devices.len();
                                    let total_len = adb_len + fastboot_devices.len();
                                    if total_len > 0 && selected_index < total_len {
                                        if selected_index < adb_len {
                                            break Some(adb_devices[selected_index].serial.clone());
                                        } else {
                                            break Some(fastboot_devices[selected_index - adb_len].serial.clone());
                                        }
                                    }
                                }
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    let current = std::env::var("ADB_SERIAL").ok();
                                    if current.is_some() {
                                        app_state = AppState::ConfirmAbort {
                                            current_serial: current,
                                            selected_option: 0,
                                        };
                                    } else {
                                        break None;
                                    }
                                }
                                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                                    let current = std::env::var("ADB_SERIAL").ok();
                                    if current.is_some() {
                                        app_state = AppState::ConfirmAbort {
                                            current_serial: current,
                                            selected_option: 0,
                                        };
                                    } else {
                                        break None;
                                    }
                                }
                                _ => {}
                            }
                        }
                        AppState::ConfirmAbort { ref current_serial, ref mut selected_option } => {
                            match code {
                                KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') | KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l') => {
                                    *selected_option = if *selected_option == 0 { 1 } else { 0 };
                                }
                                KeyCode::Enter => {
                                    if *selected_option == 0 {
                                        break current_serial.clone();
                                    } else {
                                        break Some("__CLEAR__".to_string());
                                    }
                                }
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    break current_serial.clone();
                                }
                                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                                    break current_serial.clone();
                                }
                                _ => {}
                            }
                        }
                    }
                }
                AppEvent::NewDevices(new_serials) => {
                    // Find removed devices
                    adb_devices.retain(|d| new_serials.contains(&d.serial));
                    active_adb_serials.retain(|s| new_serials.contains(s));

                    // Find new devices
                    for serial in new_serials {
                        if !active_adb_serials.contains(&serial) {
                            active_adb_serials.push(serial.clone());
                            adb_devices.push(DeviceInfo {
                                serial: serial.clone(),
                                name: serial.clone(),
                                fingerprint: String::new(),
                                release: String::new(),
                                sdk: String::new(),
                                users: None,
                                loading: true,
                                mode: DeviceMode::Adb,
                            });

                            // Spawn thread to fetch info
                            let info_tx = tx.clone();
                            let serial_clone = serial.clone();
                            thread::spawn(move || {
                                let info = get_device_info(&serial_clone);
                                info_tx.send(AppEvent::ResolvedInfo(serial_clone, info)).unwrap();
                            });
                        }
                    }

                    // Adjust selection if out of bounds
                    let total_len = adb_devices.len() + fastboot_devices.len();
                    if selected_index >= total_len && total_len > 0 {
                        selected_index = total_len - 1;
                    }
                }
                AppEvent::NewFastbootDevices(new_serials) => {
                    // Find removed devices
                    fastboot_devices.retain(|d| new_serials.contains(&d.serial));
                    active_fastboot_serials.retain(|s| new_serials.contains(s));

                    // Find new devices
                    for serial in new_serials {
                        if !active_fastboot_serials.contains(&serial) {
                            active_fastboot_serials.push(serial.clone());
                            fastboot_devices.push(DeviceInfo {
                                serial: serial.clone(),
                                name: serial.clone(),
                                fingerprint: String::new(),
                                release: String::new(),
                                sdk: String::new(),
                                users: None,
                                loading: true,
                                mode: DeviceMode::Fastboot,
                            });

                            // Spawn thread to fetch info
                            let info_tx = tx.clone();
                            let serial_clone = serial.clone();
                            thread::spawn(move || {
                                let info = get_fastboot_device_info(&serial_clone);
                                info_tx.send(AppEvent::ResolvedFastbootInfo(serial_clone, info)).unwrap();
                            });
                        }
                    }

                    // Adjust selection if out of bounds
                    let total_len = adb_devices.len() + fastboot_devices.len();
                    if selected_index >= total_len && total_len > 0 {
                        selected_index = total_len - 1;
                    }
                }
                AppEvent::ResolvedInfo(serial, info) => {
                    if let Some(dev) = adb_devices.iter_mut().find(|d| d.serial == serial) {
                        *dev = info;
                        dev.loading = false;
                    }
                }
                AppEvent::ResolvedFastbootInfo(serial, info) => {
                    if let Some(dev) = fastboot_devices.iter_mut().find(|d| d.serial == serial) {
                        *dev = info;
                        dev.loading = false;
                    }
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Some(serial) = selected_serial {
        println!("{}", serial);
    }
    Ok(())
}
