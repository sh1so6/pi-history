//! 127.0.0.1 HTTP server: list page, /api/index, /session/<id>, plus token-protected control endpoints.
use crate::export::{ensure_session_html, Exporter};
use crate::scan::{build_index, now_iso, scan_sessions, Index, ScanOptions, ScanResult, SessionEntry, SpendRow};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tiny_http::{Header, Method, Request, Response, Server};

pub const INDEX_HTML: &str = include_str!("index.html");
pub const TOKEN_HEADER: &str = "X-Pi-History-Token";

/// Index + its pre-serialized `/api/index` body.
pub struct Snapshot {
    pub index: Index,
    pub json: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexJson<'a> {
    generated_at: &'a str,
    include_subagents: bool,
    entries: &'a [SessionEntry],
    spend: &'a [SpendRow],
}

impl Snapshot {
    #[allow(dead_code)]
    pub fn new(entries: Vec<SessionEntry>, include_subagents: bool) -> Snapshot {
        Snapshot::with_spend(entries, Vec::new(), include_subagents)
    }

    pub fn with_spend(entries: Vec<SessionEntry>, spend: Vec<SpendRow>, include_subagents: bool) -> Snapshot {
        let index = build_index(entries);
        let generated_at = now_iso();
        let json = serde_json::to_vec(&IndexJson { generated_at: &generated_at, include_subagents, entries: &index.entries, spend: &spend }).unwrap_or_default();
        Snapshot { index, json }
    }
}

pub struct AppState {
    pub snapshot: RwLock<Arc<Snapshot>>,
    pub sessions_dir: PathBuf,
    pub cache_path: PathBuf,
    pub html_dir: PathBuf,
    pub exporter: Box<Exporter>,
    pub token: String,
    pub port: u16,
    pub scanning: AtomicBool,
    pub on_shutdown: Box<dyn Fn() + Send + Sync>,
}

#[derive(Serialize)]
pub struct ScanSummary {
    pub ok: bool,
    pub sessions: usize,
    pub parsed: usize,
    pub cached: usize,
    pub ms: u128,
    #[serde(rename = "includeSubagents")]
    pub include_subagents: bool,
}

pub fn summarize(r: &ScanResult, include_subagents: bool) -> ScanSummary {
    ScanSummary { ok: true, sessions: r.entries.len(), parsed: r.parsed, cached: r.cached, ms: r.ms, include_subagents }
}

/// Scan the sessions directory and build a new snapshot (no locks held).
pub fn scan_snapshot(
    sessions_dir: &std::path::Path,
    cache_path: &std::path::Path,
    include_subagents: bool,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> (Snapshot, ScanSummary) {
    let res = scan_sessions(&ScanOptions { sessions_dir, cache_path, include_subagents, on_progress: progress });
    let summary = summarize(&res, include_subagents);
    (Snapshot::with_spend(res.entries, res.spend, include_subagents), summary)
}

impl AppState {
    /// Rescan and swap the snapshot. `None` when a scan is already running.
    pub fn rescan(&self, include_subagents: bool, progress: Option<&(dyn Fn(usize, usize) + Sync)>) -> Option<ScanSummary> {
        if self.scanning.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return None;
        }
        let (snap, summary) = scan_snapshot(&self.sessions_dir, &self.cache_path, include_subagents, progress);
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snap);
        self.scanning.store(false, Ordering::Release);
        Some(summary)
    }

    fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/* ------------------------------------------------------------------ helpers */

