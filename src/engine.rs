//! Engines remain entirely plugin-owned. No GPUI or Chartr implementation types.
use crate::{EngineEvent, pages::CONTENT_BRIDGE};
use anyhow::{Result, bail};
#[cfg(target_os = "macos")]
use chartr_browser_wry as wry;
use std::sync::Arc;

type Events = Arc<dyn Fn(EngineEvent) + Send + Sync>;
#[cfg(target_os = "linux")]
pub type WebView = crate::linux::WebView;
#[cfg(target_os = "macos")]
pub type WebView = wry::WebView;

#[cfg(target_os = "linux")]
pub fn build(
    kind: u32,
    parent: usize,
    url: Option<&str>,
    html: &str,
    events: Events,
) -> Result<WebView> {
    if kind != chartr_native_plugin::PARENT_X11 {
        bail!("Browser requires an X11 or XWayland parent");
    }
    WebView::new(parent.try_into()?, CONTENT_BRIDGE, url, html, events)
}

#[cfg(target_os = "macos")]
pub fn build(
    kind: u32,
    parent: usize,
    url: Option<&str>,
    html: &str,
    events: Events,
) -> Result<WebView> {
    use wry::{
        PageLoadEvent, WebContext, WebViewBuilder,
        raw_window_handle::{AppKitWindowHandle, WindowHandle},
    };
    if kind != chartr_native_plugin::PARENT_NS_VIEW {
        bail!("Browser requires an AppKit parent");
    }
    let pointer = std::ptr::NonNull::new(parent as *mut std::ffi::c_void)
        .ok_or_else(|| anyhow::anyhow!("Missing parent view"))?;
    // SAFETY: ABI v1 borrows this live NSView until destroy returns, on the UI thread.
    let parent = unsafe { WindowHandle::borrow_raw(AppKitWindowHandle::new(pointer).into()) };
    let mut context = WebContext::new(None);
    let ipc = events.clone();
    let popups = events.clone();
    let downloads = events.clone();
    let titles = events.clone();
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_incognito(true)
        .with_initialization_script(CONTENT_BRIDGE)
        .with_ipc_handler(move |r| ipc(EngineEvent::Message(r.body().clone())))
        .with_navigation_handler(|url| crate::pages::is_allowed_navigation(&url))
        .with_new_window_req_handler(move |url, _| {
            popups(EngineEvent::Message(
                serde_json::json!({"action":"navigate", "value":url}).to_string(),
            ));
            wry::NewWindowResponse::Deny
        })
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_download_started_handler(move |url, _| {
            downloads(EngineEvent::Download(url));
            false
        })
        .with_document_title_changed_handler(move |v| titles(EngineEvent::Title(v)))
        .with_on_page_load_handler(move |event, url| {
            events(match event {
                PageLoadEvent::Started => EngineEvent::Started(url),
                PageLoadEvent::Finished => EngineEvent::Finished(url),
            })
        })
        .with_bounds(wry::Rect {
            position: wry::dpi::LogicalPosition::new(0., 0.).into(),
            size: wry::dpi::LogicalSize::new(1., 1.).into(),
        })
        .with_visible(false)
        .with_focused(false);
    let builder = if let Some(url) = url {
        builder.with_url(url)
    } else {
        builder.with_html(html)
    };
    Ok(builder.build_as_child(&parent)?)
}

pub fn resize(view: &WebView, x: f64, y: f64, w: f64, h: f64, scale: f64) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let left = (x * scale).round() as i32;
        let top = (y * scale).round() as i32;
        view.set_bounds(
            left,
            top,
            (((x + w) * scale).round() - left as f64).max(0.) as u32,
            (((y + h) * scale).round() - top as f64).max(0.) as u32,
        )?;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = scale;
        view.set_bounds(wry::Rect {
            position: wry::dpi::LogicalPosition::new(x, y).into(),
            size: wry::dpi::LogicalSize::new(w, h).into(),
        })?;
    }
    Ok(())
}
