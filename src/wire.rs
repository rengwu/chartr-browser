//! Private, bounded control messages between the Linux plugin and its one helper.
//! Pixels and input events go directly through native windows, never this pipe.
use serde::{Deserialize, Serialize};
pub const MAX_FRAME: u64 = 1024 * 1024;
#[derive(Debug, Serialize, Deserialize)]
pub enum EngineEvent {
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
#[derive(Serialize, Deserialize)]
pub struct Request {
    pub sequence: u64,
    pub pane: u64,
    pub command: Command,
}
#[derive(Serialize, Deserialize)]
pub enum Command {
    Create {
        parent: u32,
        script: String,
        url: Option<String>,
        html: String,
    },
    Resize {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    Visible(bool),
    Focus(bool),
    Url(String),
    Html(String),
    Script(String),
    Back,
    Forward,
    Reload,
    Destroy,
    Shutdown,
}
#[derive(Serialize, Deserialize)]
pub enum Reply {
    Done {
        sequence: u64,
        error: Option<String>,
    },
    Event {
        pane: u64,
        event: EngineEvent,
    },
}
