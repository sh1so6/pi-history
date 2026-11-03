//! Session-page font preferences, kept in `<agentDir>/session-history/ui.json` so they survive the
//! per-launch random port (browser `localStorage` is per origin). Everything is sanitized on the way
//! in, because the values end up inside a `<style>` block served with every session page.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const MAX_FAMILY_LEN: usize = 200;
pub const SIZE: (f64, f64) = (12.0, 24.0);
pub const LINE: (f64, f64) = (1.3, 2.2);
pub const MONO_SIZE: (f64, f64) = (10.0, 24.0);

/// All fields optional: a missing field means "keep the built-in look".
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono_preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono_size: Option<f64>,
}

/// A `font-family` list we are willing to put in CSS: letters/digits (any script), space, `, - _ .` and
/// balanced quotes. No `; { } < > ( ) \ / : @ !` or control characters, so it cannot leave the declaration.
pub fn sanitize_family(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > MAX_FAMILY_LEN {
        return None;
    }
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match c {
            '"' | '\'' => match quote {
                Some(q) if q == c => quote = None,
                None => quote = Some(c),
                _ => {}
            },
            c if c.is_alphanumeric() || matches!(c, ' ' | ',' | '-' | '_' | '.') => {}
            _ => return None,
        }
    }
    if quote.is_some() {
        return None;
    }
    Some(s.to_string())
}

fn num(v: Option<&Value>, (lo, hi): (f64, f64), step: f64) -> Option<f64> {
    let n = v?.as_f64()?;
    if !n.is_finite() || n < lo || n > hi {
        return None;
    }
    // snap to the step, then drop float noise (1.8000000000000003 -> 1.8)
    Some((((n / step).round() * step) * 100.0).round() / 100.0)
}

fn fam(v: Option<&Value>) -> Option<String> {
    sanitize_family(v?.as_str()?)
}

impl Prefs {
    /// Lenient: invalid or out-of-range fields are dropped, valid ones kept.
    pub fn from_value(v: &Value) -> Prefs {
        Prefs {
            font_preset: fam(v.get("fontPreset")),
            font: fam(v.get("font")),
            size: num(v.get("size"), SIZE, 0.5),
            line: num(v.get("line"), LINE, 0.05),
            mono_preset: fam(v.get("monoPreset")),
            mono: fam(v.get("mono")),
            mono_size: num(v.get("monoSize"), MONO_SIZE, 0.5),
        }
    }

    /// `:root{--ph-…}` for the served page, or `None` when nothing is customised. The free-text field wins over
    /// the preset. Our own default stack stays behind the chosen one as a fallback.
    pub fn css(&self) -> Option<String> {
        let mut d = Vec::new();
        let pick = |custom: &Option<String>, preset: &Option<String>| custom.clone().or_else(|| preset.clone());
        if let Some(f) = pick(&self.font, &self.font_preset) {
            d.push(format!("--ph-font:{f},var(--pi-sans)"));
        }
        if let Some(n) = self.size {
            d.push(format!("--ph-font-size:{n}px"));
        }
        if let Some(n) = self.line {
            d.push(format!("--ph-line:{n}"));
        }
        if let Some(f) = pick(&self.mono, &self.mono_preset) {
            d.push(format!("--ph-mono:{f},var(--pi-mono)"));
        }
        if let Some(n) = self.mono_size {
            d.push(format!("--ph-mono-size:{n}px"));
        }
        if d.is_empty() {
            None
        } else {
            Some(format!(":root{{{}}}", d.join(";")))
        }
    }
}

/// `None` = no (valid) ui.json. A file with no usable fields (`{}`, written by "reset") gives `Some(empty)`:
/// the server has spoken, so a browser must not resurrect an older localStorage copy.
pub fn load(path: &Path) -> Option<Prefs> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    v.is_object().then(|| Prefs::from_value(&v))
}

/// Atomic write (temp file + rename); `None` removes the file.
pub fn save(path: &Path, prefs: &Option<Prefs>) -> std::io::Result<()> {
    match prefs {
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Some(p) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
            std::fs::write(&tmp, serde_json::to_vec_pretty(p).unwrap_or_default())?;
            std::fs::rename(&tmp, path).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn family_allowlist() {
        assert_eq!(sanitize_family("  \"Yu Gothic UI\", 'Meiryo UI', sans-serif ").unwrap(), "\"Yu Gothic UI\", 'Meiryo UI', sans-serif");
        assert!(sanitize_family("游ゴシック, ＭＳ ゴシック").is_some());
        for bad in ["a;b", "a}b", "a{b", "</style><script>", "a(b)", "url(x)", "a\\b", "a/*b", "a:b", "a!important", "\"a", "'a\"b'x'", "a\nb", "", "   ", &"x".repeat(201)] {
            assert!(sanitize_family(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn numbers_clamped_to_range_or_dropped() {
        let p = Prefs::from_value(&json!({"size": 25, "line": 1.25, "monoSize": 13.26, "mono": "Consolas"}));
        assert_eq!((p.size, p.line, p.mono_size), (None, None, Some(13.5)));
        let p = Prefs::from_value(&json!({"size": "15", "line": null}));
        assert_eq!(p, Prefs::default());
        assert_eq!(Prefs::from_value(&json!({"size": 12, "line": 2.2})).css().unwrap(), ":root{--ph-font-size:12px;--ph-line:2.2}");
    }

    #[test]
    fn css_prefers_free_text_and_stays_inside_the_declaration() {
        let p = Prefs::from_value(&json!({"fontPreset": "\"Meiryo UI\", sans-serif", "font": "Foo Bar", "monoPreset": "Consolas", "size": 16, "line": 1.8, "monoSize": 14}));
        let css = p.css().unwrap();
        assert_eq!(css, ":root{--ph-font:Foo Bar,var(--pi-sans);--ph-font-size:16px;--ph-line:1.8;--ph-mono:Consolas,var(--pi-mono);--ph-mono-size:14px}");
        let evil = Prefs::from_value(&json!({"font": "x;}</style><script>alert(1)</script>", "mono": "a{b}", "fontPreset": "ok"}));
        assert_eq!(evil.css().unwrap(), ":root{--ph-font:ok,var(--pi-sans)}");
        assert!(Prefs::default().css().is_none());
    }

    #[test]
    fn save_load_roundtrip_and_remove() {
        let dir = std::env::temp_dir().join(format!("pi-history-prefs-{}", std::process::id()));
        let path = dir.join("ui.json");
        assert!(load(&path).is_none());
        let p = Prefs::from_value(&json!({"font": "Foo", "size": 17.5}));
        save(&path, &Some(p.clone())).unwrap();
        assert_eq!(load(&path), Some(p));
        save(&path, &Some(Prefs::default())).unwrap();
        assert_eq!(load(&path), Some(Prefs::default()));
        std::fs::write(&path, "[1]").unwrap();
        assert!(load(&path).is_none());
        std::fs::write(&path, r#"{"font":"a;b","evil":1,"size":16}"#).unwrap();
        assert_eq!(load(&path).unwrap(), Prefs::from_value(&json!({"size": 16})));
        save(&path, &None).unwrap();
        assert!(load(&path).is_none() && !path.exists());
        save(&path, &None).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
