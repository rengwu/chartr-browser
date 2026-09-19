# Private macOS Wry namespace

Source: crates.io wry 0.56.1 (Apache-2.0 OR MIT).

The sole code/build change is `[lib] name = "chartr_browser_wry"`.
objc2 derives Objective-C class names from the Rust module path and crate
version. Giving this library its own module namespace prevents its WKWebView
subclasses/delegates from merging with the independently built host Wry copy.
Each copy retains its own Rust state and callbacks. No host Wry objects cross
the plugin ABI. Examples/dev-only files are omitted from this snapshot.

Keep this namespace when updating Wry. Test a host Wry view and a plugin view
in the same macOS process before publishing a release.
