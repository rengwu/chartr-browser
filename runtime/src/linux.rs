use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use cef::*;
use x11rb::{
    connection::Connection,
    protocol::xproto::{ConfigureWindowAux, ConnectionExt, CreateWindowAux, WindowClass},
    rust_connection::RustConnection,
};

#[derive(Debug)]
pub enum Event {
    Message(String),
    Title(String),
    Started(String),
    Finished(String),
    Failed(String),
    Download(String),
    History {
        loading: bool,
        back: bool,
        forward: bool,
    },
}

type Events = Arc<dyn Fn(Event) + Send + Sync>;
static BROWSERS: LazyLock<Mutex<HashMap<i32, (u32, Browser)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();
thread_local! {
    static PROFILE: RefCell<Option<tempfile::TempDir>> = const { RefCell::new(None) };
}

/// Dispatch Chromium subprocess roles before starting the private control loop.
pub fn run_subprocess() -> Option<i32> {
    if !std::env::args_os().any(|arg| arg.as_encoded_bytes().starts_with(b"--type=")) {
        return None;
    }
    api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();
    let mut app = BrowserApp::new(Arc::new(Mutex::new(HashMap::new())));
    Some(execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    ))
}

static PACKAGE: OnceLock<PathBuf> = OnceLock::new();

pub fn configure(package: PathBuf) -> Result<()> {
    if let Some(existing) = PACKAGE.get() {
        if existing != &package {
            bail!("Browser runtime is already loaded from another package");
        }
    } else {
        let _ = PACKAGE.set(package);
    }
    Ok(())
}

fn runtime_directory() -> Result<PathBuf> {
    let package = PACKAGE.get().context("Browser package is not configured")?;
    for candidate in [package.join("browser-runtime"), package.clone()] {
        if candidate.join("icudtl.dat").is_file() && candidate.join("resources.pak").is_file() {
            return Ok(candidate);
        }
    }
    bail!("Bundled Chromium resources are missing; reinstall the Browser plugin")
}

fn check_sandbox(package: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let helper = std::env::var_os("CHROME_DEVEL_SANDBOX")
        .map(PathBuf::from)
        .unwrap_or_else(|| package.join("chrome-sandbox"));
    if helper
        .metadata()
        .is_ok_and(|m| m.is_file() && m.uid() == 0 && m.mode() & 0o7777 == 0o4755)
    {
        return Ok(());
    }
    // This runs once before CEF starts any threads, in the private helper.
    // Probe in a child so the helper's own namespaces remain untouched.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if pid == 0 {
        let result = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
        unsafe {
            libc::_exit(if result == 0 { 0 } else { 1 });
        }
    }
    let mut status = 0;
    if unsafe { libc::waitpid(pid, &mut status, 0) } == pid
        && libc::WIFEXITED(status)
        && libc::WEXITSTATUS(status) == 0
    {
        return Ok(());
    }
    bail!(
        "Chromium's sandbox is unavailable. Install the Browser system package, which supplies its standard sandbox helper; no system settings need to be changed."
    )
}

fn initialize_runtime() -> Result<()> {
    let result = INITIALIZED.get_or_init(|| {
        (|| -> Result<()> {
            check_sandbox(PACKAGE.get().context("Missing Browser package")?)?;
            let directory = runtime_directory()?;
            let profile = tempfile::Builder::new()
                .prefix("chartr-browser-")
                .tempdir()?;
            api_hash(sys::CEF_API_VERSION_LAST, 0);
            let args = args::Args::new();
            let settings = Settings {
                multi_threaded_message_loop: 1,
                command_line_args_disabled: 1,
                browser_subprocess_path: CefString::from(
                    PACKAGE
                        .get()
                        .unwrap()
                        .join("chartr-browser-helper")
                        .to_string_lossy()
                        .as_ref(),
                ),
                root_cache_path: CefString::from(profile.path().to_string_lossy().as_ref()),
                resources_dir_path: CefString::from(directory.to_string_lossy().as_ref()),
                locales_dir_path: CefString::from(
                    directory.join("locales").to_string_lossy().as_ref(),
                ),
                log_severity: LogSeverity::WARNING,
                log_file: CefString::from(
                    profile
                        .path()
                        .join("chromium.log")
                        .to_string_lossy()
                        .as_ref(),
                ),
                ..Default::default()
            };
            let mut app = BrowserApp::new(Arc::new(Mutex::new(HashMap::new())));
            if initialize(
                Some(args.as_main_args()),
                Some(&settings),
                Some(&mut app),
                std::ptr::null_mut(),
            ) != 1
            {
                bail!("Could not initialize bundled Chromium");
            }
            PROFILE.with(|slot| *slot.borrow_mut() = Some(profile));
            Ok(())
        })()
        .map_err(|error| format!("{error:#}"))
    });
    result
        .as_ref()
        .map_err(|message| anyhow!(message.clone()))
        .copied()
}

