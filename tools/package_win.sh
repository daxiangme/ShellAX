#!/bin/sh
# Cross-build the Windows x64 exe (cargo-xwin, MSVC target, dist profile) and package
#   $OUT/coossh-Setup-x64.exe   (NSIS, per-user, Start menu + desktop shortcut, uninstaller)
#   $OUT/coossh-portable-x64.zip
# Needs: cargo-xwin, clang, lld-link, llvm-rc, makensis, zip (see WINDOWS_TEST / PROGRESS).
set -e
cd "$(dirname "$0")/.."
OUT=${OUT:-/workspace/dist-win}
[ -n "$NOBUILD" ] || cargo xwin build --profile dist --target x86_64-pc-windows-msvc --bin coossh --bin coossh-cli
EXE=target/x86_64-pc-windows-msvc/dist/coossh.exe
ST=$(mktemp -d); mkdir -p "$OUT"
cp "$EXE" "$ST/coossh.exe"
cp target/x86_64-pc-windows-msvc/dist/coossh-cli.exe "$ST/coossh-cli.exe"
cp THIRD_PARTY_NOTICES.md docs/WINDOWS_TEST.md "$ST/"
cp docs/WINDOWS_TEST.md "$OUT/"
(cd packaging/windows && makensis -V2 -DSRC="$ST" -DOUT="$OUT/coossh-Setup-x64.exe" installer.nsi)
rm -f "$OUT/coossh-portable-x64.zip"
(cd "$ST" && mkdir coossh && mv coossh.exe coossh-cli.exe THIRD_PARTY_NOTICES.md WINDOWS_TEST.md coossh/ && zip -9 -q -r "$OUT/coossh-portable-x64.zip" coossh)
rm -rf "$ST"
(cd "$OUT" && sha256sum coossh-Setup-x64.exe coossh-portable-x64.zip > SHA256SUMS.txt && ls -la)
