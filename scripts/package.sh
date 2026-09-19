#!/bin/bash
# Package already-built artifacts. This script never invokes a compiler.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
binaries=$(cd "${1:-$root/target/release}" && pwd)
output=${2:-$root/dist}
case $(uname -s) in Linux) os=linux; library=libchartr_browser.so;; Darwin) os=macos; library=libchartr_browser.dylib;; *) exit 1;; esac
case $(uname -m) in x86_64) arch=x86_64;; arm64|aarch64) arch=aarch64;; *) exit 1;; esac
package="$output/package"
[[ ! -e "$package" ]] || { echo "package directory already exists: $package" >&2; exit 1; }
mkdir -p "$package"
cp "$root/chartr-plugin.toml" "$root/LICENSE" "$package/"
cp -R "$root/icons" "$package/"
cp "$binaries/$library" "$package/"
if [[ $os == linux ]]; then
    cp "$binaries/chartr-browser-helper" "$package/"
    bash "$root/package-runtime.sh" "$binaries" "$package/browser-runtime"
    strip --strip-unneeded "$package/$library" "$package/chartr-browser-helper" "$package/browser-runtime/"*.so*
else
    mkdir -p "$package/licenses/wry"
    cp "$root/vendor/wry/LICENSE-MIT" "$root/vendor/wry/LICENSE-APACHE" "$package/licenses/wry/"
    codesign --force --sign - "$package/$library"
fi
archive="chartr-plugin-$os-$arch.tar.gz"
tar -czf "$output/$archive" -C "$package" .
(cd "$output" && shasum -a 256 "$archive" > "$archive.sha256")
echo "$output/$archive"