/// Close native panes before their X11 parent disappears. The CEF UI loop is
/// independent, so wait for OnBeforeClose while the desktop's window still exists.
fn close_browsers() -> bool {
    if !matches!(INITIALIZED.get(), Some(Ok(()))) {
        return true;
    }
    let _ = on_ui(move || {
        let browsers: Vec<_> = BROWSERS
            .lock()
            .unwrap()
            .values()
            .map(|(_, browser)| browser.clone())
            .collect();
        for browser in browsers {
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if BROWSERS.lock().unwrap().is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Shut down CEF on the initialization thread after all plugin panes close.
pub fn shutdown_runtime() {
    if !matches!(INITIALIZED.get(), Some(Ok(()))) {
        return;
    }
    if close_browsers() {
        shutdown();
        PROFILE.with(|slot| slot.borrow_mut().take());
    }
}

wrap_app! {
    struct BrowserApp { scripts: Arc<Mutex<HashMap<i32, String>>> }
    impl App {
        fn on_before_command_line_processing(&self, _process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                // Chartr's pane parent is X11, also under Wayland via XWayland.
                command_line.append_switch_with_value(Some(&CefString::from("ozone-platform")), Some(&CefString::from("x11")));
                // An embedded pane has no Chrome profile onboarding window.
                command_line.append_switch(Some(&CefString::from("no-first-run")));
                command_line.append_switch(Some(&CefString::from("no-default-browser-check")));
            }
        }
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(Renderer::new(self.scripts.clone()))
        }
    }
}

wrap_render_process_handler! {
    struct Renderer { scripts: Arc<Mutex<HashMap<i32, String>>> }
    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            if let (Some(browser), Some(info)) = (browser, extra_info) {
                self.scripts.lock().unwrap().insert(browser.identifier(), CefString::from(&info.string(Some(&CefString::from("script")))).to_string());
            }
        }
        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser { self.scripts.lock().unwrap().remove(&browser.identifier()); }
        }
        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(browser), Some(frame), Some(context)) = (browser, frame, context) else { return; };
            if frame.is_main() == 0 { return; }
            let Some(global) = context.global() else { return; };
            let Some(mut ipc) = v8_value_create_object(None, None) else { return; };
            let mut handler = MessageBridge::new(frame.clone());
            let Some(mut function) = v8_value_create_function(Some(&CefString::from("postMessage")), Some(&mut handler)) else { return; };
            ipc.set_value_bykey(Some(&CefString::from("postMessage")), Some(&mut function), sys::cef_v8_propertyattribute_t::V8_PROPERTY_ATTRIBUTE_READONLY.into());
            global.set_value_bykey(Some(&CefString::from("ipc")), Some(&mut ipc), sys::cef_v8_propertyattribute_t::V8_PROPERTY_ATTRIBUTE_READONLY.into());
            if let Some(script) = self.scripts.lock().unwrap().get(&browser.identifier()) {
                frame.execute_java_script(Some(&CefString::from(script.as_str())), None, 0);
            }
        }
    }
}

wrap_v8_handler! {
    struct MessageBridge { frame: Frame }
    impl V8Handler {
        fn execute(&self, _name: Option<&CefString>, _object: Option<&mut V8Value>, arguments: Option<&[Option<V8Value>]>, _retval: Option<&mut Option<V8Value>>, _exception: Option<&mut CefString>) -> i32 {
            let Some(Some(value)) = arguments.and_then(|args| args.first()) else { return 0; };
            if value.is_string() == 0 { return 0; }
            let text = CefString::from(&value.string_value()).to_string();
            if text.len() > 64 * 1024 { return 0; }
            let Some(mut message) = process_message_create(Some(&CefString::from("chartr-browser"))) else { return 0; };
            if let Some(args) = message.argument_list() { args.set_string(0, Some(&CefString::from(text.as_str()))); }
            self.frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
            1
        }
    }
}

wrap_task! {
    struct UiTask { function: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> }
    impl Task {
        fn execute(&self) {
            let function = self.function.lock().unwrap().take();
            if let Some(function) = function { function(); }
        }
    }
}

