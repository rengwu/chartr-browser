use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};
use url::Url;
pub(crate) fn resolve_input(input: &str) -> Result<Url, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a URL or search query.".into());
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        return Url::parse(input)
            .map_err(|_| "That URL is not valid.".to_owned())
            .and_then(validate_http);
    }
    if is_local_host(input) {
        return Url::parse(&format!("http://{input}"))
            .map_err(|_| "That URL is not valid.".to_owned())
            .and_then(validate_http);
    }
    if looks_like_host(input) && has_no_scheme_like_colon(input) {
        return Url::parse(&format!("https://{input}"))
            .map_err(|_| "That URL is not valid.".to_owned())
            .and_then(validate_http);
    }
    if let Ok(url) = Url::parse(input) {
        return validate_http(url);
    }
    if input.contains("://") {
        return Err("That URL is not valid.".into());
    }
    Url::parse_with_params("https://duckduckgo.com/", &[("q", input)])
        .map_err(|error| error.to_string())
}

pub(crate) fn validate_http(url: Url) -> Result<Url, String> {
    if matches!(url.scheme(), "http" | "https") && url.host().is_some() {
        Ok(url)
    } else {
        Err("Browser supports only HTTP and HTTPS addresses.".into())
    }
}

pub(crate) fn looks_like_host(input: &str) -> bool {
    !input.chars().any(char::is_whitespace) && (input.contains('.') || is_local_host(input))
}

pub(crate) fn has_no_scheme_like_colon(input: &str) -> bool {
    let authority = input.split(['/', '?', '#']).next().unwrap_or(input);
    !authority.contains(':')
        || authority
            .rsplit_once(':')
            .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
}

pub(crate) fn is_local_host(input: &str) -> bool {
    let host = input.split(['/', '?', '#']).next().unwrap_or(input);
    let lowercase = host.to_ascii_lowercase();
    lowercase
        .strip_prefix("localhost")
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'))
        || is_ip_like(host)
}

pub(crate) fn is_ip_like(input: &str) -> bool {
    if let Some(bracketed) = input
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
    {
        return bracketed.parse::<std::net::IpAddr>().is_ok();
    }
    if input.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    input.rsplit_once(':').is_some_and(|(host, port)| {
        port.parse::<u16>().is_ok() && host.parse::<std::net::Ipv4Addr>().is_ok()
    })
}

pub(crate) fn is_http_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn is_allowed_navigation(url: &str) -> bool {
    is_http_url(url) || url == "about:blank"
}

pub(crate) fn display_title(title: &str) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "Browser".to_owned()
    } else {
        title
    }
}

pub(crate) fn state_path(data: &Path, space: &str, instance: u64) -> PathBuf {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in space.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    data.join("instances")
        .join(format!("{hash:016x}-{instance}.url"))
}

pub(crate) fn read_saved_url(path: &Path) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    is_http_url(value).then(|| value.to_owned())
}

pub(crate) fn save_url(path: &Path, url: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("url.tmp");
    fs::write(&temporary, url)?;
    fs::rename(temporary, path)
}

pub(crate) type BrowserTheme = chartr_native_plugin::Theme;

pub(crate) enum LocalPage<'a> {
    Start,
    Error { message: &'a str, retry: &'a str },
}

pub(crate) fn local_page(theme: &BrowserTheme, page: LocalPage<'_>) -> String {
    let theme = serde_json::to_string(theme).unwrap_or_else(|_| "{}".into());
    let body = match page {
        LocalPage::Start => {
            "<div class='globe'>🌏</div><h1>Browse the web</h1><p>Enter a URL or search above"
                .to_owned()
        }
        LocalPage::Error { message, retry } => format!(
            "<div class='globe'>!</div><h1>Page unavailable</h1><p>{}</p><button id='retry' data-address='{}'>Retry</button>",
            escape_html(message),
            escape_html(retry),
        ),
    };
    LOCAL_HTML
        .replace("__THEME__", &theme)
        .replace("__BODY__", &body)
}

pub(crate) fn escape_html(value: &str) -> Cow<'_, str> {
    if !value.contains(['&', '<', '>', '"', '\'']) {
        return Cow::Borrowed(value);
    }
    Cow::Owned(
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;"),
    )
}

pub(crate) const CONTENT_BRIDGE: &str = r#"
(() => {
  const send = (action, value) => window.ipc.postMessage(JSON.stringify({ action, value }));
  addEventListener('pointerdown', () => send('focus'), true);
  addEventListener('keydown', event => {
    const command = navigator.platform.includes('Mac') ? event.metaKey : event.ctrlKey;
    let action = null;
    if (command && event.key.toLowerCase() === 'l') action = 'focus-address';
    else if (command && event.key.toLowerCase() === 'r') action = 'reload';
    else if (event.key === 'Escape') action = 'stop';
    else if (event.altKey && event.key === 'ArrowLeft') action = 'back';
    else if (event.altKey && event.key === 'ArrowRight') action = 'forward';
    else if (event.metaKey && event.key === '[') action = 'back';
    else if (event.metaKey && event.key === ']') action = 'forward';
    if (action) { event.preventDefault(); event.stopPropagation(); send(action); }
  }, true);
})();
"#;

