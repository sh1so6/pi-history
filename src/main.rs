//! pi-history: browse past Pi sessions in the browser (list + search + calendar filter).
//! Port of the `session-history.ts` Pi extension as a standalone executable. See README.md.
mod config;
mod export;
mod inject;
mod prefs;
mod scan;
mod server;

use config::{agent_dir, load_config, parse_args, ParsedArgs};
use serde::{Deserialize, Serialize};
use server::{scan_snapshot, serve, AppState, TOKEN_HEADER};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const HELP: &str = "pi-history [stop|all|<project substring>]  (alias: pi-history all <text>)

  (none)     scan sessions (cached), start or reuse the local server, open the list page in the browser
  <text>     same, with the project filter pre-set to projects containing <text>
  all [text] also include sub-agent temp folders (Temp-pi-agent-*)
  stop       stop the background server

Options:
  --foreground, -f   run the server in this process instead of in the background
  --help, -h         this help
  --version, -V      version

Env: PI_CODING_AGENT_DIR (agent dir), PI_SESSION_HISTORY_NO_OPEN=1 (do not open the browser),
     PI_SESSION_HISTORY_PI_CLI (path to Pi's cli.js for `pi --export`)
Config: <agentDir>/session-history.json  { \"includeSubagents\": false, \"port\": 0 }";

/* ------------------------------------------------------------------ logging (never panics on a closed pipe) */

pub fn log(msg: &str) {
    let _ = writeln!(std::io::stderr(), "{msg}");
}

static LAST_PROGRESS_MS: AtomicU64 = AtomicU64::new(0);

/// Throttled scan progress on stderr.
pub fn log_progress(done: usize, total: usize) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let last = LAST_PROGRESS_MS.load(Ordering::Relaxed);
    if done >= total || (now.saturating_sub(last) > 1000 && LAST_PROGRESS_MS.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()) {
        log(&format!("history: スキャン中 {done}/{total}"));
    }
}

/* ------------------------------------------------------------------ server.json */

#[derive(Serialize, Deserialize, Clone)]
struct ServerInfo {
    pid: u32,
    port: u16,
    token: String,
    #[serde(rename = "startedAt")]
    started_at: String,
}

fn server_json_path(agent: &Path) -> PathBuf {
    agent.join("session-history").join("server.json")
}

fn read_server_info(agent: &Path) -> Option<ServerInfo> {
    serde_json::from_str(&std::fs::read_to_string(server_json_path(agent)).ok()?).ok()
}

fn write_server_info(agent: &Path, info: &ServerInfo) -> std::io::Result<()> {
    let path = server_json_path(agent);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(info)?)?;
    std::fs::rename(&tmp, &path)
}

fn remove_server_info_if(agent: &Path, token: Option<&str>) {
    if let Some(t) = token {
        if read_server_info(agent).map(|i| i.token) != Some(t.to_string()) {
            return;
        }
    }
    let _ = std::fs::remove_file(server_json_path(agent));
}

fn new_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u32 {
        let mut h = RandomState::new().build_hasher();
        h.write_u32(i);
        h.write_u32(std::process::id());
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/* ------------------------------------------------------------------ tiny HTTP client for the control endpoints */

/// POST to our own server; Ok((status, body)). Err when nothing (valid) answers.
fn http_post(port: u16, path: &str, token: &str, timeout: Duration) -> Result<(u16, String), String> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    let req = format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed response")?;
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or("malformed response")?;
    Ok((status, body.to_string()))
}

