PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
CARGO ?= cargo
NOVA := target/release/nova

.PHONY: all build debug test check-examples install clean help

all: build

build:
	$(CARGO) build --release

debug:
	$(CARGO) build

test:
	$(CARGO) test

check-examples: debug
	$(CARGO) test --test feature_examples

install: build
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "$(NOVA)" "$(DESTDIR)$(BINDIR)/nova"
	@echo "Installed nova to $(DESTDIR)$(BINDIR)/nova"

clean:
	$(CARGO) clean

help:
	@echo "Nova build targets:"
	@echo "  make build          Build an optimized compiler"
	@echo "  make debug          Build a development compiler"
	@echo "  make test           Run all tests"
	@echo "  make check-examples Check every feature package"
	@echo "  make install        Install to PREFIX/bin"
	@echo "  make clean          Remove Cargo artifacts"
	@echo ""
	@echo "Install examples:"
	@echo "  sudo make install"
	@echo "  make install PREFIX=/custom/prefix"
	@echo "  make install DESTDIR=/package/root PREFIX=/usr"
