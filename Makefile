PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
INSTALLED_NAME ?= adbs-bin

CARGO ?= cargo
CARGO_FLAGS ?=

TARGET_DIR ?= ./target
BINARY = $(TARGET_DIR)/release/adbs-bin

.PHONY: all build install uninstall clean

all: build

build: $(BINARY)

$(BINARY): src/main.rs Cargo.toml
	$(CARGO) build --release --target-dir $(TARGET_DIR)

install: $(BINARY)
	install -d $(DESTDIR)$(BINDIR)
	install -m 755 $(BINARY) $(DESTDIR)$(BINDIR)/$(INSTALLED_NAME)

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/$(INSTALLED_NAME)

clean:
	rm -rf $(TARGET_DIR)
