#!/bin/sh
# Regenerate vendor/gpui-pre-windows/prebuilt/shaders_bytes.rs with Microsoft fxc.exe under wine
# (same modules / profiles / flags as gpui-pre-windows' own build.rs, which only runs on a Windows host).
# FXC: wrapper that runs fxc.exe (default /tmp/sdkbt/fxc/fxc.sh; fxc.exe+d3dcompiler_47.dll from the
# Microsoft.Windows.SDK.CPP NuGet package, c/bin/<ver>/x64/).
set -e
cd "$(dirname "$0")/.."
FXC=${FXC:-/tmp/sdkbt/fxc/fxc.sh}
V=$PWD/vendor/gpui-pre-windows; T=$(mktemp -d); OUT=$V/prebuilt/shaders_bytes.rs
mkdir -p $V/prebuilt; : > $OUT.tmp
one() { # module file
  for k in vertex:vs:VERTEX fragment:ps:FRAGMENT; do
    e=${k%%:*}; r=${k#*:}; p=${r%%:*}; up=${r#*:}
    M=$(echo $1 | tr a-z A-Z); name=${M}_${up}_BYTES
    $FXC /T ${p}_4_1 /E ${1}_$e /Fh $T/$1_$p.h /Vn $name /O3 $2 >/dev/null 2>&1 || { echo "fxc failed: $1 $e"; $FXC /T ${p}_4_1 /E ${1}_$e /Fh $T/x.h /Vn X /O3 $2; exit 1; }
    python3 - "$T/$1_$p.h" "$name" >> $OUT.tmp <<'PY'
import sys
h=open(sys.argv[1]).read(); g=h[h.index("const BYTE"):]; v=g[g.index("=")+1:].strip()
print(f"const {sys.argv[2]}: &[u8] = &{v.replace('{','[').replace('}',']')}")
PY
  done
}
for m in quad shadow path_rasterization path_sprite underline monochrome_sprite subpixel_sprite polychrome_sprite; do one $m $V/src/shaders.hlsl; done
one emoji_rasterization $V/src/color_text_raster.hlsl
mv $OUT.tmp $OUT; rm -rf $T; echo "wrote $OUT ($(wc -c <$OUT) bytes)"
