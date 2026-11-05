use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST,
    REFERRER_POLICY, TRANSFER_ENCODING, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::Response;
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Notify, oneshot};
use zeroize::Zeroizing;

const LOCKED_DOWN_CSP: &str = "default-src 'none'; base-uri 'none'; frame-ancestors 'none'";
const MAX_CODE_LEN: usize = 128;

pub(crate) fn bind(port: u16) -> std::io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub(crate) struct Page {
    pub form_action: String,
    pub manifest: String,
    pub form_origin: String,
}

pub(crate) struct Tokens {
    pub state: Zeroizing<String>,
    pub launch: Zeroizing<String>,
    pub nonce: String,
}

pub(crate) struct Claim {
    pub code: Zeroizing<String>,
    pub reply: oneshot::Sender<String>,
}

struct Shared {
    host: String,
    tokens: Tokens,
    page: Page,
    start_served: AtomicBool,
    claimed: AtomicBool,
    claims: Mutex<Option<oneshot::Sender<Claim>>>,
    stop: Arc<Notify>,
}

pub(crate) struct Listener {
    pub claim: oneshot::Receiver<Claim>,
    stop: Arc<Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Listener {
    pub(crate) async fn close(self) {
        self.stop.notify_one();
        let _ = self.server.await;
    }
}

pub(crate) fn start(
    listener: std::net::TcpListener,
    port: u16,
    tokens: Tokens,
    page: Page,
) -> std::io::Result<Listener> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let (claims, claim) = oneshot::channel();
    let stop = Arc::new(Notify::new());
    let shared = Arc::new(Shared {
        host: format!("127.0.0.1:{port}"),
        tokens,
        page,
        start_served: AtomicBool::new(false),
        claimed: AtomicBool::new(false),
        claims: Mutex::new(Some(claims)),
        stop: stop.clone(),
    });
    let app = axum::Router::new().fallback(handle).with_state(shared);
    let stopped = stop.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stopped.notified().await })
            .await;
    });
    Ok(Listener {
        claim,
        stop,
        server,
    })
}

fn respond(status: StatusCode, csp: &str, content_type: &'static str, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(CONNECTION, HeaderValue::from_static("close"));
    if let Ok(csp) = HeaderValue::from_str(csp) {
        headers.insert(CONTENT_SECURITY_POLICY, csp);
    }
    response
}

fn plain(status: StatusCode) -> Response {
    respond(
        status,
        LOCKED_DOWN_CSP,
        "text/plain; charset=utf-8",
        status.canonical_reason().unwrap_or_default().to_string(),
    )
}

pub(crate) fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn message_page(text: &str) -> Response {
    respond(
        StatusCode::OK,
        LOCKED_DOWN_CSP,
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Saka</title></head><body><p>{}</p></body></html>",
            html_escape(text)
        ),
    )
}

fn start_page(shared: &Shared) -> Response {
    let nonce = &shared.tokens.nonce;
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; form-action {}; base-uri 'none'; frame-ancestors 'none'",
        shared.page.form_origin
    );
    let body = format!(
        concat!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Saka</title></head><body>",
            "<form id=\"manifest\" method=\"post\" action=\"{action}\">",
            "<input type=\"hidden\" name=\"manifest\" value=\"{manifest}\">",
            "<button type=\"submit\">Create the GitHub App on GitHub</button>",
            "</form>",
            "<script nonce=\"{nonce}\">document.getElementById(\"manifest\").submit()</script>",
            "</body></html>"
        ),
        action = html_escape(&shared.page.form_action),
        manifest = html_escape(&shared.page.manifest),
        nonce = nonce,
    );
    respond(StatusCode::OK, &csp, "text/html; charset=utf-8", body)
}

fn same_secret(given: &str, expected: &str) -> bool {
    bool::from(given.as_bytes().ct_eq(expected.as_bytes()))
}

fn code_ok(code: &str) -> bool {
    (1..=MAX_CODE_LEN).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn has_body(request: &Request) -> bool {
    let headers = request.headers();
    headers.contains_key(TRANSFER_ENCODING)
        || headers
            .get(CONTENT_LENGTH)
            .is_some_and(|len| len.as_bytes() != b"0")
}

fn right_host(shared: &Shared, request: &Request) -> bool {
    let header_ok = request
        .headers()
        .get(HOST)
        .is_some_and(|h| h.as_bytes() == shared.host.as_bytes());
    let authority_ok = request
        .uri()
        .authority()
        .is_none_or(|a| a.as_str() == shared.host);
    header_ok && authority_ok
}

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    if !right_host(&shared, &request) {
        return plain(StatusCode::MISDIRECTED_REQUEST);
    }
    let path = request.uri().path();
    let known = path == "/callback" || path.starts_with("/start/");
    if !known {
        return plain(StatusCode::NOT_FOUND);
    }
    if request.method() != Method::GET {
        return plain(StatusCode::METHOD_NOT_ALLOWED);
    }
    if has_body(&request) {
        return plain(StatusCode::PAYLOAD_TOO_LARGE);
    }
    if let Some(launch) = path.strip_prefix("/start/") {
        if !same_secret(launch, &shared.tokens.launch) {
            return plain(StatusCode::NOT_FOUND);
        }
        if shared.start_served.swap(true, Ordering::SeqCst) {
            return plain(StatusCode::GONE);
        }
        return start_page(&shared);
    }
    let uri = request.uri().clone();
    drop(request);
    callback(&shared, &uri).await
}

async fn callback(shared: &Shared, uri: &axum::http::Uri) -> Response {
    let Ok(Query(query)) = Query::<HashMap<String, String>>::try_from_uri(uri) else {
        return plain(StatusCode::BAD_REQUEST);
    };
    let state_ok = query
        .get("state")
        .is_some_and(|s| same_secret(s, &shared.tokens.state));
    if !state_ok {
        return plain(StatusCode::FORBIDDEN);
    }
    let Some(code) = query.get("code").filter(|c| code_ok(c)) else {
        return plain(StatusCode::BAD_REQUEST);
    };
    if shared.claimed.swap(true, Ordering::SeqCst) {
        return plain(StatusCode::GONE);
    }
    let (reply, answer) = oneshot::channel();
    let claim = Claim {
        code: Zeroizing::new(code.clone()),
        reply,
    };
    shared.stop.notify_one();
    let Some(claims) = shared.claims.lock().await.take() else {
        return plain(StatusCode::GONE);
    };
    if claims.send(claim).is_err() {
        return plain(StatusCode::SERVICE_UNAVAILABLE);
    }
    match answer.await {
        Ok(text) => message_page(&text),
        Err(_) => plain(StatusCode::SERVICE_UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listener_binds_only_to_the_ipv4_loopback() {
        let any = bind(0).unwrap();
        assert_eq!(any.local_addr().unwrap().ip(), Ipv4Addr::LOCALHOST);
        let port = any.local_addr().unwrap().port();
        drop(any);
        let given = bind(port).unwrap();
        let at = given.local_addr().unwrap();
        assert_eq!(at.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(at.port(), port);
    }

    #[test]
    fn codes_are_short_url_safe_tokens() {
        assert!(code_ok("abc123_-XYZ"));
        assert!(!code_ok(""));
        assert!(!code_ok("a/b"));
        assert!(!code_ok(&"a".repeat(129)));
    }

    #[test]
    fn markup_is_escaped_in_attributes_and_text() {
        assert_eq!(
            html_escape(r#"{"a":"<b>&'"}"#),
            "{&quot;a&quot;:&quot;&lt;b&gt;&amp;&#39;&quot;}"
        );
    }
}
