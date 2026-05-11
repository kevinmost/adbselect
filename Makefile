PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
INSTALLED_NAME ?= adbs-bin

CARGO ?= cargo
CARGO_FLAGS ?= +nightly -Zscript

TARGET_DIR ?= ./target
BINARY = $(TARGET_DIR)/release/adbs

.PHONY: all build install uninstall clean

all: build

build: $(BINARY)

$(BINARY): adbs.rs
	$(CARGO) $(CARGO_FLAGS) build --release --manifest-path adbs.rs --target-dir $(TARGET_DIR)

install: $(BINARY)
	install -d $(DESTDIR)$(BINDIR)
	install -m 755 $(BINARY) $(DESTDIR)$(BINDIR)/$(INSTALLED_NAME)

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/$(INSTALLED_NAME)

clean:
	rm -rf $(TARGET_DIR)
