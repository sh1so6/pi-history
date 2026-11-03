//! agentDir の決定（`--agent-dir` → `PI_CODING_AGENT_DIR` → アプリ設定 app.json → 既定 `~/.pi/agent`）、
//! `sessions` フォルダの検証、フォルダ選択ダイアログのループ。ダイアログ本体は `Ui` trait の裏に隠す（テストで差し替え）。
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let v = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let v = std::env::var_os("HOME");
    v.filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn expand_tilde_with(p: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if p == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = p.strip_prefix("~/").or_else(|| if cfg!(windows) { p.strip_prefix("~\\") } else { None }) {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// アプリ設定ディレクトリ（app.json の置き場）。`PI_HISTORY_CONFIG_DIR` があればそれ（主にテスト用）。
/// Windows: `%APPDATA%\pi-history` / macOS: `~/Library/Application Support/pi-history` / その他: `$XDG_CONFIG_HOME` か `~/.config` の `pi-history`。
pub fn app_config_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("PI_HISTORY_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(v));
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").filter(|s| !s.is_empty()).map(|a| PathBuf::from(a).join("pi-history"))
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library").join("Application Support").join("pi-history"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".config")))
            .map(|c| c.join("pi-history"))
    }
}

pub fn app_json_path() -> Option<PathBuf> {
    app_config_dir().map(|d| d.join("app.json"))
}

/// app.json から agentDir を読む。無い・壊れている・文字列でない・空は `None`（無視）。
pub fn parse_app_json(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    match v.get("agentDir") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

pub fn render_app_json(agent_dir: &Path) -> String {
    serde_json::to_string_pretty(&json!({ "agentDir": agent_dir.to_string_lossy() })).unwrap_or_default() + "\n"
}

pub fn load_saved_agent_dir(path: &Path) -> Option<String> {
    parse_app_json(&std::fs::read_to_string(path).ok()?)
}

pub fn save_agent_dir(path: &Path, agent_dir: &Path) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, render_app_json(agent_dir))?;
    std::fs::rename(&tmp, path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Cli,
    Env,
    Saved,
    Default,
}

/// 決定順: cli → env → saved → `~/.pi/agent`。空文字は未指定扱い。
pub fn resolve_agent_dir(cli: Option<&str>, env: Option<&str>, saved: Option<&str>, home: Option<&Path>) -> (PathBuf, Source) {
    fn pick(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.trim().is_empty())
    }
    if let Some(p) = pick(cli) {
        return (expand_tilde_with(p, home), Source::Cli);
    }
    if let Some(p) = pick(env) {
        return (expand_tilde_with(p, home), Source::Env);
    }
    if let Some(p) = pick(saved) {
        return (expand_tilde_with(p, home), Source::Saved);
    }
    (home.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")).join(".pi").join("agent"), Source::Default)
}

/// 実環境（env・app.json・HOME）から決定する。
pub fn resolve_from_environment(cli: Option<&str>) -> (PathBuf, Source) {
    let env = std::env::var("PI_CODING_AGENT_DIR").ok();
    let saved = app_json_path().and_then(|p| load_saved_agent_dir(&p));
    resolve_agent_dir(cli, env.as_deref(), saved.as_deref(), home_dir().as_deref())
}

pub fn has_sessions(agent: &Path) -> bool {
    agent.join("sessions").is_dir()
}

/// 選ばれたフォルダから agentDir を決める。`sessions` 自体なら親、`sessions` を含むならそのフォルダ自身、それ以外は Err（表示用メッセージ）。
pub fn agent_dir_from_pick(picked: &Path) -> Result<PathBuf, String> {
    let is_sessions_name = picked.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("sessions")).unwrap_or(false);
    if is_sessions_name && picked.is_dir() {
        if let Some(parent) = picked.parent().filter(|p| !p.as_os_str().is_empty()) {
            return Ok(parent.to_path_buf());
        }
    }
    if has_sessions(picked) {
        return Ok(picked.to_path_buf());
    }
    Err(format!(
        "「{}」には sessions フォルダがありません。\nPi の agent フォルダ（sessions を含む。通常は ~/.pi/agent）か、sessions フォルダ自体を選んでください。",
        picked.display()
    ))
}

/// ダイアログ UI の差し替え点。
pub trait Ui {
    /// フォルダ選択。`None` はキャンセル。
    fn pick_folder(&mut self, title: &str, start_dir: Option<&Path>) -> Option<PathBuf>;
    /// エラー表示（メッセージボックス等）。
    fn error(&mut self, title: &str, msg: &str);
}

