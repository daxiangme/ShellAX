#!/bin/sh
# Cross-build the Windows x64 exe (cargo-xwin, MSVC target, dist profile) and package
#   $OUT/AgentShell-Setup-x64.exe   (NSIS, per-user, Start menu + desktop shortcut, uninstaller)
#   $OUT/AgentShell-portable-x64.zip
# Needs: cargo-xwin, clang, lld-link, llvm-rc, makensis, zip (see WINDOWS_TEST / PROGRESS).
set -e
cd "$(dirname "$0")/.."
OUT=${OUT:-/workspace/dist-win}
[ -n "$NOBUILD" ] || cargo xwin build --profile dist --target x86_64-pc-windows-msvc --bin ssh-term-proto
EXE=target/x86_64-pc-windows-msvc/dist/ssh-term-proto.exe
ST=$(mktemp -d); mkdir -p "$OUT"
cp "$EXE" "$ST/AgentShell.exe"
cp THIRD_PARTY_NOTICES.md packaging/windows/WINDOWS_TEST.md "$ST/"
cp packaging/windows/WINDOWS_TEST.md "$OUT/"
(cd packaging/windows && makensis -V2 -DSRC="$ST" -DOUT="$OUT/AgentShell-Setup-x64.exe" installer.nsi)
rm -f "$OUT/AgentShell-portable-x64.zip"
(cd "$ST" && mkdir AgentShell && mv AgentShell.exe THIRD_PARTY_NOTICES.md WINDOWS_TEST.md AgentShell/ && zip -9 -q -r "$OUT/AgentShell-portable-x64.zip" AgentShell)
rm -rf "$ST"
(cd "$OUT" && sha256sum AgentShell-Setup-x64.exe AgentShell-portable-x64.zip > SHA256SUMS.txt && ls -la)
