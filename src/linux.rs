//! Lightweight proxy for the shared plugin-owned engine process.
use crate::wire::{Command, EngineEvent, MAX_FRAME, Reply, Request};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as Process, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

type Callback = Arc<dyn Fn(EngineEvent) + Send + Sync>;
type Listener = (Callback, Arc<AtomicU8>);
static CLIENT: LazyLock<Mutex<Option<Arc<Client>>>> = LazyLock::new(|| Mutex::new(None));
static PACKAGE: Mutex<Option<PathBuf>> = Mutex::new(None);
static NEXT_PANE: AtomicU64 = AtomicU64::new(1);

pub fn configure(package: PathBuf) -> Result<()> {
    let mut slot = PACKAGE.lock().unwrap();
    if slot.as_ref().is_some_and(|old| old != &package) {
        bail!("Browser is already loaded from another package");
    }
    *slot = Some(package);
    Ok(())
}
struct Client {
    input: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    listeners: Mutex<HashMap<u64, Listener>>,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Result<(), String>>>>,
    sequence: AtomicU64,
    alive: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Client {
    fn get() -> Result<Arc<Self>> {
        let mut slot = CLIENT.lock().unwrap();
        if let Some(client) = slot.as_ref().filter(|c| c.alive.load(Ordering::Acquire)) {
            return Ok(client.clone());
        }
        if let Some(old) = slot.take() {
            old.shutdown();
        }
        let package = PACKAGE
            .lock()
            .unwrap()
            .clone()
            .context("Browser package is not configured")?;
        let mut command = Process::new(package.join("chartr-browser-helper"));
        command
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // Child-only environment: system packages supply the standard helper.
        // User installs can reuse it without changing the user's environment.
        for helper in [
            package.join("chrome-sandbox"),
            PathBuf::from("/usr/lib/chartr/plugins/com.chartr.browser/chrome-sandbox"),
        ] {
            if trusted_sandbox(&helper) {
                command.env("CHROME_DEVEL_SANDBOX", helper);
                break;
            }
        }
        let mut child = command
            .spawn()
            .context("Starting the packaged Browser engine")?;
        let output = child.stdout.take().unwrap();
        let client = Arc::new(Self {
            input: Mutex::new(child.stdin.take()),
            child: Mutex::new(child),
            listeners: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            worker: Mutex::new(None),
        });
        let weak = Arc::downgrade(&client);
        let worker = std::thread::Builder::new()
            .name("browser-events".into())
            .spawn(move || {
                let mut output = BufReader::new(output);
                loop {
                    let mut frame = Vec::new();
                    let Ok(size) = output
                        .by_ref()
                        .take(MAX_FRAME + 1)
                        .read_until(b'\n', &mut frame)
                    else {
                        break;
                    };
                    if size == 0 || size as u64 > MAX_FRAME {
                        break;
                    }
                    let Some(client) = weak.upgrade() else {
                        break;
                    };
                    let Ok(reply) = serde_json::from_slice(&frame) else {
                        continue;
                    };
                    match reply {
                        Reply::Done { sequence, error } => {
                            if let Some(waiter) = client.pending.lock().unwrap().remove(&sequence) {
                                let _ = waiter.send(error.map_or(Ok(()), Err));
                            }
                        }
                        Reply::Event { pane, event } => {
                            let listener = client.listeners.lock().unwrap().get(&pane).cloned();
                            if let Some((callback, history)) = listener {
                                if let EngineEvent::History { back, forward, .. } = &event {
                                    history.store(
                                        u8::from(*back) | (u8::from(*forward) << 1),
                                        Ordering::Relaxed,
                                    );
                                }
                                callback(event);
                            }
                        }
                    }
                }
                if let Some(client) = weak.upgrade() {
                    client.alive.store(false, Ordering::Release);
                    for (_, waiter) in std::mem::take(&mut *client.pending.lock().unwrap()) {
                        let _ = waiter.send(Err("Browser engine stopped".into()));
                    }
                    let callbacks: Vec<_> = client
                        .listeners
                        .lock()
                        .unwrap()
                        .values()
                        .map(|(c, _)| c.clone())
                        .collect();
                    for callback in callbacks {
                        callback(EngineEvent::Failed(
                            "The Browser engine stopped. Close and reopen this pane to restart it."
                                .into(),
                        ));
                    }
                }
            })?;
        *client.worker.lock().unwrap() = Some(worker);
        *slot = Some(client.clone());
        Ok(client)
    }
    fn send(&self, pane: u64, command: Command, wait: bool) -> Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            bail!("Browser engine stopped; close and reopen this pane");
        }
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut bytes = serde_json::to_vec(&Request {
            sequence,
            pane,
            command,
        })?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_FRAME {
            bail!("Browser control message is too large");
        }
        let receiver = if wait {
            let (tx, rx) = mpsc::sync_channel(1);
            self.pending.lock().unwrap().insert(sequence, tx);
            Some(rx)
        } else {
            None
        };
        let result = (|| {
            self.input
                .lock()
                .unwrap()
                .as_mut()
                .context("Browser engine is closed")?
                .write_all(&bytes)?;
            if let Some(receiver) = receiver {
                receiver
                    .recv_timeout(Duration::from_secs(20))
                    .context("Browser engine did not respond")?
                    .map_err(anyhow::Error::msg)?;
            }
            Ok(())
        })();
        self.pending.lock().unwrap().remove(&sequence);
        result
    }
    fn shutdown_if_unused(self: &Arc<Self>) {
        if !self.listeners.lock().unwrap().is_empty() {
            return;
        }
        let client = {
            let mut slot = CLIENT.lock().unwrap();
            if slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, self)) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(client) = client {
            client.shutdown();
        }
    }
    fn shutdown(&self) {
        let _ = self.send(0, Command::Shutdown, true);
        self.input.lock().unwrap().take();
        let mut child = self.child.lock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                _ if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}