fn port_open(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

fn body_ok(body: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if v.get("ok") == Some(&serde_json::Value::Bool(true)) { Some(v) } else { None }
}

/* ------------------------------------------------------------------ browser */

fn open_url(url: &str) {
    let mut cmd;
    if cfg!(windows) {
        cmd = Command::new("rundll32");
        cmd.args(["url.dll,FileProtocolHandler", url]);
    } else if cfg!(target_os = "macos") {
        cmd = Command::new("open");
        cmd.arg(url);
    } else {
        cmd = Command::new("xdg-open");
        cmd.arg(url);
    }
    export::no_window(&mut cmd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Err(e) = cmd.spawn() {
        log(&format!("history: could not open browser: {e}"));
    }
}

fn no_open() -> bool {
    std::env::var("PI_SESSION_HISTORY_NO_OPEN").map(|v| !v.is_empty()).unwrap_or(false)
}

fn page_url(port: u16, project: &str) -> String {
    let q = if project.is_empty() { String::new() } else { format!("?project={}", scan::encode_uri_component(project)) };
    format!("http://127.0.0.1:{port}/{q}")
}

#[allow(clippy::too_many_arguments)]
fn announce(sessions: u64, parsed: u64, cached: u64, ms: u64, reused: bool, url: &str) {
    println!(
        "history: {sessions} sessions (parsed {parsed}, cached {cached}, {:.1}s){}\n{url}\n(pi-history stop で停止)",
        ms as f64 / 1000.0,
        if reused { " [server reused]" } else { "" }
    );
    let _ = std::io::stdout().flush();
}

/* ------------------------------------------------------------------ server process */

/// Scan, bind, write server.json. Returns the listening server + state + the scan summary.
fn start_server(agent: &Path, include: bool) -> Result<(Arc<tiny_http::Server>, Arc<AppState>, server::ScanSummary), String> {
    let cfg = load_config(agent);
    let data_dir = agent.join("session-history");
    let sessions_dir = agent.join("sessions");
    let cache_path = data_dir.join("cache.json");
    let prefs_path = data_dir.join("ui.json");
    let (snap, summary) = scan_snapshot(&sessions_dir, &cache_path, include, Some(&|d, t| log_progress(d, t)));
    let tserver = tiny_http::Server::http(("127.0.0.1", cfg.port)).map_err(|e| format!("could not start server: {e}"))?;
    let port = tserver.server_addr().to_ip().map(|a| a.port()).ok_or("no ip address")?;
    let token = new_token();
    let info = ServerInfo { pid: std::process::id(), port, token: token.clone(), started_at: scan::now_iso() };
    write_server_info(agent, &info).map_err(|e| format!("could not write server.json: {e}"))?;
    let agent_owned = agent.to_path_buf();
    let tk = token.clone();
    let state = Arc::new(AppState {
        snapshot: RwLock::new(Arc::new(snap)),
        sessions_dir,
        cache_path,
        html_dir: data_dir.join("html"),
        prefs: RwLock::new(prefs::load(&prefs_path)),
        prefs_path,
        exporter: Box::new(export::export_with_pi),
        token,
        port,
        scanning: AtomicBool::new(false),
        on_shutdown: Box::new(move || {
            remove_server_info_if(&agent_owned, Some(&tk));
            std::process::exit(0);
        }),
    });
    Ok((Arc::new(tserver), state, summary))
}

/// `--daemon-child`: runs detached; tells the launcher via one stdout line, then serves forever.
fn run_daemon_child(agent: &Path, include: bool) -> i32 {
    match start_server(agent, include) {
        Ok((srv, state, sum)) => {
            println!("READY {} {} {} {} {}", state.port, sum.sessions, sum.parsed, sum.cached, sum.ms);
            let _ = std::io::stdout().flush();
            serve(state, srv, 16);
            0
        }
        Err(e) => {
            log(&format!("history: {e}"));
            1
        }
    }
}

/// Windows: CreateProcess(bInheritHandles=TRUE) would hand our own std handles to the detached server, so a caller
/// capturing our output through a pipe would never see EOF. Make them non-inheritable first (the child gets its own pipes).
#[cfg(windows)]
fn uninherit_std_handles() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> isize;
        fn SetHandleInformation(h: isize, mask: u32, flags: u32) -> i32;
    }
    for n in [-10i32, -11, -12] {
        // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE; HANDLE_FLAG_INHERIT = 1
        unsafe {
            let h = GetStdHandle(n as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, 1, 0);
            }
        }
    }
}

fn spawn_daemon(include: bool) -> Result<(u16, [u64; 4]), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let build = |breakaway: bool| {
        let mut cmd = Command::new(&exe);
        cmd.arg("--daemon-child");
        if include {
            cmd.arg("--sub");
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP (| CREATE_BREAKAWAY_FROM_JOB)
            cmd.creation_flags(0x0000_0008 | 0x0000_0200 | if breakaway { 0x0100_0000 } else { 0 });
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let _ = breakaway;
            cmd.process_group(0);
        }
        #[cfg(not(any(windows, unix)))]
        let _ = breakaway;
        cmd
    };
    #[cfg(windows)]
    uninherit_std_handles();
    let mut child = match build(true).spawn() {
        Ok(c) => c,
        Err(_) => build(false).spawn().map_err(|e| format!("could not start background server: {e}"))?,
    };
    let stderr = child.stderr.take().ok_or("no stderr")?;
    std::thread::spawn(move || {
        for l in BufReader::new(stderr).lines().map_while(Result::ok) {
            log(&l);
        }
    });
    let stdout = child.stdout.take().ok_or("no stdout")?;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(rest) = line.strip_prefix("READY ") {
            let n: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if n.len() == 5 {
                return Ok((n[0] as u16, [n[1], n[2], n[3], n[4]]));
            }
        }
    }
    let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
    Err(format!("background server exited before it was ready ({status})"))
}

