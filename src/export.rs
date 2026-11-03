//! Per-session HTML via Pi's own exporter: `pi --export <jsonl> <out>` (child process, no shell).
use crate::scan::sanitize_id;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);

pub type Exporter = dyn Fn(&Path, &Path) -> Result<(), String> + Send + Sync;

/// Keep child processes from opening console windows (the server runs detached on Windows).
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/* ------------------------------------------------------------------ locating pi */

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Pull the cli.js path out of an npm `pi.cmd` shim.
pub fn parse_pi_cmd_shim(text: &str, shim_dir: &Path) -> Option<PathBuf> {
    let lower = text.to_ascii_lowercase();
    const KEY: &str = "\"%~dp0";
    for (i, _) in lower.match_indices(KEY) {
        let rest = &text[i + KEY.len()..];
        let rel = rest.trim_start_matches(['\\', '/']);
        if let Some(end) = rel.find('"') {
            let rel = &rel[..end];
            if rel.to_ascii_lowercase().ends_with("cli.js") {
                return Some(lexical_normalize(&shim_dir.join(rel.replace('\\', "/"))));
            }
        }
    }
    None
}

/// What to run: `(program, leading args)`.
#[derive(Debug, PartialEq, Eq)]
pub struct PiCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
}

fn find_in_path(dirs: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    for d in dirs {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// `PI_SESSION_HISTORY_PI_CLI` (cli.js, run with node) -> `pi.exe` / `pi.cmd` / `pi` on PATH -> plain `pi`.
pub fn resolve_pi_command(env_cli: Option<&str>, path_var: Option<OsString>) -> PiCommand {
    let dirs: Vec<PathBuf> = path_var.map(|p| std::env::split_paths(&p).filter(|d| !d.as_os_str().is_empty()).collect()).unwrap_or_default();
    let node = || -> OsString {
        let names: &[&str] = if cfg!(windows) { &["node.exe"] } else { &["node"] };
        find_in_path(&dirs, names).map(PathBuf::into_os_string).unwrap_or_else(|| "node".into())
    };
    if let Some(cli) = env_cli.filter(|c| !c.is_empty() && Path::new(c).exists()) {
        return PiCommand { program: node(), args: vec![cli.into()] };
    }
    if cfg!(windows) {
        for d in &dirs {
            let exe = d.join("pi.exe");
            if exe.is_file() {
                return PiCommand { program: exe.into_os_string(), args: vec![] };
            }
            let shim = d.join("pi.cmd");
            if shim.is_file() {
                if let Some(cli) = std::fs::read_to_string(&shim).ok().and_then(|t| parse_pi_cmd_shim(&t, d)).filter(|c| c.is_file()) {
                    let local_node = d.join("node.exe");
                    let program = if local_node.is_file() { local_node.into_os_string() } else { node() };
                    return PiCommand { program, args: vec![cli.into_os_string()] };
                }
                // unknown shim layout: run the .cmd itself (Rust escapes arguments for cmd.exe)
                return PiCommand { program: shim.into_os_string(), args: vec![] };
            }
        }
    } else if let Some(bin) = find_in_path(&dirs, &["pi"]) {
        return PiCommand { program: bin.into_os_string(), args: vec![] };
    }
    PiCommand { program: "pi".into(), args: vec![] }
}

/* ------------------------------------------------------------------ running pi */

struct Semaphore {
    permits: Mutex<usize>,
    cv: Condvar,
}
impl Semaphore {
    fn acquire(&self) {
        let mut p = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        while *p == 0 {
            p = self.cv.wait(p).unwrap_or_else(|e| e.into_inner());
        }
        *p -= 1;
    }
    fn release(&self) {
        *self.permits.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_one();
    }
}

fn export_slots() -> &'static Semaphore {
    static S: OnceLock<Semaphore> = OnceLock::new();
    S.get_or_init(|| Semaphore { permits: Mutex::new(3), cv: Condvar::new() })
}

fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { s.to_string() } else { s.chars().skip(n - max).collect() }
}

fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        tail(&String::from_utf8_lossy(&buf), 2000)
    })
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        no_window(&mut c).args(["/T", "/F", "/PID", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
        let _ = c.status();
    }
    let _ = child.kill();
}

/// Default exporter: `pi --export <jsonl> <out>`.
pub fn export_with_pi(jsonl: &Path, out: &Path) -> Result<(), String> {
    let pc = resolve_pi_command(std::env::var("PI_SESSION_HISTORY_PI_CLI").ok().as_deref(), std::env::var_os("PATH"));
    let slots = export_slots();
    slots.acquire();
    let r = run_pi(&pc, jsonl, out);
    slots.release();
    r
}