fn trusted_sandbox(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.uid() == 0 && m.mode() & 0o7777 == 0o4755)
}
pub fn shutdown() {
    if let Some(client) = CLIENT.lock().unwrap().take() {
        client.shutdown();
    }
}

pub struct WebView {
    client: Arc<Client>,
    pane: u64,
    history: Arc<AtomicU8>,
    closed: AtomicBool,
}
impl WebView {
    pub fn new(
        parent: u32,
        script: &str,
        url: Option<&str>,
        html: &str,
        events: Callback,
    ) -> Result<Self> {
        let client = Client::get()?;
        let pane = NEXT_PANE.fetch_add(1, Ordering::Relaxed);
        let history = Arc::new(AtomicU8::new(0));
        client
            .listeners
            .lock()
            .unwrap()
            .insert(pane, (events, history.clone()));
        if let Err(error) = client.send(
            pane,
            Command::Create {
                parent,
                script: script.into(),
                url: url.map(str::to_owned),
                html: html.into(),
            },
            true,
        ) {
            client.listeners.lock().unwrap().remove(&pane);
            client.shutdown_if_unused();
            return Err(error);
        }
        Ok(Self {
            client,
            pane,
            history,
            closed: AtomicBool::new(false),
        })
    }
    fn send(&self, command: Command) -> Result<()> {
        self.client.send(self.pane, command, false)
    }
    pub fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
        self.send(Command::Resize {
            x,
            y,
            width,
            height,
        })
    }
    pub fn set_visible(&self, v: bool) -> Result<()> {
        self.send(Command::Visible(v))
    }
    pub fn focus(&self) -> Result<()> {
        self.send(Command::Focus(true))
    }
    pub fn focus_parent(&self) -> Result<()> {
        self.send(Command::Focus(false))
    }
    pub fn load_url(&self, url: &str) -> Result<()> {
        self.send(Command::Url(url.into()))
    }
    pub fn load_html(&self, html: &str) -> Result<()> {
        self.send(Command::Html(html.into()))
    }
    pub fn evaluate_script(&self, script: &str) -> Result<()> {
        self.send(Command::Script(script.into()))
    }
    pub fn go_back(&self) -> Result<()> {
        self.send(Command::Back)
    }
    pub fn go_forward(&self) -> Result<()> {
        self.send(Command::Forward)
    }
    pub fn reload(&self) -> Result<()> {
        self.send(Command::Reload)
    }
    pub fn can_go_back(&self) -> Result<bool> {
        Ok(self.history.load(Ordering::Relaxed) & 1 != 0)
    }
    pub fn can_go_forward(&self) -> Result<bool> {
        Ok(self.history.load(Ordering::Relaxed) & 2 != 0)
    }
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.client.listeners.lock().unwrap().remove(&self.pane);
        let _ = self.client.send(self.pane, Command::Destroy, true);
        self.client.shutdown_if_unused();
    }
}
impl Drop for WebView {
    fn drop(&mut self) {
        self.close();
    }
}