pub const PICK_TITLE: &str = "Pi の agent フォルダ（sessions を含む）を選択";
pub const APP_TITLE: &str = "pi-history";

/// ダイアログの初期フォルダ: `~/.pi` があればそれ、無ければ指定なし。
pub fn initial_dir(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join(".pi")).filter(|p| p.is_dir())
}

/// 選択ダイアログのループ。有効な選択（agentDir）を返す。キャンセルは `Err`。
/// 不正な選択はエラーを出して再選択させる。
pub fn pick_loop(ui: &mut dyn Ui, start: Option<&Path>) -> Result<PathBuf, String> {
    loop {
        match ui.pick_folder(PICK_TITLE, start) {
            None => return Err("フォルダの選択がキャンセルされました。".to_string()),
            Some(p) => match agent_dir_from_pick(&p) {
                Ok(a) => return Ok(a),
                Err(msg) => ui.error(APP_TITLE, &msg),
            },
        }
    }
}

pub struct Options {
    pub pick_dir: bool,
    /// ダイアログを出してよいか（`--no-dialog` / env で false）
    pub dialogs: bool,
}

/// agentDir を最終決定する（Open 系コマンド用）。
/// 決定順で得た agentDir に sessions があり `--pick-dir` でなければそのまま。
/// それ以外はダイアログで選ばせ、成功したら app.json（`save_path`）へ保存。
/// `ui` は `dialogs` が true のときだけ使う。
pub fn ensure_agent_dir(
    opts: &Options,
    resolved: (PathBuf, Source),
    save_path: Option<&Path>,
    home: Option<&Path>,
    ui: &mut dyn Ui,
) -> Result<PathBuf, String> {
    let (agent, _) = resolved;
    if !opts.pick_dir && has_sessions(&agent) {
        return Ok(agent);
    }
    let reason = if opts.pick_dir {
        "--pick-dir が指定されましたが、ダイアログが無効です（--no-dialog / PI_SESSION_HISTORY_NO_DIALOG / PI_SESSION_HISTORY_NO_OPEN）".to_string()
    } else {
        format!(
            "sessions フォルダが見つかりません: {}\n--agent-dir <path> か PI_CODING_AGENT_DIR で agent フォルダを指定するか、--pick-dir で選択してください。",
            agent.join("sessions").display()
        )
    };
    if !opts.dialogs {
        return Err(reason);
    }
    let start = initial_dir(home);
    let picked = pick_loop(ui, start.as_deref())?;
    if let Some(p) = save_path {
        if let Err(e) = save_agent_dir(p, &picked) {
            // 保存に失敗しても今回は続行できる
            eprintln!("history: 設定を保存できませんでした ({}): {e}", p.display());
        }
    }
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pi-history-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resolve_order() {
        let home = PathBuf::from("/h");
        let r = |c, e, s| resolve_agent_dir(c, e, s, Some(&home));
        assert_eq!(r(Some("/c"), Some("/e"), Some("/s")), (PathBuf::from("/c"), Source::Cli));
        assert_eq!(r(None, Some("/e"), Some("/s")), (PathBuf::from("/e"), Source::Env));
        assert_eq!(r(None, None, Some("/s")), (PathBuf::from("/s"), Source::Saved));
        assert_eq!(r(None, None, None), (home.join(".pi").join("agent"), Source::Default));
        // 空文字は未指定扱い
        assert_eq!(r(Some(""), Some(" "), Some("/s")).1, Source::Saved);
        // ~ 展開
        assert_eq!(r(Some("~/x"), None, None).0, home.join("x"));
        assert_eq!(r(None, Some("~"), None).0, home);
    }

    #[test]
    fn app_json_roundtrip_and_garbage() {
        let d = tmp("appjson");
        let p = d.join("sub").join("app.json");
        assert_eq!(load_saved_agent_dir(&p), None);
        save_agent_dir(&p, Path::new("C:\\a b\\agent")).unwrap();
        assert_eq!(load_saved_agent_dir(&p).as_deref(), Some("C:\\a b\\agent"));
        std::fs::write(&p, "{ broken").unwrap();
        assert_eq!(load_saved_agent_dir(&p), None);
        assert_eq!(parse_app_json("{\"agentDir\": 3}"), None);
        assert_eq!(parse_app_json("{\"agentDir\": \"\"}"), None);
        assert_eq!(parse_app_json("[]"), None);
        assert_eq!(parse_app_json("\u{feff}{\"agentDir\":\"/x\"}").as_deref(), Some("/x"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pick_parent_and_reject() {
        let d = tmp("pick");
        let agent = d.join("agent");
        std::fs::create_dir_all(agent.join("sessions")).unwrap();
        let empty = d.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(agent_dir_from_pick(&agent), Ok(agent.clone()));
        assert_eq!(agent_dir_from_pick(&agent.join("sessions")), Ok(agent.clone()));
        assert!(agent_dir_from_pick(&empty).is_err());
        assert!(agent_dir_from_pick(&d.join("nonexistent")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    struct Fake {
        picks: Vec<Option<PathBuf>>,
        errors: Vec<String>,
        starts: Vec<Option<PathBuf>>,
    }
    impl Ui for Fake {
        fn pick_folder(&mut self, _t: &str, start: Option<&Path>) -> Option<PathBuf> {
            self.starts.push(start.map(Path::to_path_buf));
            self.picks.remove(0)
        }
        fn error(&mut self, _t: &str, m: &str) {
            self.errors.push(m.to_string());
        }
    }

    #[test]
    fn pick_loop_retries_then_accepts_and_cancel() {
        let d = tmp("loop");
        let agent = d.join("agent");
        std::fs::create_dir_all(agent.join("sessions")).unwrap();
        let bad = d.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        let mut ui = Fake { picks: vec![Some(bad.clone()), Some(agent.join("sessions"))], errors: vec![], starts: vec![] };
        assert_eq!(pick_loop(&mut ui, None), Ok(agent.clone()));
        assert_eq!(ui.errors.len(), 1);
        let mut ui = Fake { picks: vec![Some(bad), None], errors: vec![], starts: vec![] };
        assert!(pick_loop(&mut ui, None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_flow() {
        let d = tmp("ensure");
        let good = d.join("good");
        std::fs::create_dir_all(good.join("sessions")).unwrap();
        let other = d.join("other");
        std::fs::create_dir_all(other.join("sessions")).unwrap();
        let missing = d.join("missing");
        let save = d.join("cfg").join("app.json");
        let fake = |p: Vec<Option<PathBuf>>| Fake { picks: p, errors: vec![], starts: vec![] };

        // sessions あり・pick なし → そのまま、ダイアログ不要、保存もしない
        let o = Options { pick_dir: false, dialogs: true };
        let mut ui = fake(vec![]);
        assert_eq!(ensure_agent_dir(&o, (good.clone(), Source::Env), Some(&save), None, &mut ui), Ok(good.clone()));
        assert!(!save.exists());

        // sessions なし → ダイアログで選択 → 保存
        let mut ui = fake(vec![Some(other.clone())]);
        assert_eq!(ensure_agent_dir(&o, (missing.clone(), Source::Default), Some(&save), None, &mut ui), Ok(other.clone()));
        assert_eq!(load_saved_agent_dir(&save).map(PathBuf::from), Some(other.clone()));

        // --pick-dir は保存済みでも再選択して上書き
        let o2 = Options { pick_dir: true, dialogs: true };
        let mut ui = fake(vec![Some(good.join("sessions"))]);
        assert_eq!(ensure_agent_dir(&o2, (other.clone(), Source::Saved), Some(&save), None, &mut ui), Ok(good.clone()));
        assert_eq!(load_saved_agent_dir(&save).map(PathBuf::from), Some(good.clone()));

        // キャンセル → Err、保存は変わらない
        let mut ui = fake(vec![None]);
        assert!(ensure_agent_dir(&o2, (good.clone(), Source::Saved), Some(&save), None, &mut ui).is_err());
        assert_eq!(load_saved_agent_dir(&save).map(PathBuf::from), Some(good.clone()));

        // ダイアログ無効 → Err、UI は呼ばれない（picks が空なら remove で panic する）
        let o3 = Options { pick_dir: false, dialogs: false };
        let mut ui = fake(vec![]);
        let e = ensure_agent_dir(&o3, (missing.clone(), Source::Default), Some(&save), None, &mut ui).unwrap_err();
        assert!(e.contains("sessions"));
        let o4 = Options { pick_dir: true, dialogs: false };
        assert!(ensure_agent_dir(&o4, (good.clone(), Source::Cli), Some(&save), None, &mut ui).is_err());

        // 初期フォルダ: ~/.pi があれば
        std::fs::create_dir_all(d.join("home").join(".pi")).unwrap();
        assert_eq!(initial_dir(Some(&d.join("home"))), Some(d.join("home").join(".pi")));
        assert_eq!(initial_dir(Some(&d)), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
