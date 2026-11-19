#!/bin/sh
set -eu

source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
app_id=ec.tufacturaya.printer
binary=tu-factura-ya-printer
case ${XDG_DATA_HOME:-} in
    /*) data_dir=$XDG_DATA_HOME ;;
    *) data_dir=$HOME/.local/share ;;
esac
app_dir=$data_dir/$app_id
entries=$data_dir/applications
theme=$data_dir/icons/hicolor
icons=$theme/256x256/apps

mkdir -p -- "$app_dir" "$entries" "$icons"
stage=$(mktemp -d "$data_dir/.tfy-printer-install.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
trap 'exit 1' HUP INT TERM
install -m 755 "$source_dir/$binary" "$stage/$binary"
install -m 644 "$source_dir/app-icon.png" "$stage/$app_id.png"
install -m 644 "$source_dir/$app_id.desktop" "$stage/$app_id.desktop"
mv -f -- "$stage/$binary" "$app_dir/$binary"
mv -f -- "$stage/$app_id.png" "$icons/$app_id.png"
touch -- "$theme"
mv -f -- "$stage/$app_id.desktop" "$entries/$app_id.desktop"
printf '%s\n' 'Tu Factura Ya Printer ya está disponible en el menú de aplicaciones.'