pub fn is_allowed_host(host: Option<&str>, port: u16) -> bool {
    match host {
        None => false,
        Some(h) => {
            let h = h.to_ascii_lowercase();
            h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
        }
    }
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

pub fn error_page(status: u16, message: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{status}</title><style>body{{font:16px system-ui;margin:2em;color-scheme:light dark}}pre{{white-space:pre-wrap}}</style><h1>{status}</h1><pre>{}</pre><p><a href=\"/\">← 一覧へ</a></p>",
        escape_html(message)
    )
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn base_headers(ctype: &str) -> Vec<Header> {
    vec![header("Content-Type", ctype), header("Cache-Control", "no-store"), header("X-Content-Type-Options", "nosniff")]
}

fn send(req: Request, status: u16, ctype: &str, body: Vec<u8>, extra: &[(&str, &str)]) {
    let mut r = Response::from_data(body).with_status_code(status);
    for h in base_headers(ctype) {
        r.add_header(h);
    }
    for (k, v) in extra {
        r.add_header(header(k, v));
    }
    let _ = req.respond(r);
}

const HTML: &str = "text/html; charset=utf-8";
const JSON: &str = "application/json; charset=utf-8";

fn send_html(req: Request, status: u16, msg: &str, extra: &[(&str, &str)]) {
    send(req, status, HTML, error_page(status, msg).into_bytes(), extra);
}

fn header_value(req: &Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn query_flag(query: &str, key: &str) -> bool {
    query.split('&').any(|kv| {
        let mut it = kv.splitn(2, '=');
        it.next() == Some(key) && matches!(it.next(), Some("1") | Some("true"))
    })
}

/* ------------------------------------------------------------------ request handling */

pub fn handle(state: &Arc<AppState>, req: Request) {
    if !is_allowed_host(header_value(&req, "Host").as_deref(), state.port) {
        return send_html(req, 403, "forbidden host", &[]);
    }
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url, String::new()),
    };
    let method = req.method().clone();
    if method == Method::Post && (path == "/api/rescan" || path == "/api/shutdown") {
        return handle_control(state, req, &path, &query);
    }
    if method != Method::Get && method != Method::Head {
        return send_html(req, 405, "method not allowed", &[("Allow", "GET, HEAD")]);
    }
    if path == "/" {
        return send(req, 200, HTML, INDEX_HTML.as_bytes().to_vec(), &[]);
    }
    if path == "/api/index" {
        let snap = state.snapshot();
        return send(req, 200, JSON, snap.json.clone(), &[]);
    }
    if let Some(raw_id) = path.strip_prefix("/session/") {
        let snap = state.snapshot();
        let Some(entry) = snap.index.resolve(raw_id) else {
            return send_html(req, 404, "session not found in the index", &[]);
        };
        return match ensure_session_html(&entry.id, std::path::Path::new(&entry.file), &state.html_dir, &*state.exporter) {
            // decorate at serve time (the cached export file stays untouched); PI_SESSION_HISTORY_NO_INJECT=1 serves it as is
            Ok(out) if !crate::inject::disabled() => match std::fs::read(&out) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(html) => send(req, 200, HTML, crate::inject::inject(&html).into_bytes(), &[]),
                    Err(e) => send(req, 200, HTML, e.into_bytes(), &[]),
                },
                Err(e) => send_html(req, 500, &format!("HTML export failed\n{e}"), &[]),
            },
            Ok(out) => match std::fs::File::open(&out) {
                Ok(f) => {
                    let mut r = Response::from_file(f).with_status_code(200);
                    for h in base_headers(HTML) {
                        r.add_header(h);
                    }
                    let _ = req.respond(r);
                }
                Err(e) => send_html(req, 500, &format!("HTML export failed\n{e}"), &[]),
            },
            Err(e) => send_html(req, 500, &format!("HTML export failed\n{e}"), &[]),
        };
    }
    send_html(req, 404, "not found", &[])
}

fn handle_control(state: &Arc<AppState>, req: Request, path: &str, query: &str) {
    // state-changing endpoints need the secret from server.json (CSRF / DNS-rebinding guard on top of the Host check)
    if header_value(&req, TOKEN_HEADER).as_deref() != Some(state.token.as_str()) {
        return send_html(req, 403, "bad token", &[]);
    }
    if path == "/api/shutdown" {
        send(req, 200, JSON, format!("{{\"ok\":true,\"pid\":{}}}", std::process::id()).into_bytes(), &[]);
        let st = state.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            (st.on_shutdown)();
        });
        return;
    }
    let include = query_flag(query, "all");
    match state.rescan(include, Some(&|d, t| crate::log_progress(d, t))) {
        Some(sum) => send(req, 200, JSON, serde_json::to_vec(&sum).unwrap_or_default(), &[]),
        None => send(req, 409, JSON, b"{\"ok\":false,\"error\":\"scan already running\"}".to_vec(), &[]),
    }
}

