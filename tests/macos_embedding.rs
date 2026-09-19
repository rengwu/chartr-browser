//! Real independently linked plugin + ordinary host Wry, on the AppKit main thread.
//! Run explicitly after packaging; regular unit tests do not need a desktop.
#[cfg(not(target_os = "macos"))]
fn main() {}
#[cfg(target_os = "macos")]
fn main() {
    if let Some(package) = std::env::var_os("CHARTR_BROWSER_TEST_PACKAGE") {
        mac::run(std::path::Path::new(&package));
    }
}
#[cfg(target_os = "macos")]
mod mac {
    use chartr_native_plugin::{self as abi, Event, Input};
    use host_wry::{
        WebViewBuilder,
        raw_window_handle::{AppKitWindowHandle, WindowHandle},
    };
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};
    use std::{
        ffi::{CString, c_void},
        io::{Read, Write},
        path::Path,
        ptr::NonNull,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    unsafe extern "C" fn emit(context: *mut c_void, bytes: abi::Bytes) {
        let queue = unsafe { &*context.cast::<Mutex<Vec<Event>>>() };
        let bytes = unsafe { std::slice::from_raw_parts(bytes.data, bytes.len) };
        queue
            .lock()
            .unwrap()
            .push(serde_json::from_slice(bytes).unwrap());
    }
    fn send(api: &abi::Api, pane: *mut c_void, input: Input) {
        let bytes = serde_json::to_vec(&input).unwrap();
        unsafe {
            (api.dispatch)(pane, abi::Bytes::new(&bytes));
        }
    }
    fn pump_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(Instant::now() < deadline, "Embedded macOS view timed out");
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
        }
    }
    pub fn run(package: &Path) {
        let mtm = MainThreadMarker::new().expect("AppKit main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0., 0.), NSSize::new(800., 600.)),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.makeKeyAndOrderFront(None);
        let view = window.contentView().unwrap();
        let parent = NonNull::from(&*view).cast::<c_void>();
        let raw = unsafe { WindowHandle::borrow_raw(AppKitWindowHandle::new(parent).into()) };
        let messages = Arc::new(Mutex::new(Vec::new()));
        let host_messages = messages.clone();
        let host = WebViewBuilder::new()
            .with_ipc_handler(move |r| host_messages.lock().unwrap().push(r.body().clone()))
            .with_html(
                "<title>Host view</title><script>window.ipc.postMessage('host-ready')</script>",
            )
            .build_as_child(&raw)
            .unwrap();
        pump_until(|| messages.lock().unwrap().iter().any(|v| v == "host-ready"));

        // The library is loaded only after ordinary Wry has registered its ObjC classes.
        let library =
            unsafe { libloading::Library::new(package.join("libchartr_browser.dylib")).unwrap() };
        let api = unsafe {
            let entry = library
                .get::<unsafe extern "C" fn() -> *const abi::Api>(abi::ENTRY_POINT)
                .unwrap();
            *entry()
        };
        assert_eq!(api.abi, abi::ABI_VERSION);
        let directory = tempfile::tempdir().unwrap();
        let package_dir = CString::new(package.to_str().unwrap()).unwrap();
        let data_dir = CString::new(directory.path().to_str().unwrap()).unwrap();
        let instance = serde_json::to_vec(&abi::Instance {
            space: "smoke".into(),
            instance_id: 1,
            theme: abi::Theme {
                page: "#202020".into(),
                field: "#303030".into(),
                border: "#555555".into(),
                focus: "#4488ff".into(),
                text: "#ffffff".into(),
                muted: "#aaaaaa".into(),
                ui_font_size: "14px".into(),
            },
        })
        .unwrap();
        let queue = Box::new(Mutex::new(Vec::<Event>::new()));
        let options = abi::Create {
            abi: abi::ABI_VERSION,
            size: std::mem::size_of::<abi::Create>() as u32,
            parent_kind: abi::PARENT_NS_VIEW,
            parent: parent.as_ptr() as usize,
            package_dir: package_dir.as_ptr(),
            data_dir: data_dir.as_ptr(),
            instance: abi::Bytes::new(&instance),
            host: abi::Host {
                context: (&*queue as *const Mutex<Vec<Event>>).cast_mut().cast(),
                emit,
            },
        };
        let pane = unsafe { (api.create)(&options) };
        assert!(
            !pane.is_null(),
            "Plugin failed: {:?}",
            queue.lock().unwrap()
        );
        unsafe {
            (api.resize)(pane, 0., 0., 400., 300., 2.);
            (api.visible)(pane, true);
            (api.focus)(pane, true);
        }
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", server.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in server.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer);
                let body = "<title>Plugin view</title><script>window.ipc.postMessage(JSON.stringify({action:'focus-address'}))</script>";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        send(
            &api,
            pane,
            Input::Action {
                id: "navigate".into(),
                value: Some(url),
            },
        );
        let mut title = false;
        let mut ipc = false;
        pump_until(|| {
            let events = std::mem::take(&mut *queue.lock().unwrap());
            for event in events {
                match event {
                    Event::Wake => send(&api, pane, Input::Poll),
                    Event::State { title: value, .. } => title |= value == "Plugin view",
                    Event::Focus { control, .. } => ipc |= control.as_deref() == Some("location"),
                    Event::Error { message } => panic!("{message}"),
                }
            }
            title && ipc
        });
        unsafe {
            (api.focus)(pane, false);
            (api.visible)(pane, false);
            (api.destroy)(pane);
            (api.shutdown)();
        }
        queue.lock().unwrap().clear();
        host.evaluate_script("window.ipc.postMessage('host-after-plugin')")
            .unwrap();
        pump_until(|| {
            messages
                .lock()
                .unwrap()
                .iter()
                .any(|v| v == "host-after-plugin")
        });
        assert!(queue.lock().unwrap().is_empty(), "Callback after destroy");
        // Native libraries containing Objective-C classes remain pinned until exit.
        std::mem::forget(library);
        drop(host);
        window.close();
        println!("macOS: host/plugin loading, IPC isolation, sizing, focus and teardown passed");
    }
}