// Wry's child WKWebView intentionally declines macOS key equivalents so the
// host can handle menu shortcuts. That also keeps Cmd+A out of webpage
// JavaScript, so the browser-content key context performs the equivalent DOM
// selection explicitly.
pub(crate) const SELECT_PAGE_CONTENT: &str = r#"
(() => {
  let active = document.activeElement;
  while (active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;

  if (active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement) {
    try {
      active.select();
      return;
    } catch (_) {}
  }

  if (active instanceof HTMLElement && active.isContentEditable) {
    const range = document.createRange();
    range.selectNodeContents(active);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    return;
  }

  const range = document.createRange();
  range.selectNodeContents(document.body);
  const selection = window.getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
})();
"#;

pub(crate) const LOCAL_HTML: &str = r#"<!doctype html>
<meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Browser</title>
<style>
  :root { --page:#181818;--field:#202020;--border:#393939;--text:#ddd;--muted:#888;--focus:#d97757;--uiFontSize:14px; }
  html { font-size:var(--uiFontSize); } html,body { height:100%;margin:0; } body { display:grid;place-items:center;background:var(--page);color:var(--text);font:.857142857rem/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
  main { max-width:560px;padding:32px;text-align:center; } .globe { color:var(--muted);font-size:2rem;line-height:1;margin-bottom:1rem; } h1 { margin:0 0 .5rem;font-size:1rem;font-weight:600; } p { margin:0;color:var(--muted); } button { margin-top:24px;padding:9px 16px;border:1px solid var(--border);border-radius:8px;background:var(--field);color:var(--text);font:inherit;cursor:pointer; }
</style><main>__BODY__</main>
<script>window.setTheme=t=>{for(const [key,value] of Object.entries(t))document.documentElement.style.setProperty('--'+key,value)};window.setTheme(__THEME__);const retry=document.querySelector('#retry');if(retry)retry.onclick=()=>window.ipc.postMessage(JSON.stringify({action:'navigate',value:retry.dataset.address}));</script>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_input_distinguishes_urls_local_hosts_and_searches() {
        assert_eq!(
            resolve_input("example.com/a").unwrap().as_str(),
            "https://example.com/a"
        );
        assert_eq!(
            resolve_input("localhost:3000").unwrap().as_str(),
            "http://localhost:3000/"
        );
        assert_eq!(
            resolve_input("127.0.0.1:8080").unwrap().as_str(),
            "http://127.0.0.1:8080/"
        );
        assert_eq!(
            resolve_input("[::1]:8080").unwrap().as_str(),
            "http://[::1]:8080/"
        );
        assert_eq!(
            resolve_input("example.com:8443/path").unwrap().as_str(),
            "https://example.com:8443/path"
        );
        assert_eq!(
            resolve_input("localhost.example/path").unwrap().as_str(),
            "https://localhost.example/path"
        );
        let search = resolve_input("small browser").unwrap();
        assert_eq!(search.host_str(), Some("duckduckgo.com"));
        assert_eq!(
            search.query_pairs().find(|(key, _)| key == "q").unwrap().1,
            "small browser"
        );
    }

    #[test]
    fn non_web_schemes_are_rejected() {
        for input in ["file:///tmp/secret", "mailto:user@example.com", "tel:123"] {
            assert!(resolve_input(input).is_err(), "{input}");
        }
    }

    #[test]
    fn document_titles_are_normalized_for_tabs() {
        assert_eq!(display_title("  Example\n  Page  "), "Example Page");
        assert_eq!(display_title(" \n\t "), "Browser");
    }

    #[test]
    fn instance_state_is_namespaced_by_space_and_instance() {
        let root = Path::new("/data");
        assert_ne!(
            state_path(root, "folder:a", 7),
            state_path(root, "folder:b", 7)
        );
        assert_ne!(
            state_path(root, "folder:a", 7),
            state_path(root, "folder:a", 8)
        );
    }

    #[test]
    fn local_pages_follow_the_host_interface_font_size() {
        let theme: BrowserTheme = serde_json::from_value(serde_json::json!({
            "page":"#000", "field":"#000", "border":"#000", "focus":"#000",
            "text":"#fff", "muted":"#888", "uiFontSize":"18px"
        }))
        .unwrap();
        assert!(local_page(&theme, LocalPage::Start).contains(r#""uiFontSize":"18px""#));
    }
}