/// Serve requests on `threads` worker threads; returns when the server is unblocked/closed.
pub fn serve(state: Arc<AppState>, server: Arc<Server>, threads: usize) {
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let (state, server) = (state.clone(), server.clone());
            std::thread::spawn(move || {
                while let Ok(req) = server.recv() {
                    let st = state.clone();
                    // a panic in one request must not take the worker down
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(&st, req)));
                }
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::{jl, tmp_dir, write};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn host_guard() {
        assert!(is_allowed_host(Some("127.0.0.1:5000"), 5000));
        assert!(is_allowed_host(Some("localhost:5000"), 5000));
        assert!(is_allowed_host(Some("LocalHost:5000"), 5000));
        assert!(!is_allowed_host(Some("127.0.0.1:5001"), 5000));
        assert!(!is_allowed_host(Some("evil.example:5000"), 5000));
        assert!(!is_allowed_host(Some("127.0.0.1"), 5000));
        assert!(!is_allowed_host(None, 5000));
    }

    #[test]
    fn list_page_html_is_self_contained() {
        let html = INDEX_HTML;
        for id in ["cal", "q", "project", "sort", "clearDate", "prev", "next", "todayBtn", "list", "monthLabel"] {
            assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
        }
        assert!(html.contains("prefers-color-scheme:dark"));
        assert!(html.contains("/api/index"));
        assert!(!html.contains("http://") && !html.contains("https://"));
        assert!(!html.contains("<script src"));
        assert_eq!(html.matches("<script>").count(), 1);
    }

    fn request(port: u16, method: &str, path: &str, host: Option<&str>, extra: &[(&str, &str)]) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let host = host.map(str::to_string).unwrap_or(format!("127.0.0.1:{port}"));
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, head.to_string(), body.to_string())
    }

    #[test]
    fn http_routes() {
        let dir = tmp_dir("http");
        let sessions = dir.join("sessions");
        let file = write(&sessions, "p", "2026-10-05T00-00-00-000Z_s1.jsonl", &jl(&[r#"{"type":"session","id":"s1","timestamp":"2026-10-05T00:00:00.000Z"}"#.to_string()]));
        let entry = SessionEntry {
            id: "s1".into(), file: file.to_string_lossy().into_owned(), folder: "p".into(), project: "p".into(),
            started_at: "2026-10-05T00:00:00.000Z".into(), title: "t".into(), first_prompt: "f".into(), ..Default::default()
        };
        let fail = Arc::new(AtomicBool::new(false));
        let fail2 = fail.clone();
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let down = Arc::new(AtomicBool::new(false));
        let down2 = down.clone();
        let state = Arc::new(AppState {
            snapshot: RwLock::new(Arc::new(Snapshot::new(vec![entry], false))),
            sessions_dir: sessions.clone(),
            cache_path: dir.join("cache.json"),
            html_dir: dir.join("html"),
            exporter: Box::new(move |_s, out| {
                if fail2.load(Ordering::SeqCst) {
                    return Err("boom <bad>".into());
                }
                std::fs::write(out, "<html>session</html>").map_err(|e| e.to_string())
            }),
            token: "secret".into(),
            port,
            scanning: AtomicBool::new(false),
            on_shutdown: Box::new(move || down2.store(true, Ordering::SeqCst)),
        });
        let st = state.clone();
        let srv = server.clone();
        std::thread::spawn(move || serve(st, srv, 4));

        let (c, h, b) = request(port, "GET", "/", None, &[]);
        assert_eq!(c, 200);
        assert!(h.to_ascii_lowercase().contains("text/html"));
        assert!(b.contains("id=\"cal\""));
        let (c, h, b) = request(port, "GET", "/api/index", None, &[]);
        assert_eq!(c, 200);
        assert!(h.to_ascii_lowercase().contains("application/json"));
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["entries"][0]["id"], "s1");
        assert_eq!(v["entries"][0]["firstPrompt"], "f");
        assert!(v["generatedAt"].is_string() && v["includeSubagents"] == false);
        assert_eq!(request(port, "GET", "/api/index", Some(&format!("localhost:{port}")), &[]).0, 200);
        assert_eq!(request(port, "GET", "/api/index", Some("evil.example"), &[]).0, 403);
        assert_eq!(request(port, "GET", "/", Some(&format!("127.0.0.1:{}", port + 1)), &[]).0, 403);
        let (c, h, _) = request(port, "POST", "/", None, &[]);
        assert_eq!(c, 405);
        assert!(h.contains("Allow: GET, HEAD"));
        let (c, _, b) = request(port, "GET", "/session/s1", None, &[]);
        assert_eq!(c, 200);
        // injected at serve time (no </head>/</body> in the stub: style first, script last), cached file untouched
        assert!(b.starts_with(crate::inject::MARKER) && b.contains("<html>session</html>") && b.ends_with("</script>\n"));
        let cached = std::fs::read_to_string(state.html_dir.join(format!("{}.html", state.snapshot().index.entries[0].id))).unwrap();
        assert_eq!(cached, "<html>session</html>");
        assert_eq!(request(port, "GET", "/session/nope", None, &[]).0, 404);
        assert_eq!(request(port, "GET", "/session/..%2F..%2Fwindows%2Fwin.ini", None, &[]).0, 404);
        assert_eq!(request(port, "GET", &format!("/session/{}", crate::scan::encode_uri_component(&state.snapshot().index.entries[0].file)), None, &[]).0, 404);
        assert_eq!(request(port, "GET", "/other", None, &[]).0, 404);
        // control endpoints need the token
        assert_eq!(request(port, "POST", "/api/rescan?all=1", None, &[]).0, 403);
        assert_eq!(request(port, "POST", "/api/rescan?all=1", None, &[(TOKEN_HEADER, "wrong")]).0, 403);
        let (c, _, b) = request(port, "POST", "/api/rescan?all=1", None, &[(TOKEN_HEADER, "secret")]);
        assert_eq!(c, 200, "{b}");
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["includeSubagents"], true);
        assert_eq!(v["sessions"], 1);
        assert!(!down.load(Ordering::SeqCst));
        assert_eq!(request(port, "POST", "/api/shutdown", None, &[]).0, 403);
        assert_eq!(request(port, "POST", "/api/shutdown", None, &[(TOKEN_HEADER, "secret")]).0, 200);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(down.load(Ordering::SeqCst));
        // export failure -> 500 with the escaped message
        fail.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(dir.join("html"));
        let id = state.snapshot().index.entries[0].id.clone();
        let (c, _, b) = request(port, "GET", &format!("/session/{id}"), None, &[]);
        assert_eq!(c, 500);
        assert!(b.contains("boom &lt;bad&gt;"));
        server.unblock();
        let _ = std::fs::remove_dir_all(dir);
    }
}
