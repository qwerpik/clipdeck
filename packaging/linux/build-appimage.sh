#!/usr/bin/env bash
# Build Clipdeck-x86_64.AppImage from target/release/clipdeck.
# Usage: packaging/linux/build-appimage.sh [path/to/appimagetool]
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
tool="${1:-appimagetool}"
app="$root/dist/Clipdeck.AppDir"
rm -rf "$app"
mkdir -p "$app/usr/bin" "$app/usr/share/applications" "$app/usr/share/metainfo"
install -m755 "$root/target/release/clipdeck" "$app/usr/bin/clipdeck"
install -m644 "$root/packaging/linux/io.github.qwerpik.clipdeck.desktop" "$app/usr/share/applications/"
install -m644 "$root/packaging/linux/io.github.qwerpik.clipdeck.desktop" "$app/"
install -m644 "$root/packaging/linux/io.github.qwerpik.clipdeck.metainfo.xml" "$app/usr/share/metainfo/"
for s in 16 24 32 48 64 128 256 512; do
  d="$app/usr/share/icons/hicolor/${s}x${s}/apps"
  mkdir -p "$d"
  install -m644 "$root/assets/clipdeck-$s.png" "$d/io.github.qwerpik.clipdeck.png"
done
install -m644 "$root/assets/clipdeck-256.png" "$app/io.github.qwerpik.clipdeck.png"
ln -sf io.github.qwerpik.clipdeck.png "$app/.DirIcon"
cat > "$app/AppRun" <<'RUN'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/clipdeck" "$@"
RUN
chmod 755 "$app/AppRun"
mkdir -p "$root/dist"
ARCH=x86_64 "$tool" --no-appstream "$app" "$root/dist/Clipdeck-x86_64.AppImage"
echo "built $root/dist/Clipdeck-x86_64.AppImage"
