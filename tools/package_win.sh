#!/bin/sh
# Cross-build the Windows x64 exe (cargo-xwin, MSVC target, dist profile) and package
#   $OUT/ShellAX-Setup-x64.exe   (NSIS, per-user, Start menu + desktop shortcut, uninstaller)
#   $OUT/ShellAX-portable-x64.zip
# Needs: cargo-xwin, clang, lld-link, llvm-rc, makensis, zip (see WINDOWS_TEST / PROGRESS).
set -e
cd "$(dirname "$0")/.."
OUT=${OUT:-$PWD/dist-win}
[ -n "$NOBUILD" ] || cargo xwin build --profile dist --target x86_64-pc-windows-msvc --bin shellax --bin shellax-cli
EXE=target/x86_64-pc-windows-msvc/dist/shellax.exe
ST=$(mktemp -d); mkdir -p "$OUT"
cp "$EXE" "$ST/shellax.exe"
cp target/x86_64-pc-windows-msvc/dist/shellax-cli.exe "$ST/shellax-cli.exe"
cp THIRD_PARTY_NOTICES.md docs/WINDOWS_TEST.md "$ST/"
cp docs/WINDOWS_TEST.md "$OUT/"
(cd packaging/windows && makensis -V2 -DSRC="$ST" -DOUT="$OUT/ShellAX-Setup-x64.exe" installer.nsi)
rm -f "$OUT/ShellAX-portable-x64.zip"
(cd "$ST" && mkdir ShellAX && mv shellax.exe shellax-cli.exe THIRD_PARTY_NOTICES.md WINDOWS_TEST.md ShellAX/ && zip -9 -q -r "$OUT/ShellAX-portable-x64.zip" ShellAX)
rm -rf "$ST"
(cd "$OUT" && sha256sum ShellAX-Setup-x64.exe ShellAX-portable-x64.zip > SHA256SUMS.txt && ls -la)
