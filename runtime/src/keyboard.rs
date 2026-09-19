//! Keep toolbar input out of Chromium while the pointer is over its native child.
//!
//! X11 routes keys to descendants of the focus window beneath the pointer. A
//! sibling focus proxy avoids that rule (the same technique used by XEmbed).
//! Forward the original key events to the host, which still owns keymaps and IME.

use std::{sync::Arc, thread::JoinHandle};

use anyhow::Result;
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xproto::{ConnectionExt, CreateWindowAux, EventMask, InputFocus, WindowClass},
    },
    rust_connection::RustConnection,
};

pub(crate) struct KeyboardFocus {
    connection: Arc<RustConnection>,
    window: u32,
    worker: Option<JoinHandle<()>>,
}

impl KeyboardFocus {
    pub(crate) fn new(parent: u32) -> Result<Self> {
        let (connection, _) = x11rb::connect(None)?;
        let connection = Arc::new(connection);
        let window = connection.generate_id()?;
        connection
            .create_window(
                0,
                window,
                parent,
                -1,
                -1,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(
                    EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::STRUCTURE_NOTIFY,
                ),
            )?
            .check()?;
        let mut proxy = Self {
            connection: connection.clone(),
            window,
            worker: None,
        };
        connection.map_window(window)?.check()?;
        proxy.worker = Some(
            std::thread::Builder::new()
                .name("browser-keyboard".into())
                .spawn(move || {
                    while let Ok(event) = connection.wait_for_event() {
                        let (mut key, mask) = match event {
                            Event::KeyPress(key) => (key, EventMask::KEY_PRESS),
                            Event::KeyRelease(key) => (key, EventMask::KEY_RELEASE),
                            Event::DestroyNotify(_) => break,
                            _ => continue,
                        };
                        key.event = parent;
                        key.child = x11rb::NONE;
                        key.event_x = key.event_x.saturating_sub(1);
                        key.event_y = key.event_y.saturating_sub(1);
                        if connection.send_event(false, parent, mask, key).is_err()
                            || connection.flush().is_err()
                        {
                            break;
                        }
                    }
                })?,
        );
        Ok(proxy)
    }

    pub(crate) fn focus(&self) -> Result<()> {
        self.connection
            .set_input_focus(InputFocus::PARENT, self.window, x11rb::CURRENT_TIME)?
            .check()?;
        Ok(())
    }
}

impl Drop for KeyboardFocus {
    fn drop(&mut self) {
        // DestroyNotify wakes the worker, including when the parent closes first.
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.flush();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use x11rb::protocol::xproto::{KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask, KeyPressEvent};

    #[test]
    #[ignore = "requires DISPLAY; uses unmapped test windows without changing desktop focus"]
    fn forwards_native_keys_and_stops_when_parent_is_destroyed() -> Result<()> {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        let parent = connection.generate_id()?;
        connection
            .create_window(
                0,
                parent,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
            )?
            .check()?;
        let proxy = KeyboardFocus::new(parent)?;
        for (response_type, mask) in [
            (KEY_PRESS_EVENT, EventMask::KEY_PRESS),
            (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE),
        ] {
            let key = KeyPressEvent {
                response_type,
                detail: 38,
                time: 1234,
                root,
                event: proxy.window,
                event_x: 21,
                event_y: 31,
                root_x: 20,
                root_y: 30,
                state: KeyButMask::CONTROL | KeyButMask::SHIFT,
                same_screen: true,
                ..Default::default()
            };
            connection.send_event(false, proxy.window, mask, key)?;
            connection.flush()?;
            let deadline = Instant::now() + Duration::from_secs(2);
            let received = loop {
                if let Some(event) = connection.poll_for_event()? {
                    match event {
                        Event::KeyPress(key) | Event::KeyRelease(key) => break key,
                        _ => continue,
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "forwarded key did not reach the parent"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(received.response_type & 0x7f, response_type);
            assert_eq!(received.event, parent);
            assert_eq!(received.detail, key.detail);
            assert_eq!(received.state, key.state);
            assert_eq!(received.time, key.time);
            assert_eq!((received.event_x, received.event_y), (20, 30));
        }
        connection.destroy_window(parent)?.check()?;
        drop(proxy);
        Ok(())
    }
}
