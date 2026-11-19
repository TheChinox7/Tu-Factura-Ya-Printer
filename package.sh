#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")" && pwd)
dest=${TFY_PRINTER_DIST:-"$root/dist"}
name='Tu Factura Ya Printer'
binary=tu-factura-ya-printer
platform=${1:-}
[ -n "$platform" ] || { echo 'Uso: ./package.sh mac-arm64 | mac-universal | windows-x64'; exit 1; }
version=$(awk -F '"' '
  /^\[package\]$/ { in_package = 1; next }
  /^\[/ { in_package = 0 }
  in_package && /^version[[:space:]]*=/ { print $2; exit }
' "$root/Cargo.toml")
[ -n "$version" ] || { echo 'No se encontró la versión en Cargo.toml.' >&2; exit 1; }
mkdir -p "$dest"

case "$platform" in
  mac-arm64|mac-universal)
    native=${2:-"$root/target/release/$binary"}
    package="$dest/$name.app"
    icon_file=app-icon.icns
    icon="$root/assets/$icon_file"
    if [ ! -f "$icon" ]; then
      printf 'No se encontró el icono de macOS en %s\n' "$icon" >&2
      exit 1
    fi
    mkdir -p "$package/Contents/MacOS" "$package/Contents/Resources"
    if [ "$platform" = mac-universal ]; then
      intel=${3:-"$root/target/x86_64-apple-darwin/release/$binary"}
      lipo -create "$native" "$intel" -output "$package/Contents/MacOS/$binary"
    else
      install -m 755 "$native" "$package/Contents/MacOS/$binary"
    fi
    install -m 644 "$icon" "$package/Contents/Resources/$icon_file"
    cat > "$package/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>$binary</string>
  <key>CFBundleIdentifier</key><string>ec.tufacturaya.printer</string>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleDisplayName</key><string>$name</string>
  <key>CFBundleIconFile</key><string>$icon_file</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
    plutil -lint "$package/Contents/Info.plist"
    codesign --force --sign "${TFY_PRINTER_SIGN_IDENTITY:--}" --options runtime "$package"
    codesign --verify --strict "$package"
    ditto -c -k --sequesterRsrc --keepParent "$package" "$dest/Tu-Factura-Ya-Printer-$version-$platform.zip"
    ;;
  windows-x64)
    executable=${2:-"$root/target/x86_64-pc-windows-gnu/release/$binary.exe"}
    name=Tu-Factura-Ya-Printer-$version-Windows-x64
    install -m 755 "$executable" "$dest/$name.exe"
    (cd "$dest" && zip -q "$name.zip" "$name.exe")
    ;;
  *) echo "Plataforma desconocida: $platform"; exit 1 ;;
esac
