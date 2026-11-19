#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")" && pwd)
dest=${TFY_PRINTER_DIST:-"$root/dist"}
binary=tu-factura-ya-printer
executable=${1:-"$root/target/x86_64-unknown-linux-gnu/release/$binary"}
icon="$root/assets/app-icon.png"
version=$(awk -F '"' '
  /^\[package\]$/ { in_package = 1; next }
  /^\[/ { in_package = 0 }
  in_package && /^version[[:space:]]*=/ { print $2; exit }
' "$root/Cargo.toml")
[ -n "$version" ] || { echo 'No se encontró la versión en Cargo.toml.' >&2; exit 1; }
[ -x "$executable" ] || { printf 'No se encontró el ejecutable de Linux: %s\n' "$executable" >&2; exit 1; }
[ -f "$icon" ] || { printf 'No se encontró el icono de la aplicación: %s\n' "$icon" >&2; exit 1; }

name=Tu-Factura-Ya-Printer-$version-Linux-x64
mkdir -p "$dest"
stage=$(mktemp -d "$dest/.linux-package.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
install -d "$stage/$name"
install -m 755 "$executable" "$stage/$name/$binary"
install -m 755 "$root/packaging/linux/install-launcher.sh" "$stage/$name/install-launcher.sh"
install -m 644 "$root/packaging/linux/ec.tufacturaya.printer.desktop" "$stage/$name/ec.tufacturaya.printer.desktop"
install -m 644 "$icon" "$stage/$name/app-icon.png"
install -m 644 "$root/packaging/linux/LEEME.txt" "$stage/$name/LEEME.txt"

COPYFILE_DISABLE=1 tar --format=ustar --no-xattrs -czf "$stage/$name.tar.gz" -C "$stage" "$name"
mv -f -- "$stage/$name.tar.gz" "$dest/$name.tar.gz"
printf '%s\n' "$dest/$name.tar.gz"