/* ------------------------------------------------------------------ commands */

fn cmd_stop(agent: &Path) -> i32 {
    let Some(info) = read_server_info(agent) else {
        println!("history: server is not running");
        return 0;
    };
    match http_post(info.port, "/api/shutdown", &info.token, Duration::from_secs(5)) {
        Ok((200, body)) if body_ok(&body).is_some() => {
            let t0 = Instant::now();
            while port_open(info.port) && t0.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(100));
            }
            if port_open(info.port) {
                // the server answered with our token but did not exit: kill the verified pid
                kill_pid(info.pid);
            }
            remove_server_info_if(agent, Some(&info.token));
            println!("history: server stopped");
        }
        _ => {
            remove_server_info_if(agent, None);
            println!("history: server is not running (removed stale server.json)");
        }
    }
    0
}

fn kill_pid(pid: u32) {
    let mut cmd;
    if cfg!(windows) {
        cmd = Command::new("taskkill");
        cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
    } else {
        cmd = Command::new("kill");
        cmd.args(["-9", &pid.to_string()]);
    }
    let _ = export::no_window(&mut cmd).stdout(Stdio::null()).stderr(Stdio::null()).status();
    std::thread::sleep(Duration::from_millis(300));
}

fn cmd_open(agent: &Path, project: &str, all: bool, foreground: bool) -> i32 {
    let include = load_config(agent).include_subagents || all;
    // reuse a running server: ask it to rescan, then just open the page
    if let Some(info) = read_server_info(agent) {
        log("history: 既存サーバを再スキャン中…");
        let path = format!("/api/rescan?all={}", if include { 1 } else { 0 });
        match http_post(info.port, &path, &info.token, Duration::from_secs(30 * 60)) {
            Ok((status, body)) if status == 200 || status == 409 => {
                let url = page_url(info.port, project);
                match body_ok(&body) {
                    Some(v) => announce(v["sessions"].as_u64().unwrap_or(0), v["parsed"].as_u64().unwrap_or(0), v["cached"].as_u64().unwrap_or(0), v["ms"].as_u64().unwrap_or(0), true, &url),
                    None => println!("history: scan already running on the existing server\n{url}"),
                }
                if !no_open() {
                    open_url(&url);
                }
                return 0;
            }
            _ => {
                log("history: server.json is stale; starting a new server");
                remove_server_info_if(agent, Some(&info.token));
            }
        }
    }
    if foreground {
        let (srv, state, sum) = match start_server(agent, include) {
            Ok(x) => x,
            Err(e) => {
                log(&format!("history: {e}"));
                return 1;
            }
        };
        let url = page_url(state.port, project);
        announce(sum.sessions as u64, sum.parsed as u64, sum.cached as u64, sum.ms as u64, false, &url);
        if !no_open() {
            open_url(&url);
        }
        serve(state, srv, 16);
        return 0;
    }
    match spawn_daemon(include) {
        Ok((port, [sessions, parsed, cached, ms])) => {
            let url = page_url(port, project);
            announce(sessions, parsed, cached, ms, false, &url);
            if !no_open() {
                open_url(&url);
            }
            0
        }
        Err(e) => {
            log(&format!("history: {e}"));
            1
        }
    }
}

fn run() -> i32 {
    let mut foreground = false;
    let mut daemon_child = false;
    let mut sub = false;
    let mut words: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            "-V" | "--version" => {
                println!("pi-history {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            "-f" | "--foreground" => foreground = true,
            "--daemon-child" => daemon_child = true,
            "--sub" => sub = true,
            _ => words.push(a),
        }
    }
    let agent = agent_dir();
    if daemon_child {
        return run_daemon_child(&agent, sub);
    }
    match parse_args(&words.join(" ")) {
        ParsedArgs::Stop => cmd_stop(&agent),
        ParsedArgs::Open { project, all } => cmd_open(&agent, &project, all, foreground),
    }
}

fn main() {
    std::process::exit(run());
}
