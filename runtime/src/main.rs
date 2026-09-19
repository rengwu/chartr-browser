#[cfg(target_os = "linux")]
#[path = "../../src/wire.rs"]
mod wire;
#[cfg(target_os = "linux")]
mod server {
    use super::wire::{Command, EngineEvent, MAX_FRAME, Reply, Request};
    use anyhow::{Context, Result, bail};
    use chartr_browser_runtime::{Event, WebView};
    use std::{
        collections::HashMap,
        io::{BufRead, Read, Write},
        sync::{Arc, Mutex},
    };
    type Output = Arc<Mutex<std::io::Stdout>>;
    fn emit(output: &Output, reply: Reply) {
        if let Ok(mut bytes) = serde_json::to_vec(&reply) {
            if bytes.len() as u64 >= MAX_FRAME {
                return;
            }
            bytes.push(b'\n');
            let mut output = output.lock().unwrap();
            let _ = output.write_all(&bytes);
            let _ = output.flush();
        }
    }
    pub fn run() -> Result<()> {
        if let Some(code) = chartr_browser_runtime::run_subprocess() {
            std::process::exit(code);
        }
        if !std::env::args().any(|a| a == "--serve") {
            bail!("This is Browser's private engine helper");
        }
        let package = std::env::current_exe()?
            .parent()
            .context("Missing package directory")?
            .to_owned();
        chartr_browser_runtime::configure(package)?;
        let output = Arc::new(Mutex::new(std::io::stdout()));
        let mut panes = HashMap::<u64, WebView>::new();
        let mut input = std::io::stdin().lock();
        loop {
            let mut bytes = Vec::new();
            let size = input
                .by_ref()
                .take(MAX_FRAME + 1)
                .read_until(b'\n', &mut bytes)?;
            if size == 0 {
                break;
            }
            if size as u64 > MAX_FRAME {
                bail!("Oversized Browser control message");
            }
            let request: Request = serde_json::from_slice(&bytes)?;
            let shutdown = matches!(request.command, Command::Shutdown);
            let result = execute(&mut panes, request.pane, request.command, &output);
            emit(
                &output,
                Reply::Done {
                    sequence: request.sequence,
                    error: result.err().map(|e| format!("{e:#}")),
                },
            );
            if shutdown {
                break;
            }
        }
        for (_, pane) in panes.drain() {
            pane.close();
        }
        chartr_browser_runtime::shutdown_runtime();
        Ok(())
    }
    fn execute(
        panes: &mut HashMap<u64, WebView>,
        id: u64,
        command: Command,
        output: &Output,
    ) -> Result<()> {
        match command {
            Command::Create {
                parent,
                script,
                url,
                html,
            } => {
                if panes.contains_key(&id) {
                    bail!("Duplicate pane");
                }
                let output = output.clone();
                let url = url.unwrap_or_else(|| chartr_browser_runtime::html_url(&html));
                let pane = WebView::new(parent, &script, &url, move |event| {
                    let event = match event {
                        Event::Message(v) => EngineEvent::Message(v),
                        Event::Title(v) => EngineEvent::Title(v),
                        Event::Started(v) => EngineEvent::Started(v),
                        Event::Finished(v) => EngineEvent::Finished(v),
                        Event::Failed(v) => EngineEvent::Failed(v),
                        Event::Download(v) => EngineEvent::Download(v),
                        Event::History {
                            loading,
                            back,
                            forward,
                        } => EngineEvent::History {
                            loading,
                            back,
                            forward,
                        },
                    };
                    emit(&output, Reply::Event { pane: id, event });
                })?;
                panes.insert(id, pane);
                return Ok(());
            }
            Command::Destroy => {
                if let Some(pane) = panes.remove(&id) {
                    pane.close();
                }
                return Ok(());
            }
            Command::Shutdown => {
                for (_, pane) in panes.drain() {
                    pane.close();
                }
                return Ok(());
            }
            _ => {}
        }
        let pane = panes.get(&id).context("Pane is closed")?;
        match command {
            Command::Resize {
                x,
                y,
                width,
                height,
            } => pane.set_bounds(x, y, width, height)?,
            Command::Visible(value) => pane.set_visible(value)?,
            Command::Focus(true) => pane.focus()?,
            Command::Focus(false) => pane.focus_parent()?,
            Command::Url(value) => pane.load_url(&value)?,
            Command::Html(value) => pane.load_html(&value)?,
            Command::Script(value) => pane.evaluate_script(&value)?,
            Command::Back => pane.go_back()?,
            Command::Forward => pane.go_forward()?,
            Command::Reload => pane.reload()?,
            _ => {}
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn main() {
    if let Err(error) = server::run() {
        eprintln!("Browser engine: {error:#}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {}
