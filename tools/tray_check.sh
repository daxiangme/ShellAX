#!/bin/sh
# Verify the tray (StatusNotifierItem) on a bare X server: private session bus + test watcher.
# Usage: tools/tray_check.sh  (then launch the GUI with the printed DBUS_SESSION_BUS_ADDRESS)
cd "$(dirname "$0")/.." || exit 1
A=$(dbus-daemon --session --fork --print-address); export DBUS_SESSION_BUS_ADDRESS=$A
cargo build --release --example sni_watcher -q && (nohup ./target/release/examples/sni_watcher >/tmp/sni_watcher.log 2>&1 &)
echo "export DBUS_SESSION_BUS_ADDRESS='$A'"
echo "then: busctl --user get-property <item> /StatusNotifierItem org.kde.StatusNotifierItem Title"
echo "      busctl --user call <item> /MenuBar com.canonical.dbusmenu GetLayout iias 0 -- -1 0"
