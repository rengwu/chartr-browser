#!/bin/bash
# Copy the CEF distribution staged by cef-rs beside Cargo's Linux executable.
set -euo pipefail
source_dir=$(realpath "${1:?binary directory required}")
destination=${2:?destination required}
plugin_dir=$(cd "$(dirname "$0")" && pwd)
files=(libcef.so libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1
       chrome_100_percent.pak chrome_200_percent.pak resources.pak icudtl.dat
       v8_context_snapshot.bin vk_swiftshader_icd.json archive.json CREDITS.html)
for file in "${files[@]}" locales/en-US.pak chrome-sandbox; do
    [[ -s "$source_dir/$file" ]] || {
        echo "missing bundled Chromium file: $source_dir/$file" >&2; exit 1;
    }
done
mkdir -p "$destination"
for file in "${files[@]}"; do
    cp -p "$source_dir/$file" "$destination/$file"
done
cp -a "$source_dir/locales" "$destination/"
install -m644 "$plugin_dir/LICENSE-CEF.txt" "$destination/LICENSE-CEF.txt"
# Chromium looks beside its executable for the optional setuid helper. Keep
# tarballs unprivileged; deb/Arch packaging sets root ownership and mode 4755.
install -m755 "$source_dir/chrome-sandbox" "$destination/../chrome-sandbox"
