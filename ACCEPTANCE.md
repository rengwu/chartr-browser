# Browser acceptance

Install the prebuilt `com.chartr.browser` plugin and open it and exercise it in standalone,
grouped, and split panes on macOS and Linux/X11. Confirm it is absent from a fresh Chartr profile until installed and has its packaged icon. Verify the
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