fn on_ui(function: impl FnOnce() + Send + 'static) -> Result<()> {
    let mut task = UiTask::new(Arc::new(Mutex::new(Some(Box::new(function)))));
    if post_task(ThreadId::UI, Some(&mut task)) != 1 {
        bail!("Chromium's UI thread is unavailable");
    }
    Ok(())
}

/// One native child pane. All Chromium callbacks run on CEF's own threads.
pub struct WebView {
    browser: Browser,
    child: u32,
    container: Arc<Container>,
}

// CEF expects the root visual; GPUI can use a 32-bit visual. Supply an explicit
// root-visual child with its own colormap instead of inheriting GPUI's visual.
struct Container {
    connection: RustConnection,
    window: u32,
    keyboard: crate::keyboard::KeyboardFocus,
}
impl Drop for Container {
    fn drop(&mut self) {
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.flush();
    }
}

impl WebView {
    pub fn new(
        parent: u32,
        script: &str,
        url: &str,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Self> {
        initialize_runtime()?;
        let (connection, screen) = x11rb::connect(None)?;
        let root = &connection.setup().roots[screen];
        let container = connection.generate_id()?;
        connection
            .create_window(
                root.root_depth,
                container,
                parent,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                root.root_visual,
                &CreateWindowAux::new()
                    .background_pixel(0)
                    .border_pixel(0)
                    .colormap(root.default_colormap),
            )?
            .check()?;
        connection.flush()?;
        let keyboard = crate::keyboard::KeyboardFocus::new(parent)?;
        let container = Arc::new(Container {
            connection,
            window: container,
            keyboard,
        });
        let native_parent = container.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let script = script.to_owned();
        let url = url.to_owned();
        let events: Events = Arc::new(events);
        on_ui(move || {
            let mut client = BrowserClient::new(events, native_parent.clone(), parent);
            let settings = BrowserSettings::default();
            let window = WindowInfo {
                runtime_style: RuntimeStyle::ALLOY,
                ..Default::default()
            }
            .set_as_child(
                native_parent.window.into(),
                &Rect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            );
            let mut info = dictionary_value_create();
            if let Some(info) = info.as_ref() {
                info.set_string(
                    Some(&CefString::from("script")),
                    Some(&CefString::from(script.as_str())),
                );
            }
            let mut context =
                request_context_create_context(Some(&RequestContextSettings::default()), None);
            let browser = browser_host_create_browser_sync(
                Some(&window),
                Some(&mut client),
                Some(&CefString::from(url.as_str())),
                Some(&settings),
                info.as_mut(),
                context.as_mut(),
            );
            // If the caller timed out, close the newly created browser too.
            if let Err(error) = tx.send(browser) {
                if let Some(browser) = error.0.and_then(|browser| browser.host()) {
                    browser.close_browser(1);
                }
            }
        })?;
        let browser = rx
            .recv_timeout(Duration::from_secs(15))
            .context("Timed out creating Chromium pane")?
            .context("Chromium could not create a pane")?;
        let child = browser
            .host()
            .context("Chromium pane has no window")?
            .window_handle()
            .try_into()?;
        Ok(Self {
            browser,
            child,
            container,
        })
    }

    /// Wait for this pane's native child to close without touching sibling panes.
    pub fn close(&self) {
        let browser = self.browser.clone();
        let id = browser.identifier();
        let _ = on_ui(move || {
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while BROWSERS.lock().unwrap().contains_key(&id) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
        self.container
            .connection
            .configure_window(
                self.container.window,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(width.max(1))
                    .height(height.max(1)),
            )?
            .check()?;
        self.container
            .connection
            .configure_window(
                self.child,
                &ConfigureWindowAux::new()
                    .x(0)
                    .y(0)
                    .width(width.max(1))
                    .height(height.max(1)),
            )?
            .check()?;
        self.container.connection.flush()?;
        Ok(())
    }
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        if visible {
            self.container
                .connection
                .map_window(self.container.window)?
                .check()?;
        } else {
            self.container
                .connection
                .unmap_window(self.container.window)?
                .check()?;
        }
        self.container.connection.flush()?;
        Ok(())
    }
    pub fn focus(&self) -> Result<()> {
        let browser = self.browser.clone();
        on_ui(move || {
            if let Some(host) = browser.host() {
                host.set_focus(1);
            }
        })
    }
    pub fn focus_parent(&self) -> Result<()> {
        let browser = self.browser.clone();
        let container = self.container.clone();
        on_ui(move || {
            // Chromium's focus manager and X11 focus are separate; relinquish both.
            if let Some(host) = browser.host() {
                host.set_focus(0);
            }
            let _ = container.keyboard.focus();
        })
    }
    pub fn load_url(&self, url: &str) -> Result<()> {
        let browser = self.browser.clone();
        let url = url.to_owned();
        on_ui(move || {
            if let Some(frame) = browser.main_frame() {
                frame.load_url(Some(&CefString::from(url.as_str())));
            }
        })
    }
    pub fn load_html(&self, html: &str) -> Result<()> {
        self.load_url(&html_url(html))
    }
    pub fn evaluate_script(&self, script: &str) -> Result<()> {
        let browser = self.browser.clone();
        let script = script.to_owned();
        on_ui(move || {
            if let Some(frame) = browser.main_frame() {
                frame.execute_java_script(Some(&CefString::from(script.as_str())), None, 0);
            }
        })
    }
    pub fn go_back(&self) -> Result<()> {
        let browser = self.browser.clone();
        on_ui(move || browser.go_back())
    }
    pub fn go_forward(&self) -> Result<()> {
        let browser = self.browser.clone();
        on_ui(move || browser.go_forward())
    }
    pub fn reload(&self) -> Result<()> {
        let browser = self.browser.clone();
        on_ui(move || browser.reload())
    }
    pub fn can_go_back(&self) -> Result<bool> {
        Ok(self.browser.can_go_back() != 0)
    }
    pub fn can_go_forward(&self) -> Result<bool> {
        Ok(self.browser.can_go_forward() != 0)
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        let _ = self.set_visible(false);
        let browser = self.browser.clone();
        let _ = on_ui(move || {
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
        });
    }
}

pub fn html_url(html: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(html.as_bytes());
    format!("data:text/html;charset=utf-8;base64,{encoded}")
}

fn text(value: Option<&CefString>) -> String {
    value.map(CefString::to_string).unwrap_or_default()
}

fn permitted_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        matches!(url.scheme(), "https" | "http" | "data" | "blob")
            || matches!(url.as_str(), "about:blank" | "about:srcdoc")
    })
}

