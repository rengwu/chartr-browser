# Browser acceptance

Install the prebuilt `com.chartr.browser` plugin and exercise it in standalone,
grouped, and split panes on macOS and Linux/X11. Confirm it is absent from a
fresh Chartr profile until installed and has its packaged icon. Verify the
themed toolbar, URL/search interpretation, redirects, Back/Forward, Stop/Reload,
keyboard shortcuts, one page per pane, current-pane handling of new-window
links, native file uploads, system-browser download handoff, denied site
permissions, ephemeral web-engine storage, and per-pane last-URL restoration.
On Linux, verify bundled Chromium without system GStreamer codecs: play a
VP9/Opus clip and an ordinary YouTube video, switch to Wayfinder and back, and
resize the pane. Terminate only the test page renderer and verify the error
page's Retry action recovers without closing Chartr. Check Ctrl+L from a focused
page input with the pointer left over that input: typing, selection, Backspace
and Enter must operate on the address bar without changing the page input.
Click the page again and verify typing returns there. Repeat with the normal
XIM input method enabled, after switching tabs, and from the packaged tree.
Repeat the playback check from the packaged installation tree.
Confirm deb/Arch packages install the standard root-owned sandbox helper with
mode 4755; source/tarball builds use user namespaces. macOS retains WKWebView
and may retain its native network/TLS error page. DRM and proprietary codecs are
not required for this migration.

Open two Browser panes and verify one `--serve` helper owns both on Linux.
Close one: the other must continue playing. Close the last: the helper and its
Chromium children must exit. Reopen: a fresh helper must work without restarting
Chartr. Confirm Chartr's mapped libraries contain no `libcef.so`.

Automated package checks: `python3 tests/linux_embedding.py dist/package` on
Linux (or under Xvfb); `CHARTR_BROWSER_TEST_PACKAGE="$PWD/dist/package" cargo test
--test macos_embedding --locked` on macOS. The latter loads the independent
library after an ordinary host Wry view and verifies both IPC paths and teardown.
