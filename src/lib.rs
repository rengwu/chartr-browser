//! Browser's independent native plugin. The only host contract is ABI v1.
mod engine;
#[cfg(target_os = "linux")]
mod linux;
mod pages;
mod wire;

use anyhow::{Context as _, Result, bail};
use chartr_native_plugin::{self as abi, Control, Event, Input, Shortcut};
use pages::*;
use std::{
    collections::VecDeque,
    ffi::{CStr, c_void},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use wire::EngineEvent;

/// Callback ownership is synchronized with destroy. Queued CEF callbacks can
/// outlive a view, but can neither touch the host nor enqueue work after close.
struct Events {
    host: Mutex<Option<(usize, unsafe extern "C" fn(*mut c_void, abi::Bytes))>>,
    queue: Mutex<VecDeque<EngineEvent>>,
}
impl Events {
    fn emit(&self, event: Event) {
        let Ok(bytes) = serde_json::to_vec(&event) else {
            return;
        };
        let host = self.host.lock().unwrap();
        if let Some((context, callback)) = *host {
            // SAFETY: host promises a live callback until destroy; holding this
            // lock makes close wait for any callback already in progress.
            unsafe {
                callback(context as *mut c_void, abi::Bytes::new(&bytes));
            }
        }
    }
    fn enqueue(&self, event: EngineEvent) {
        // Queue before waking; the receiver always observes the pending event.
        if self.host.lock().unwrap().is_none() {
            return;
        }
        self.queue.lock().unwrap().push_back(event);
        self.emit(Event::Wake);
    }
    fn close(&self) {
        self.host.lock().unwrap().take();
    }
}

struct Browser {
    view: Option<engine::WebView>,
    events: Arc<Events>,
    title: String,
    current_url: Option<String>,
    last_attempt: Option<String>,
    loading: bool,
    showing_local: bool,
    state_path: PathBuf,
    theme: abi::Theme,
}
impl Browser {
    fn new(options: &abi::Create) -> Result<Self> {
        if options.abi != abi::ABI_VERSION
            || options.size < std::mem::size_of::<abi::Create>() as u32
        {
            bail!("Unsupported native plugin interface");
        }
        // SAFETY: all arguments are valid borrowed strings/bytes per ABI v1.
        let (package, data, instance) = unsafe {
            (
                PathBuf::from(CStr::from_ptr(options.package_dir).to_str()?),
                PathBuf::from(CStr::from_ptr(options.data_dir).to_str()?),
                serde_json::from_slice::<abi::Instance>(bytes(options.instance)?)?,
            )
        };
        #[cfg(target_os = "linux")]
        linux::configure(package)?;
        #[cfg(not(target_os = "linux"))]
        let _ = package;
        let events = Arc::new(Events {
            host: Mutex::new(Some((options.host.context as usize, options.host.emit))),
            queue: Mutex::new(VecDeque::new()),
        });
        let state_path = state_path(&data, &instance.space, instance.instance_id);
        let saved = read_saved_url(&state_path);
        let callback = events.clone();
        let view = match engine::build(
            options.parent_kind,
            options.parent,
            saved.as_deref(),
            &local_page(&instance.theme, LocalPage::Start),
            Arc::new(move |event| callback.enqueue(event)),
        ) {
            Ok(view) => view,
            Err(error) => {
                events.close();
                return Err(error);
            }
        };
        let browser = Self {
            view: Some(view),
            events,
            title: "Browser".into(),
            current_url: saved.clone(),
            last_attempt: saved.clone(),
            loading: saved.is_some(),
            showing_local: saved.is_none(),
            state_path,
            theme: instance.theme,
        };
        browser.publish();
        Ok(browser)
    }
    fn address(&self) -> &str {
        self.current_url
            .as_deref()
            .or(self.last_attempt.as_deref())
            .unwrap_or("")
    }
    fn publish(&self) {
        let view = self.view.as_ref().unwrap();
        let button = |id: &str, label: &str, icon: &str, enabled| Control {
            id: id.into(),
            label: label.into(),
            icon: Some(format!("icons/{icon}.svg")),
            enabled,
            value: None,
        };
        let controls = vec![
            button(
                "back",
                "Back",
                "arrow_left",
                view.can_go_back().unwrap_or(false),
            ),
            button(
                "forward",
                "Forward",
                "arrow_right",
                view.can_go_forward().unwrap_or(false),
            ),
            if self.loading {
                button("stop", "Stop", "stop", true)
            } else {
                button("reload", "Reload", "rotate_cw", true)
            },
            Control {
                id: "location".into(),
                label: "Enter a URL or search".into(),
                icon: Some(format!(
                    "icons/{}.svg",
                    if self.address().starts_with("https://") {
                        "lock"
                    } else {
                        "public"
                    }
                )),
                enabled: true,
                value: Some(self.address().into()),
            },
        ];
        #[cfg(target_os = "macos")]
        let keys = [
            ("cmd-l", "focus-address"),
            ("cmd-r", "reload"),
            ("cmd-[", "back"),
            ("cmd-]", "forward"),
            ("escape", "stop"),
            ("cmd-a", "select-content"),
        ];
        #[cfg(not(target_os = "macos"))]
        let keys = [
            ("ctrl-l", "focus-address"),
            ("ctrl-r", "reload"),
            ("alt-left", "back"),
            ("alt-right", "forward"),
            ("escape", "stop"),
        ];
        let shortcuts = keys
            .into_iter()
            .map(|(key, action)| Shortcut {
                key: key.into(),
                action: action.into(),
                content_only: action == "select-content",
            })
            .collect();
        self.events.emit(Event::State {
            title: self.title.clone(),
            controls,
            shortcuts,
        });
    }
    fn error(&mut self, message: String) {
        self.loading = false;
        self.showing_local = true;
        self.current_url = None;
        if self
            .view
            .as_ref()
            .unwrap()
            .load_html(&local_page(
                &self.theme,
                LocalPage::Error {
                    message: &message,
                    retry: self.last_attempt.as_deref().unwrap_or(""),
                },
            ))
            .is_err()
        {
            self.events.emit(Event::Error { message });
        }
    }
    fn action(&mut self, action: &str, value: Option<&str>) {
        if action == "reload" && self.showing_local {
            let retry = self.last_attempt.clone().unwrap_or_default();
            self.action("navigate", Some(&retry));
            return;
        }
        let view = self.view.as_ref().unwrap();
        let result: Result<()> = (|| {
            match action {
                "location" | "navigate" => {
                    let input = value.unwrap_or("");
                    self.last_attempt = Some(input.into());
                    let url = resolve_input(input).map_err(anyhow::Error::msg)?;
                    view.load_url(url.as_str())?;
                    self.current_url = Some(url.to_string());
                    self.loading = true;
                    self.showing_local = false;
                    view.focus()?;
                }
                "back" => {
                    view.go_back()?;
                }
                "forward" => {
                    view.go_forward()?;
                }
                "reload" => {
                    view.reload()?;
                }
                "stop" => {
                    view.evaluate_script("window.stop()")?;
                    self.loading = false;
                }
                "focus-address" => self.events.emit(Event::Focus {
                    control: Some("location".into()),
                    select_all: true,
                }),
                "focus" => self.events.emit(Event::Focus {
                    control: None,
                    select_all: false,
                }),
                "select-content" => {
                    view.evaluate_script(SELECT_PAGE_CONTENT)?;
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.error(error.to_string());
        }
    }
    fn poll(&mut self) {
        let pending: Vec<_> = self.events.queue.lock().unwrap().drain(..).collect();
        if pending.is_empty() {
            return;
        }
        for event in pending {
            match event {
                EngineEvent::Message(body) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                        self.action(
                            value["action"].as_str().unwrap_or(""),
                            value["value"].as_str(),
                        );
                    }
                }
                EngineEvent::Title(title) => self.title = display_title(&title),
                EngineEvent::Started(url) if is_http_url(&url) => {
                    self.current_url = Some(url.clone());
                    self.last_attempt = Some(url);
                    self.loading = true;
                    self.showing_local = false;
                }
                EngineEvent::Finished(url) if is_http_url(&url) => {
                    self.current_url = Some(url.clone());
                    self.last_attempt = Some(url.clone());
                    self.loading = false;
                    self.showing_local = false;
                    let _ = save_url(&self.state_path, &url);
                }
                EngineEvent::History { loading, .. } => {
                    self.loading = loading && !self.showing_local;
                    if !loading && let Some(url) = &self.current_url {
                        let _ = save_url(&self.state_path, url);
                    }
                }
                EngineEvent::Failed(error) => self.error(error),
                EngineEvent::Download(url) if is_http_url(&url) => {
                    let command = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(command).arg(url).spawn();
                }
                _ => {}
            }
        }
        self.publish();
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.events.close();
        if let Some(view) = self.view.take() {
            let _ = view.focus_parent();
            let _ = view.set_visible(false);
            #[cfg(target_os = "macos")]
            {
                let _ = view.evaluate_script("window.stop();document.querySelectorAll('audio,video').forEach(m=>{m.pause();m.removeAttribute('src');m.load()});");
                let _ = view.load_html("<!doctype html><title>Closed</title>");
            }
            #[cfg(target_os = "linux")]
            view.close();
            drop(view);
        }
    }
}

unsafe fn bytes<'a>(value: abi::Bytes) -> Result<&'a [u8]> {
    if value.len > abi::MAX_MESSAGE_BYTES || (value.len > 0 && value.data.is_null()) {
        bail!("Invalid native plugin message");
    }
    if value.len == 0 {
        return Ok(&[]);
    }
    Ok(unsafe { std::slice::from_raw_parts(value.data, value.len) })
}
fn guarded(handle: *mut c_void, f: impl FnOnce(&mut Browser) -> Result<()>) {
    if handle.is_null() {
        return;
    }
    // SAFETY: host returns the unique live handle, on the creation thread.
    let browser = unsafe { &mut *handle.cast::<Browser>() };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(browser))) {
        Ok(Ok(())) => {}
        result => browser.events.emit(Event::Error {
            message: match result {
                Ok(Err(e)) => format!("{e:#}"),
                _ => "Browser plugin failed".into(),
            },
        }),
    }
}
unsafe extern "C" fn create(options: *const abi::Create) -> *mut c_void {
    if options.is_null() {
        return std::ptr::null_mut();
    }
    let options = unsafe { &*options };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Browser::new(options))) {
        Ok(Ok(browser)) => Box::into_raw(Box::new(browser)).cast(),
        result => {
            let message = match result {
                Ok(Err(e)) => format!("{e:#}"),
                _ => "Browser initialization failed".into(),
            };
            if let Ok(bytes) = serde_json::to_vec(&Event::Error { message }) {
                unsafe {
                    (options.host.emit)(options.host.context, abi::Bytes::new(&bytes));
                }
            }
            std::ptr::null_mut()
        }
    }
}
unsafe extern "C" fn destroy(handle: *mut c_void) {
    if !handle.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(handle.cast::<Browser>()));
        }));
    }
}
unsafe extern "C" fn resize(handle: *mut c_void, x: f64, y: f64, w: f64, h: f64, scale: f64) {
    guarded(handle, |b| {
        engine::resize(b.view.as_ref().unwrap(), x, y, w, h, scale)
    });
}
unsafe extern "C" fn visible(handle: *mut c_void, value: bool) {
    guarded(handle, |b| {
        b.view.as_ref().unwrap().set_visible(value)?;
        Ok(())
    });
}
unsafe extern "C" fn focus(handle: *mut c_void, value: bool) {
    guarded(handle, |b| {
        let view = b.view.as_ref().unwrap();
        if value {
            view.focus()?;
        } else {
            view.focus_parent()?;
        }
        Ok(())
    });
}
unsafe extern "C" fn dispatch(handle: *mut c_void, input: abi::Bytes) {
    guarded(handle, |b| {
        match serde_json::from_slice(unsafe { bytes(input)? }).context("Invalid host event")? {
            Input::Poll => b.poll(),
            Input::Action { id, value } => {
                b.action(&id, value.as_deref());
                b.publish();
            }
            Input::Theme { theme } => {
                if b.showing_local {
                    b.view.as_ref().unwrap().evaluate_script(&format!(
                        "window.setTheme({})",
                        serde_json::to_string(&theme)?
                    ))?;
                }
                b.theme = theme;
            }
        }
        Ok(())
    });
}
unsafe extern "C" fn shutdown() {
    #[cfg(target_os = "linux")]
    {
        let _ = std::panic::catch_unwind(linux::shutdown);
    }
}
static API: abi::Api = abi::Api {
    abi: abi::ABI_VERSION,
    size: std::mem::size_of::<abi::Api>() as u32,
    create,
    destroy,
    resize,
    visible,
    focus,
    dispatch,
    shutdown,
};
#[unsafe(no_mangle)]
pub extern "C" fn chartr_native_plugin_v1() -> *const abi::Api {
    &API
}