wrap_client! {
    struct BrowserClient { events: Events, container: Arc<Container>, parent: u32 }
    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> { Some(Display::new(self.events.clone())) }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(LifeSpan::new(self.events.clone(), self.parent)) }
        fn load_handler(&self) -> Option<LoadHandler> { Some(Loading::new(self.events.clone())) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(Requests::new(self.events.clone())) }
        fn focus_handler(&self) -> Option<FocusHandler> { Some(Focus::new(self.events.clone())) }
        fn keyboard_handler(&self) -> Option<KeyboardHandler> { Some(Shortcuts::new(self.events.clone())) }
        fn download_handler(&self) -> Option<DownloadHandler> { Some(Downloads::new(self.events.clone())) }
        fn on_process_message_received(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, source_process: ProcessId, message: Option<&mut ProcessMessage>) -> i32 {
            if source_process != ProcessId::RENDERER || !frame.is_some_and(|frame| frame.is_main() != 0) { return 0; }
            let Some(message) = message else { return 0; };
            if CefString::from(&message.name()).to_string() != "chartr-browser" { return 0; }
            if let Some(args) = message.argument_list() {
                let value = CefString::from(&args.string(0)).to_string();
                if value.len() <= 64 * 1024 { (self.events)(Event::Message(value)); }
            }
            1
        }
    }
}

wrap_display_handler! {
    struct Display { events: Events }
    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) { (self.events)(Event::Title(text(title))); }
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|frame| frame.is_main() != 0) { (self.events)(Event::Started(text(url))); }
        }
    }
}

wrap_life_span_handler! {
    struct LifeSpan { events: Events, parent: u32 }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser { BROWSERS.lock().unwrap().insert(browser.identifier(), (self.parent, browser.clone())); }
        }
        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser { BROWSERS.lock().unwrap().remove(&browser.identifier()); }
        }
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32, target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, _target_disposition: WindowOpenDisposition, _user_gesture: i32, _popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 {
            let url = text(target_url);
            if permitted_url(&url) { (self.events)(Event::Message(serde_json::json!({"action":"navigate","value":url}).to_string())); }
            1
        }
    }
}

