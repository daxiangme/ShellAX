#!/bin/sh
# Build the release binary, wrap it as ShellAX.app, and make a drag-install disk image.
#   dist/ShellAX.app
#   dist/ShellAX-<version>-macos-<arch>.dmg   (open it, drag ShellAX onto Applications)
# shellax-cli sits next to the GUI binary: the app looks for it in the same directory.
set -e
cd "$(dirname "$0")/.."
VER=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
OUT=${OUT:-dist}
case "$(uname -m)" in
  arm64) ARCH=arm64 ;;
  x86_64) ARCH=x64 ;;
  *) ARCH=$(uname -m) ;;
esac

if [ -z "$NOBUILD" ]; then
  cargo build --profile dist --bin shellax --bin shellax-cli
fi

APP="$OUT/ShellAX.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/dist/shellax "$APP/Contents/MacOS/shellax"
cp target/dist/shellax-cli "$APP/Contents/MacOS/shellax-cli"
cp THIRD_PARTY_NOTICES.md "$APP/Contents/Resources/THIRD_PARTY_NOTICES.md"
cp packaging/macos/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
chmod 755 "$APP/Contents/MacOS/shellax" "$APP/Contents/MacOS/shellax-cli"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>ShellAX</string>
  <key>CFBundleExecutable</key>
  <string>shellax</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>com.daxiangme.shellax</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>ShellAX</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VER}</string>
  <key>CFBundleVersion</key>
  <string>${VER}</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticTermination</key>
  <false/>
  <key>NSSupportsSuddenTermination</key>
  <false/>
</dict>
</plist>
EOF

# Ad-hoc signature so Apple Silicon will launch the app. This is not a Developer ID signature,
# so the first open from Finder still needs a right-click → Open, or clearing quarantine.
xattr -cr "$APP"
codesign --force -s - "$APP/Contents/MacOS/shellax-cli"
codesign --force --deep -s - "$APP"
codesign --verify --deep --strict "$APP"

DMG="$OUT/ShellAX-${VER}-macos-${ARCH}.dmg"
STAGE=$(mktemp -d)
ditto "$APP" "$STAGE/ShellAX.app"
ln -s /Applications "$STAGE/Applications"
cat > "$STAGE/安装说明.txt" <<EOF
ShellAX ${VER}（Apple Silicon / arm64）

安装：把 ShellAX 拖到「应用程序」文件夹。
命令行：ShellAX.app/Contents/MacOS/shellax-cli

这个包没有 Apple 开发者签名。第一次打开如果被系统拦住，
在 Finder 里对 ShellAX 点右键，选择「打开」。
EOF
rm -f "$DMG"
hdiutil create -volname "ShellAX" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
(cd "$OUT" && shasum -a 256 "$(basename "$DMG")" > SHA256SUMS.txt)
ls -lh "$APP/Contents/MacOS/shellax" "$DMG"
