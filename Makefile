PREFIX ?= /usr/local

build:
	cargo build --release

install: build
	install -d $(PREFIX)/bin
	install -m 755 target/release/chatsh $(PREFIX)/bin/chatsh

uninstall:
	rm -f $(PREFIX)/bin/chatsh

test:
	cargo test

clean:
	cargo clean

.PHONY: build install uninstall test clean