wrap_load_handler! {
    struct Loading { events: Events }
    impl LoadHandler {
        fn on_loading_state_change(&self, _browser: Option<&mut Browser>, loading: i32, back: i32, forward: i32) {
            (self.events)(Event::History { loading: loading != 0, back: back != 0, forward: forward != 0 });
        }
        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            if let Some(frame) = frame.filter(|frame| frame.is_main() != 0) { (self.events)(Event::Started(CefString::from(&frame.url()).to_string())); }
        }
        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            if let Some(frame) = frame.filter(|frame| frame.is_main() != 0) { (self.events)(Event::Finished(CefString::from(&frame.url()).to_string())); }
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error_code: Errorcode, error_text: Option<&CefString>, _failed_url: Option<&CefString>) {
            if frame.is_some_and(|frame| frame.is_main() != 0) && sys::cef_errorcode_t::from(error_code) != sys::cef_errorcode_t::ERR_ABORTED {
                (self.events)(Event::Failed(text(error_text)));
            }
        }
    }
}

wrap_request_handler! {
    struct Requests { events: Events }
    impl RequestHandler {
        fn on_before_browse(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _user_gesture: i32, _is_redirect: i32) -> i32 {
            request.is_some_and(|request| !permitted_url(&CefString::from(&request.url()).to_string())) as i32
        }
        fn on_open_urlfrom_tab(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, target_url: Option<&CefString>, _target_disposition: WindowOpenDisposition, _user_gesture: i32) -> i32 {
            let url = text(target_url);
            if permitted_url(&url) { (self.events)(Event::Message(serde_json::json!({"action":"navigate","value":url}).to_string())); }
            1
        }
        fn on_render_process_terminated(&self, _browser: Option<&mut Browser>, _status: TerminationStatus, _error_code: i32, _error_string: Option<&CefString>) {
            (self.events)(Event::Failed("This page's browser process stopped. Reload to try again.".into()));
        }
    }
}

// Browser shortcuts must work even when a site intercepts keys or its
// JavaScript is busy. Handle them before Chromium sends keys to the renderer.
wrap_keyboard_handler! {
    struct Shortcuts { events: Events }
    impl KeyboardHandler {
        fn on_pre_key_event(&self, _browser: Option<&mut Browser>, event: Option<&KeyEvent>, _os_event: Option<&mut sys::XEvent>, _is_keyboard_shortcut: Option<&mut i32>) -> i32 {
            let Some(event) = event.filter(|event| event.type_ == KeyEventType::RAWKEYDOWN) else { return 0; };
            use sys::cef_event_flags_t as Flags;
            let modifiers = event.modifiers & (Flags::EVENTFLAG_SHIFT_DOWN.0 | Flags::EVENTFLAG_CONTROL_DOWN.0 | Flags::EVENTFLAG_ALT_DOWN.0 | Flags::EVENTFLAG_COMMAND_DOWN.0 | Flags::EVENTFLAG_ALTGR_DOWN.0);
            let action = if modifiers == Flags::EVENTFLAG_CONTROL_DOWN.0 {
                match event.windows_key_code { 0x4c => "focus-address", 0x52 => "reload", _ => return 0 }
            } else if modifiers == Flags::EVENTFLAG_ALT_DOWN.0 {
                match event.windows_key_code { 0x25 => "back", 0x27 => "forward", _ => return 0 }
            } else if modifiers == 0 && event.windows_key_code == 0x1b {
                "stop"
            } else { return 0; };
            (self.events)(Event::Message(serde_json::json!({"action": action}).to_string()));
            1
        }
    }
}

wrap_focus_handler! {
    struct Focus { events: Events }
    impl FocusHandler {
        fn on_got_focus(&self, _browser: Option<&mut Browser>) { (self.events)(Event::Message("{\"action\":\"focus\"}".into())); }
    }
}

wrap_download_handler! {
    struct Downloads { events: Events }
    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            let url = text(url);
            if url::Url::parse(&url).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
                // Preserve Browser's existing handoff to the default browser.
                (self.events)(Event::Download(url));
            }
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_navigation_cannot_open_local_files_or_privileged_schemes() {
        for url in [
            "https://example.org",
            "http://localhost:1234/",
            "data:text/html,hello",
            "blob:https://example.org/id",
            "about:blank",
            "about:srcdoc",
        ] {
            assert!(permitted_url(url), "{url}");
        }
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "chrome://settings",
            "devtools://devtools",
            "about:config",
            "mailto:a@example.org",
            "not a URL",
        ] {
            assert!(!permitted_url(url), "{url}");
        }
    }

    #[test]
    fn local_pages_round_trip_without_interpreting_url_delimiters() {
        let html = "<!doctype html><p>雪 & # ? %</p>";
        let url = html_url(html);
        let encoded = url
            .strip_prefix("data:text/html;charset=utf-8;base64,")
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            html.as_bytes()
        );
    }
}
