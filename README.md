# Chartr Browser

An independently distributed, embedded Browser plugin for [Chartr](https://github.com/rengwu/chartr).
It is not bundled with Chartr. Linux packages include pinned Chromium/CEF;
macOS uses the system WKWebView. The plugin has no GPUI dependency.

On Linux, one plugin-owned engine process serves all Browser panes. Each page
embeds directly as a native child window; no pixels are copied over IPC and no
second UI framework is loaded. The helper starts with the first Browser pane
and exits when the last closes. Chartr never links or preloads Chromium.

## Install

Requires a Chartr build with native surface ABI v1 (`kind = "embedded"`).
In **Settings → Plugins → Install from Git**, enter:

```
https://github.com/rengwu/chartr-browser
```

Chartr downloads the prebuilt package for your platform, verifies its checksum,
and presents the native-code trust prompt. Confirm, restart, and open **Browser**
from **New surface**. Updating uses the same installation flow and retains each
pane's saved URL.

**Installation never compiles source, installs build tools, or runs a package
build script.** If your platform has no release asset, installation reports that
no build is available. You can also extract a release archive and install that
prepared directory through **Install from folder**.

Linux supports x86_64 on Ubuntu 24.04/current Arch with X11 or XWayland. Downloads
include Chromium and open-media codecs; no system WebKit or GStreamer media
plugins are used by Browser. DRM and proprietary H.264/AAC codecs are not
included. Some sites requiring those technologies will remain unsupported.

Chromium's sandbox stays enabled. User-installed archives require unprivileged
user namespaces. System `.deb` and Arch packages install Chromium's standard
root-owned sandbox helper with mode 4755; they do not change kernel settings.

## Behavior

Each Chartr pane owns one page, with Back, Forward, Stop/Reload and an address
field. Enter a URL, localhost address or search query. Ctrl+L (Cmd+L on macOS)
selects the address field; Enter navigates. The toolbar uses Chartr's existing
native controls, including text composition and selection.

Page titles update the tab. New-window links open in the same pane. File uploads
use the native picker, downloads hand off to the default browser, and site
permission requests are denied. Browser storage is ephemeral; only each pane's
last URL is saved. Renderer crashes show a retry page. Closing a pane stops its
media and releases its native view.

## Build and release (plugin authors only)

End users do not run these steps. Install Rust, a C/C++ toolchain, CMake and Ninja
on Linux, or Xcode command line tools on macOS. Then:

```sh
cargo build --workspace --release --locked
bash scripts/package.sh target/release dist
```

There is no JavaScript build, GPUI checkout or Chartr compilation. The Linux
build downloads the pinned prebuilt CEF SDK; `.cargo/config.toml` keeps one SDK
cache shared by development, test and release builds. It builds CEF's API glue,
not Chromium itself. Release archives contain only binaries, resources, icons,
licenses and the plugin manifest. GitHub Actions builds Linux x86_64 and both
macOS architectures, then publishes checked archives and Linux system packages.

`runtime/` owns the Linux engine, sandbox and keyboard routing. `src/` owns the
plugin lifecycle, toolbar description, navigation and macOS engine. `sdk/` is a
vendored copy of Chartr's small ABI-v1 crate, permitting independent builds
without fetching Chartr or its GPUI dependency graph. On macOS, `vendor/wry`
uses a private crate name so its Objective-C classes cannot collide with the
host's own Wry views; see [the patch note](vendor/wry/CHARTR-PATCH.md).

Run `cargo test --workspace --locked`. With an X11 display, also run
`cargo test -p chartr-browser-runtime --locked -- --include-ignored` for the
native keyboard-routing regression. Follow [ACCEPTANCE.md](ACCEPTANCE.md) for
real embedded playback, input, sizing, crash recovery and close checks.

## Engine maintenance

CEF is pinned in `runtime/Cargo.toml` and `Cargo.lock`. Track Chromium security
updates and publish a new plugin release when moving that pin. Chartr updates
do not update this independently installed engine. Recheck real media playback,
keyboard routing and packaged runtime loading for each release.

Source is GPL-3.0-or-later. See [LICENSE](LICENSE), [CEF's license](LICENSE-CEF.txt),
packaged Chromium `CREDITS.html`, and the icon attribution in `icons/`.
