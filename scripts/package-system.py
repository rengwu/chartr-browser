#!/usr/bin/env python3
"""Package a ready-built Linux plugin, including the standard sandbox helper.
No compiler or package build hook is ever run during plugin installation.
"""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('format', choices=['deb', 'arch'])
    parser.add_argument('package', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    package = args.package.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    manifest = tomllib.loads((package / 'chartr-plugin.toml').read_text())
    assert manifest['id'] == 'com.chartr.browser' and manifest['kind'] == 'embedded'
    version = manifest['version']
    assert all(c.isalnum() or c in '.-' for c in version)
    for name in ('libchartr_browser.so', 'chartr-browser-helper', 'chrome-sandbox', 'browser-runtime/libcef.so'):
        assert (package / name).is_file(), f'missing {name}'
    with tempfile.TemporaryDirectory(prefix='chartr-browser-package-') as temporary:
        work = Path(temporary)
        if args.format == 'deb':
            root = work / 'root'
            library = root / 'usr/lib/chartr/plugins/com.chartr.browser'
            shutil.copytree(package, library)
            (library / 'chrome-sandbox').chmod(0o4755)
            (root / 'DEBIAN').mkdir()
            (work / 'debian').mkdir()
            (work / 'debian/control').write_text('Source: chartr-browser\nSection: utils\nPriority: optional\nMaintainer: Chartr contributors\n\nPackage: chartr-browser\nArchitecture: amd64\nDescription: Browser plugin for Chartr\n')
            deps = subprocess.check_output(['dpkg-shlibdeps', '-O', '--ignore-missing-info',
                '-l' + str(library / 'browser-runtime'), '-e' + str(library / 'libchartr_browser.so'),
                '-e' + str(library / 'chartr-browser-helper'), '-e' + str(library / 'browser-runtime/libcef.so')], cwd=work, text=True)
            dependencies = deps.strip().removeprefix('shlibs:Depends=')
            (root / 'DEBIAN/control').write_text(f'Package: chartr-browser\nVersion: {version}\nArchitecture: amd64\nMaintainer: Chartr contributors\nDepends: {dependencies}, xdg-utils\nSection: utils\nPriority: optional\nDescription: Independently distributed embedded Browser plugin for Chartr\n')
            artifact = output / f'chartr-browser_{version}_amd64.deb'
            subprocess.run(['dpkg-deb', '--root-owner-group', '--build', root, artifact], check=True)
        else:
            archive = work / 'plugin.tar.gz'
            subprocess.run(['tar', '-czf', archive, '-C', package, '.'], check=True)
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            recipe = f'''pkgname=chartr-browser-bin
pkgver={version}
pkgrel=1
pkgdesc='Embedded Browser plugin for Chartr, with bundled Chromium'
arch=('x86_64')
url='https://github.com/rengwu/chartr-browser'
license=('GPL-3.0-or-later' 'BSD-3-Clause')
depends=('glibc' 'gcc-libs' 'glib2' 'nss' 'nspr' 'at-spi2-core' 'dbus' 'libcups' 'libx11' 'libxcomposite' 'libxdamage' 'libxext' 'libxfixes' 'libxrandr' 'mesa' 'expat' 'libxcb' 'libxkbcommon' 'cairo' 'pango' 'systemd-libs' 'alsa-lib' 'xdg-utils')
options=('!strip' '!debug')
source=('plugin.tar.gz')
sha256sums=('{digest}')
package() {{
    install -d "$pkgdir/usr/lib/chartr/plugins/com.chartr.browser"
    for file in chartr-plugin.toml LICENSE icons libchartr_browser.so chartr-browser-helper chrome-sandbox browser-runtime; do
        cp -a "$srcdir/$file" "$pkgdir/usr/lib/chartr/plugins/com.chartr.browser/"
    done
    chmod 4755 "$pkgdir/usr/lib/chartr/plugins/com.chartr.browser/chrome-sandbox"
}}
'''
            (work / 'PKGBUILD').write_text(recipe)
            subprocess.run(['makepkg', '--nodeps', '--noconfirm', '--nosign'], cwd=work, check=True)
            artifact, = work.glob('*.pkg.tar.zst')
            shutil.copy2(artifact, output / artifact.name)


if __name__ == '__main__':
    main()
