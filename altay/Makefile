# Altay — build & install
#
#   make                 build the release binary
#   sudo make install    install system-wide (PREFIX=/usr/local)
#   make install-user    install for the current user (~/.local)
#   sudo make uninstall  remove a system-wide install
#   make uninstall-user  remove a user install
#
# Honours the standard PREFIX and DESTDIR variables for packaging.

PREFIX     ?= /usr/local
DESTDIR    ?=
BIN         = target/release/altay
APP_ID      = altay

USER_PREFIX = $(HOME)/.local

.PHONY: build deb install uninstall install-user uninstall-user clean check

build:
	cargo build --release

# Build the Debian package via the helper script.
deb:
	scripts/build-deb.sh --lint

# ---- System-wide ----
install: build
	install -Dm755 $(BIN)            $(DESTDIR)$(PREFIX)/bin/$(APP_ID)
	install -Dm644 $(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 assets/$(APP_ID).png \
	        $(DESTDIR)$(PREFIX)/share/icons/hicolor/256x256/apps/$(APP_ID).png
	-update-desktop-database $(DESTDIR)$(PREFIX)/share/applications 2>/dev/null || true
	-gtk-update-icon-cache $(DESTDIR)$(PREFIX)/share/icons/hicolor 2>/dev/null || true
	@echo "Installed Altay to $(DESTDIR)$(PREFIX). Launch it from your app menu or run 'altay'."

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(APP_ID)
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/256x256/apps/$(APP_ID).png

# ---- Per-user (no root) ----
install-user: build
	install -Dm755 $(BIN)            $(USER_PREFIX)/bin/$(APP_ID)
	install -Dm644 $(APP_ID).desktop $(USER_PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 assets/$(APP_ID).png \
	        $(USER_PREFIX)/share/icons/hicolor/256x256/apps/$(APP_ID).png
	-update-desktop-database $(USER_PREFIX)/share/applications 2>/dev/null || true
	@echo "Installed Altay for $(USER). Ensure $(USER_PREFIX)/bin is on your PATH."

uninstall-user:
	rm -f $(USER_PREFIX)/bin/$(APP_ID)
	rm -f $(USER_PREFIX)/share/applications/$(APP_ID).desktop
	rm -f $(USER_PREFIX)/share/icons/hicolor/256x256/apps/$(APP_ID).png

check:
	desktop-file-validate $(APP_ID).desktop

clean:
	cargo clean