fn run_pi(pc: &PiCommand, jsonl: &Path, out: &Path) -> Result<(), String> {
    let mut cmd = Command::new(&pc.program);
    no_window(&mut cmd)
        .args(&pc.args)
        .arg("--export")
        .arg(jsonl)
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not start pi: {e}"))?;
    let so = drain(child.stdout.take().ok_or("no stdout")?);
    let se = drain(child.stderr.take().ok_or("no stderr")?);
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if t0.elapsed() > EXPORT_TIMEOUT {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    return Err("pi --export timed out (120s)".into());
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(format!("could not start pi: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let (o, e) = (so.join().unwrap_or_default(), se.join().unwrap_or_default());
    let msg = if e.trim().is_empty() { o } else { e };
    let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into());
    Err(format!("pi --export exited with {code}: {}", msg.trim()))
}

/* ------------------------------------------------------------------ cache of generated html */

/// One slot per output file: concurrent callers wait for the running job and share its result.
#[derive(Default)]
struct Slot {
    last: Mutex<Option<Result<(), String>>>,
    finished: AtomicU64,
}

fn slots() -> &'static Mutex<HashMap<PathBuf, Arc<Slot>>> {
    static L: OnceLock<Mutex<HashMap<PathBuf, Arc<Slot>>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Generate (or reuse) `<html_dir>/<id>.html` for a session. Re-generated when the jsonl is newer.
/// Concurrent requests for the same session share one export.
pub fn ensure_session_html<F: Fn(&Path, &Path) -> Result<(), String> + ?Sized>(id: &str, jsonl: &Path, html_dir: &Path, exporter: &F) -> Result<PathBuf, String> {
    let out = html_dir.join(format!("{}.html", sanitize_id(id)));
    let slot = slots().lock().unwrap_or_else(|e| e.into_inner()).entry(out.clone()).or_default().clone();
    let before = slot.finished.load(Ordering::SeqCst);
    let result = {
        let mut last = slot.last.lock().unwrap_or_else(|e| e.into_inner());
        match last.clone() {
            // a job finished while we were waiting for the lock: share its result
            Some(r) if slot.finished.load(Ordering::SeqCst) != before => r,
            _ => {
                let r = generate(jsonl, html_dir, &out, exporter);
                *last = Some(r.clone());
                slot.finished.fetch_add(1, Ordering::SeqCst);
                r
            }
        }
    };
    let mut map = slots().lock().unwrap_or_else(|e| e.into_inner());
    drop(slot);
    if map.get(&out).map(Arc::strong_count) == Some(1) {
        map.remove(&out);
    }
    result.map(|_| out)
}

fn generate<F: Fn(&Path, &Path) -> Result<(), String> + ?Sized>(jsonl: &Path, html_dir: &Path, out: &Path, exporter: &F) -> Result<(), String> {
    let src = std::fs::metadata(jsonl).map_err(|e| e.to_string())?;
    if let Ok(o) = std::fs::metadata(out) {
        if let (Ok(om), Ok(sm)) = (o.modified(), src.modified()) {
            if o.len() > 0 && om >= sm {
                return Ok(());
            }
        }
    }
    std::fs::create_dir_all(html_dir).map_err(|e| e.to_string())?;
    let tmp = PathBuf::from(format!("{}.{}.tmp.html", out.display(), std::process::id()));
    let r = exporter(jsonl, &tmp).and_then(|_| {
        let len = std::fs::metadata(&tmp).map(|m| m.len()).map_err(|e| e.to_string())?;
        if len == 0 {
            return Err("export produced an empty file".to_string());
        }
        std::fs::rename(&tmp, out).map_err(|e| e.to_string())
    });
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::{jl, tmp_dir, write};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn parses_npm_cmd_shim() {
        let shim = "@SETLOCAL\r\n@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\" \"%~dp0\\..\\.mise\\pkg\\node_modules\\@x\\pi\\dist\\bundle\\cli.js\" %*\r\n) ELSE (\r\n  node \"%~dp0\\..\\.mise\\pkg\\node_modules\\@x\\pi\\dist\\bundle\\cli.js\" %*\r\n)";
        let dir = Path::new("/some/bin");
        let expected = lexical_normalize(&dir.join("../.mise/pkg/node_modules/@x/pi/dist/bundle/cli.js"));
        assert_eq!(parse_pi_cmd_shim(shim, dir), Some(expected));
        assert_eq!(parse_pi_cmd_shim("nothing", dir), None);
    }

    #[test]
    fn resolves_pi_cli_from_env() {
        let dir = tmp_dir("picli");
        let cli = dir.join("cli.js");
        std::fs::write(&cli, "").unwrap();
        let pc = resolve_pi_command(cli.to_str(), None);
        assert_eq!(pc.args, vec![OsString::from(&cli)]);
        let pc2 = resolve_pi_command(Some("/definitely/missing/cli.js"), None);
        assert_eq!(pc2, PiCommand { program: "pi".into(), args: vec![] });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ensure_html_generates_reuses_and_regenerates() {
        let dir = tmp_dir("html");
        let file = write(&dir, "p", "2026-10-05T00-00-00-000Z_h.jsonl", &jl(&[r#"{"type":"session","id":"h","timestamp":"2026-10-05T00:00:00.000Z"}"#.to_string()]));
        let html_dir = dir.join("html");
        let calls = AtomicUsize::new(0);
        let exporter = |src: &Path, out: &Path| -> Result<(), String> {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            assert_eq!(src, file.as_path());
            std::thread::sleep(Duration::from_millis(50));
            std::fs::write(out, format!("<html>run {n}</html>")).map_err(|e| e.to_string())
        };
        let out = ensure_session_html("h", &file, &html_dir, &exporter).unwrap();
        assert_eq!(out, html_dir.join("h.html"));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "<html>run 1</html>");
        ensure_session_html("h", &file, &html_dir, &exporter).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let set_future = |secs: u64| {
            let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
            f.set_modified(std::time::SystemTime::now() + Duration::from_secs(secs)).unwrap();
        };
        set_future(60);
        ensure_session_html("h", &file, &html_dir, &exporter).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // concurrent requests share one export
        set_future(120);
        std::thread::scope(|s| {
            let a = s.spawn(|| ensure_session_html("h", &file, &html_dir, &exporter));
            let b = s.spawn(|| ensure_session_html("h", &file, &html_dir, &exporter));
            a.join().unwrap().unwrap();
            b.join().unwrap().unwrap();
        });
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // empty output is an error and leaves no html behind
        let empty = |_: &Path, out: &Path| std::fs::write(out, "").map_err(|e| e.to_string());
        let err = ensure_session_html("h2", &file, &html_dir, &empty).unwrap_err();
        assert!(err.contains("empty"), "{err}");
        assert!(!html_dir.join("h2.html").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
