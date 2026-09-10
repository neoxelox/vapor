#!/bin/sh
# Install icon assets only. No app executable or desktop launcher is installed.
set -eu
prefix=${1:-"$HOME/.local"}
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
find "$source_dir/hicolor" -type f | while IFS= read -r asset; do
    relative=${asset#"$source_dir/"}
    destination="$prefix/share/icons/$relative"
    mkdir -p "$(dirname -- "$destination")"
    cp "$asset" "$destination"
done
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$prefix/share/icons/hicolor/index.theme" ]; then
    gtk-update-icon-cache -f "$prefix/share/icons/hicolor"
fi
printf 'Installed Vapor icons under %s/share/icons/hicolor\n' "$prefix"
