//! Session index: jsonl parsing, cache (JSON-compatible with the TS extension), scanning.
use rayon::prelude::*;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const SUBAGENT_MARK: &str = "Temp-pi-agent-";
pub const SEARCH_TEXT_MAX: usize = 20000;
pub const FIRST_PROMPT_MAX: usize = 200;
const BIG_LINE: usize = 2000;
const CACHE_VERSION: u64 = 2;
/// How many parent hops to follow when folding a sub-agent session into its owner's project.
const MAX_PARENT_HOPS: usize = 10;

/* ------------------------------------------------------------------ pure helpers */

pub fn is_subagent_folder(folder: &str) -> bool {
    folder.contains(SUBAGENT_MARK)
}

pub fn project_name(cwd: Option<&str>, folder: &str) -> String {
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        let norm = cwd.replace('\\', "/");
        let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
        if !segs.is_empty() {
            return segs[segs.len().saturating_sub(2)..].join("/");
        }
    }
    let t = folder.trim_matches('-');
    if t.is_empty() { folder.to_string() } else { t.to_string() }
}

/// Characters JS treats as whitespace (`\s`, `String.prototype.trim`).
pub fn is_js_space(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// First `n` UTF-16 code units of `s` (never splits a surrogate pair: a pair that does not fit is dropped).
pub fn slice_utf16(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

fn len_utf16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

pub fn sanitize_id(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('_');
            }
        }
    }
    let dots = out.len() - out.trim_start_matches('.').len();
    if dots > 0 {
        out = format!("_{}", &out[dots..]);
    }
    out.truncate(128);
    out
}

/// `new Date(ms).toISOString()`
pub fn iso_from_ms(ms: f64) -> String {
    let total = ms.floor() as i64;
    let days = total.div_euclid(86_400_000);
    let rem = total.rem_euclid(86_400_000);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000
    )
}

pub fn now_iso() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
    iso_from_ms(ms)
}

/// Calendar date (`YYYY-MM-DD`) of an RFC 3339 timestamp in the given time zone.
pub fn date_in_tz<Tz: chrono::TimeZone>(ts: &str, tz: &Tz) -> Option<String> {
    use chrono::Datelike;
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(tz);
    Some(format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day()))
}

/// Local (machine time zone) calendar date of an ISO timestamp.
pub fn local_date(ts: &str) -> Option<String> {
    date_in_tz(ts, &chrono::Local)
}

/// `decodeURIComponent` (None on malformed input / invalid UTF-8).
pub fn decode_uri_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3)?;
            if !h.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `encodeURIComponent`
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/* ------------------------------------------------------------------ entry */

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionEntry {
    pub id: String,
    pub file: String,
    pub folder: String,
    pub project: String,
    pub cwd: String,
    pub started_at: String,
    pub ended_at: String,
    pub title: String,
    pub first_prompt: String,
    pub search_text: String,
    pub cost: f64,
    pub messages: u64,
    pub models: Vec<String>,
}

/// Spend of one session on one (local) day for one model. Stored in the cache, not in `/api/index` entries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DailyRow {
    pub date: String,
    pub model: String,
    pub cost: f64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub messages: u64,
}

/// Per-session data besides the list entry: the `parentSession` header field and the daily spend.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionSpend {
    pub parent_session: String,
    pub daily: Vec<DailyRow>,
}

/* ------------------------------------------------------------------ lenient line model */
// Mirrors the JS behaviour of "take the field if it has the expected type, ignore it otherwise".

mod lenient {
    use super::*;

    fn drain_seq<'de, A: SeqAccess<'de>>(mut a: A) -> Result<(), A::Error> {
        while a.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }
    fn drain_map<'de, A: MapAccess<'de>>(mut a: A) -> Result<(), A::Error> {
        while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(())
    }

    struct StrV;
    impl<'de> Visitor<'de> for StrV {
        type Value = Option<String>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("any") }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> { Ok(Some(v.to_owned())) }
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E> { Ok(Some(v)) }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(None) }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok(None) }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok(None) }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok(None) }
        fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(None) }
        fn visit_none<E>(self) -> Result<Self::Value, E> { Ok(None) }
        fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> { drain_seq(a).map(|_| None) }
        fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> { drain_map(a).map(|_| None) }
    }
    pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        d.deserialize_any(StrV)
    }

    struct NumV;
    impl<'de> Visitor<'de> for NumV {
        type Value = Option<f64>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("any") }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> { Ok(None) }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(None) }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> { Ok(Some(v as f64)) }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> { Ok(Some(v as f64)) }
        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> { Ok(Some(v)) }
        fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(None) }
        fn visit_none<E>(self) -> Result<Self::Value, E> { Ok(None) }
        fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> { drain_seq(a).map(|_| None) }
        fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> { drain_map(a).map(|_| None) }
    }
    pub fn number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        d.deserialize_any(NumV)
    }

    /// An object field: `Some(T)` when the JSON value is an object, `None` for anything else.
    pub struct Obj<T>(pub Option<T>);
    impl<T> Default for Obj<T> {
        fn default() -> Self { Obj(None) }
    }
    struct ObjV<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjV<T> {
        type Value = Obj<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("any") }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_none<E>(self) -> Result<Self::Value, E> { Ok(Obj(None)) }
        fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> { drain_seq(a).map(|_| Obj(None)) }
        fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
            T::deserialize(de::value::MapAccessDeserializer::new(a)).map(|t| Obj(Some(t)))
        }
    }
    impl<'de, T: Deserialize<'de>> Deserialize<'de> for Obj<T> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(ObjV(std::marker::PhantomData))
        }
    }

    /// `message.content`: a string, or an array of parts (we keep the non-empty `text` of `{type:"text"}` parts).
    #[derive(Default)]
    pub struct Content(pub Vec<String>);
    #[derive(Deserialize, Default)]
    struct Part {
        #[serde(rename = "type", default, deserialize_with = "string")]
        ty: Option<String>,
        #[serde(default, deserialize_with = "string")]
        text: Option<String>,
    }
    struct ContentV;
    impl<'de> Visitor<'de> for ContentV {
        type Value = Content;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("any") }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> { Ok(Content(vec![v.to_owned()])) }
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E> { Ok(Content(vec![v])) }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> { Ok(Content(vec![])) }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> { Ok(Content(vec![])) }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> { Ok(Content(vec![])) }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> { Ok(Content(vec![])) }
        fn visit_unit<E>(self) -> Result<Self::Value, E> { Ok(Content(vec![])) }
        fn visit_none<E>(self) -> Result<Self::Value, E> { Ok(Content(vec![])) }
        fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> { drain_map(a).map(|_| Content(vec![])) }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let mut parts = Vec::new();
            while let Some(Obj(p)) = a.next_element::<Obj<Part>>()? {
                if let Some(Part { ty: Some(t), text: Some(x) }) = p {
                    if t == "text" && !x.is_empty() {
                        parts.push(x);
                    }
                }
            }
            Ok(Content(parts))
        }
    }
    impl<'de> Deserialize<'de> for Content {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(ContentV)
        }
    }
}
use lenient::{Content, Obj};

#[derive(Deserialize, Default)]
struct Cost {
    #[serde(default, deserialize_with = "lenient::number")]
    total: Option<f64>,
}
#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    cost: Obj<Cost>,
    #[serde(default, deserialize_with = "lenient::number")]
    input: Option<f64>,
    #[serde(default, deserialize_with = "lenient::number")]
    output: Option<f64>,
    #[serde(default, rename = "cacheRead", deserialize_with = "lenient::number")]
    cache_read: Option<f64>,
    #[serde(default, rename = "cacheWrite", deserialize_with = "lenient::number")]
    cache_write: Option<f64>,
}
fn tokens(v: Option<f64>) -> u64 {
    match v {
        Some(f) if f.is_finite() && f > 0.0 => f.round() as u64,
        _ => 0,
    }
}
impl Usage {
    fn total(&self) -> f64 {
        num(self.cost.0.as_ref().and_then(|c| c.total))
    }
}
#[derive(Deserialize, Default)]
struct Message {
    #[serde(default, deserialize_with = "lenient::string")]
    role: Option<String>,
    #[serde(default)]
    content: Content,
    #[serde(default, deserialize_with = "lenient::string")]
    model: Option<String>,
    #[serde(default)]
    usage: Obj<Usage>,
}
#[derive(Deserialize, Default)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    ty: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    timestamp: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    title: Option<String>,
    #[serde(default, rename = "parentSession", deserialize_with = "lenient::string")]
    parent_session: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    model: Option<String>,
    #[serde(default)]
    usage: Obj<Usage>,
    #[serde(default)]
    message: Obj<Message>,
}

fn num(v: Option<f64>) -> f64 {
    match v {
        Some(f) if f.is_finite() => f,
        _ => 0.0,
    }
}

/// Typed fast path; on failure (duplicate keys etc.) re-parse via `Value` (last key wins, like JS).
fn parse_line(text: &str) -> Option<Line> {
    match serde_json::from_str::<Line>(text) {
        Ok(l) => Some(l),
        Err(_) => {
            let v: serde_json::Value = serde_json::from_str(text).ok()?;
            Line::deserialize(v).ok()
        }
    }
}

/* ------------------------------------------------------------------ prefix peeking for huge lines */

fn peek_type(head: &str) -> Option<&str> {
    let rest = head.strip_prefix("{\"type\":\"")?;
    let end = rest.find(|c: char| !(c.is_ascii_alphabetic() || c == '_'))?;
    if end > 0 && rest[end..].starts_with('"') { Some(&rest[..end]) } else { None }
}

fn peek_role(head: &str) -> Option<&str> {
    const KEY: &str = "\"message\":{\"role\":\"";
    for (i, _) in head.match_indices(KEY) {
        let rest = &head[i + KEY.len()..];
        if let Some(end) = rest.find(|c: char| !c.is_ascii_alphabetic()) {
            if end > 0 && rest[end..].starts_with('"') {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

fn peek_timestamp(head: &str) -> Option<&str> {
    const KEY: &str = "\"timestamp\":\"";
    for (i, _) in head.match_indices(KEY) {
        let rest = &head[i + KEY.len()..];
        if let Some(end) = rest.find('"') {
            if end > 0 {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

/* ------------------------------------------------------------------ accumulator */

#[derive(Default)]
pub struct SessionAccumulator {
    id: String,
    started_at: String,
    cwd: String,
    ended_at: String,
    name: String,
    first_prompt: String,
    cost: f64,
    messages: u64,
    texts: String,
    text_count: usize,
    text_len: usize,
    models: Vec<String>,
    saw_any: bool,
    parent_session: String,
    daily: HashMap<(String, String), DailyRow>,
}

impl SessionAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add to the (local date, model) bucket. The date comes from the line's timestamp (else the previous line's / the header's).
    fn add_spend(&mut self, model: Option<&str>, u: Option<&Usage>, message: bool) {
        let ts = if !self.ended_at.is_empty() { &self.ended_at } else { &self.started_at };
        let date = local_date(ts).unwrap_or_default(); // "" is re-dated in finish()
        let model = model.filter(|m| !m.is_empty()).unwrap_or("unknown").to_string();
        let row = self.daily.entry((date.clone(), model.clone())).or_insert_with(|| DailyRow { date, model, ..Default::default() });
        if message {
            row.messages += 1;
        }
        if let Some(u) = u {
            row.cost += u.total();
            row.input += tokens(u.input);
            row.output += tokens(u.output);
            row.cache_read += tokens(u.cache_read);
            row.cache_write += tokens(u.cache_write);
        }
    }

    /// Feed one raw line (without the trailing `\n`; a trailing `\r` is tolerated).
    pub fn add_line(&mut self, raw: &[u8]) {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.first() != Some(&b'{') {
            return;
        }
        if line.len() > BIG_LINE {
            let head = String::from_utf8_lossy(&line[..400]);
            if let Some(ty) = peek_type(&head) {
                if ty != "session" && ty != "session_info" && ty != "usage" {
                    let skip = if ty == "message" {
                        matches!(peek_role(&head), Some(r) if r != "user" && r != "assistant")
                    } else {
                        true
                    };
                    if skip {
                        // not interesting: only peek the timestamp, never JSON-parse megabytes of tool output
                        if let Some(ts) = peek_timestamp(&head) {
                            self.ended_at = ts.to_string();
                            self.saw_any = true;
                        }
                        return;
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(line);
        let Some(obj) = parse_line(&text) else { return };
        let Some(ty) = obj.ty.as_deref() else { return };
        self.saw_any = true;
        if let Some(ts) = &obj.timestamp {
            self.ended_at = ts.clone();
        }
        match ty {
            "session" => {
                if let Some(id) = &obj.id {
                    if self.id.is_empty() {
                        self.id = id.clone();
                    }
                }
                if let Some(ts) = &obj.timestamp {
                    if self.started_at.is_empty() {
                        self.started_at = ts.clone();
                    }
                }
                if let Some(cwd) = &obj.cwd {
                    if self.cwd.is_empty() {
                        self.cwd = cwd.clone();
                    }
                }
                if let Some(p) = &obj.parent_session {
                    if self.parent_session.is_empty() {
                        self.parent_session = p.clone();
                    }
                }
            }
            "session_info" => {
                let n = obj.name.as_deref().or(obj.title.as_deref()).unwrap_or("");
                let n = js_trim(n);
                if !n.is_empty() {
                    self.name = n.to_string();
                }
            }
            "usage" => {
                if let Some(u) = &obj.usage.0 {
                    self.cost += u.total();
                    self.add_spend(obj.model.as_deref(), Some(u), false);
                }
            }
            "message" => {
                let Some(m) = &obj.message.0 else { return };
                match m.role.as_deref() {
                    Some("user") => {
                        self.messages += 1;
                        let joined = m.content.0.join("\n");
                        let t = js_trim(&joined);
                        if !t.is_empty() {
                            if self.first_prompt.is_empty() {
                                let mut collapsed = String::new();
                                let mut in_ws = false;
                                for c in t.chars() {
                                    if is_js_space(c) {
                                        if !in_ws {
                                            collapsed.push(' ');
                                        }
                                        in_ws = true;
                                    } else {
                                        collapsed.push(c);
                                        in_ws = false;
                                    }
                                }
                                self.first_prompt = slice_utf16(&collapsed, FIRST_PROMPT_MAX).to_string();
                            }
                            if self.text_len < SEARCH_TEXT_MAX {
                                let room = SEARCH_TEXT_MAX - self.text_len;
                                let piece = slice_utf16(t, room);
                                if self.text_count > 0 {
                                    self.texts.push('\n');
                                }
                                self.texts.push_str(piece);
                                self.text_count += 1;
                                self.text_len += len_utf16(piece) + 1;
                            }
                        }
                    }
                    Some("assistant") => {
                        self.messages += 1;
                        if let Some(u) = &m.usage.0 {
                            self.cost += u.total();
                        }
                        self.add_spend(m.model.as_deref(), m.usage.0.as_ref(), true);
                        if let Some(model) = &m.model {
                            if !model.is_empty() && !self.models.contains(model) {
                                self.models.push(model.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    pub fn finish(self, file: &str, folder: &str, mtime_ms: f64) -> Option<(SessionEntry, SessionSpend)> {
        if !self.saw_any {
            return None;
        }
        let name = Path::new(file).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let base = if name.len() >= 6 && name.is_char_boundary(name.len() - 6) && name[name.len() - 6..].eq_ignore_ascii_case(".jsonl") {
            &name[..name.len() - 6]
        } else {
            &name[..]
        };
        let file_id = match base.rfind('_') {
            Some(i) if i + 1 < base.len() => &base[i + 1..],
            _ => base,
        };
        let started_at = if !self.started_at.is_empty() {
            self.started_at.clone()
        } else if let Some(s) = iso_from_file_name(base) {
            s
        } else {
            iso_from_ms(mtime_ms)
        };
        let id = sanitize_id(if self.id.is_empty() { file_id } else { &self.id });
        let mut by_key: BTreeMap<(String, String), DailyRow> = BTreeMap::new();
        for (_, mut row) in self.daily {
            if row.date.is_empty() {
                row.date = local_date(&started_at).unwrap_or_default();
            }
            let key = (row.date.clone(), row.model.clone());
            match by_key.get_mut(&key) {
                Some(t) => {
                    t.cost += row.cost;
                    t.input += row.input;
                    t.output += row.output;
                    t.cache_read += row.cache_read;
                    t.cache_write += row.cache_write;
                    t.messages += row.messages;
                }
                None => {
                    by_key.insert(key, row);
                }
            }
        }
        let spend = SessionSpend { parent_session: self.parent_session, daily: by_key.into_values().collect() };
        Some((SessionEntry {
            id: if id.is_empty() { "session".to_string() } else { id },
            file: file.to_string(),
            folder: folder.to_string(),
            project: project_name(Some(&self.cwd), folder),
            cwd: self.cwd.clone(),
            ended_at: if self.ended_at.is_empty() { started_at.clone() } else { self.ended_at.clone() },
            started_at,
            title: if self.name.is_empty() { self.first_prompt.clone() } else { self.name.clone() },
            first_prompt: self.first_prompt,
            search_text: slice_utf16(&self.texts, SEARCH_TEXT_MAX).to_string(),
            cost: self.cost,
            messages: self.messages,
            models: self.models,
        }, spend))
    }
}

/// `2026-10-02T03-04-05-678Z_x` -> `2026-10-02T03:04:05.678Z`
fn iso_from_file_name(base: &str) -> Option<String> {
    let b = base.as_bytes();
    if b.len() < 24 {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let ok = digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10) && b[10] == b'T'
        && digits(11..13) && b[13] == b'-' && digits(14..16) && b[16] == b'-' && digits(17..19) && b[19] == b'-'
        && digits(20..23) && b[23] == b'Z';
    if !ok {
        return None;
    }
    Some(format!("{}T{}:{}:{}.{}Z", &base[0..10], &base[11..13], &base[14..16], &base[17..19], &base[20..23]))
}

/// Stream a file line by line (linear time even for multi-MB lines).
pub fn for_each_line(file: &Path, mut f: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, File::open(file)?);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        f(&buf);
        if buf.capacity() > (64 << 20) {
            buf = Vec::new();
        }
    }
    Ok(())
}

/// Parse one session jsonl into an index entry (None when unreadable / not a session).
#[cfg(test)]
pub fn parse_session_file(file: &Path, folder: &str, mtime_ms: f64) -> Option<SessionEntry> {
    parse_session_full(file, folder, mtime_ms).map(|(e, _)| e)
}

/// Like `parse_session_file`, plus the per-session spend data.
pub fn parse_session_full(file: &Path, folder: &str, mtime_ms: f64) -> Option<(SessionEntry, SessionSpend)> {
    let mut acc = SessionAccumulator::new();
    for_each_line(file, |l| acc.add_line(l)).ok()?;
    acc.finish(&file.to_string_lossy(), folder, mtime_ms)
}

/* ------------------------------------------------------------------ scan + cache */

#[derive(Serialize, Deserialize, Clone)]
struct CachedFile {
    #[serde(rename = "mtimeMs")]
    mtime_ms: f64,
    size: u64,
    entry: SessionEntry,
    #[serde(rename = "parentSession", default, skip_serializing_if = "String::is_empty")]
    parent_session: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    daily: Vec<DailyRow>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u64,
    files: HashMap<String, CachedFile>,
}

fn load_cache(path: &Path) -> CacheFile {
    let empty = || CacheFile { version: CACHE_VERSION, files: HashMap::new() };
    let Ok(f) = File::open(path) else { return empty() };
    match serde_json::from_reader::<_, CacheFile>(BufReader::with_capacity(1 << 20, f)) {
        Ok(c) if c.version == CACHE_VERSION => c,
        _ => empty(),
    }
}

fn save_cache(path: &Path, cache: &CacheFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        serde_json::to_writer(&mut w, cache)?;
        w.flush()?;
    }
    let mut last = Ok(());
    for _ in 0..5 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Err(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    last
}

pub fn mtime_ms(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1e6)
        .unwrap_or(0.0)
}

pub struct ScanOptions<'a> {
    pub sessions_dir: &'a Path,
    pub cache_path: &'a Path,
    pub include_subagents: bool,
    pub on_progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ScanResult {
    /// Sessions for the list (sub-agent sessions only when `include_subagents`).
    pub entries: Vec<SessionEntry>,
    /// Spend of all sessions, sub-agents included.
    pub spend: Vec<SpendRow>,
    pub total: usize,
    pub parsed: usize,
    pub cached: usize,
    pub skipped_folders: usize,
    pub ms: u128,
}

struct FoundFile {
    path: PathBuf,
    key: String,
    folder: String,
    mtime_ms: f64,
    size: u64,
}

/// One scanned session: list entry + spend data (fresh parse or cache hit).
pub struct Scanned {
    pub entry: SessionEntry,
    pub spend: SessionSpend,
}

pub fn scan_sessions(opts: &ScanOptions) -> ScanResult {
    let t0 = Instant::now();
    let cache = load_cache(opts.cache_path);
    let mut files: Vec<FoundFile> = Vec::new();
    let mut skipped_folders = 0;
    let mut kept: HashMap<String, CachedFile> = HashMap::new();
    let mut folders: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(opts.sessions_dir) {
        for d in rd.flatten() {
            if d.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                folders.push(d.file_name().to_string_lossy().into_owned());
            }
        }
    }
    folders.sort();
    // Sub-agent folders are always scanned: their spend is folded into the owning project even when
    // the list hides them (`include_subagents == false`).
    for folder in &folders {
        if !opts.include_subagents && is_subagent_folder(folder) {
            skipped_folders += 1;
        }
        let dir = opts.sessions_dir.join(folder);
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for d in rd.flatten() {
            let name = d.file_name().to_string_lossy().into_owned();
            if !name.to_ascii_lowercase().ends_with(".jsonl") {
                continue;
            }
            let path = dir.join(&name);
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() {
                    files.push(FoundFile {
                        key: path.to_string_lossy().into_owned(),
                        path,
                        folder: folder.clone(),
                        mtime_ms: mtime_ms(&meta),
                        size: meta.len(),
                    });
                }
            }
        }
    }
    let total = files.len();
    let mut all: Vec<Scanned> = Vec::with_capacity(total);
    let mut to_parse: Vec<&FoundFile> = Vec::new();
    for f in &files {
        match cache.files.get(&f.key) {
            Some(c) if (c.mtime_ms - f.mtime_ms).abs() < 0.5 && c.size == f.size => {
                kept.insert(f.key.clone(), c.clone());
                let mut e = c.entry.clone();
                e.file = f.key.clone();
                e.folder = f.folder.clone();
                all.push(Scanned { entry: e, spend: SessionSpend { parent_session: c.parent_session.clone(), daily: c.daily.clone() } });
            }
            _ => to_parse.push(f),
        }
    }
    let cached = all.len();
    to_parse.sort_by(|a, b| b.size.cmp(&a.size)); // big files first: better load balancing
    let done = AtomicUsize::new(cached);
    let results: Vec<(&FoundFile, Option<(SessionEntry, SessionSpend)>)> = to_parse
        .par_iter()
        .map(|f| {
            let e = parse_session_full(&f.path, &f.folder, f.mtime_ms);
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(cb) = opts.on_progress {
                cb(d, total);
            }
            (*f, e)
        })
        .collect();
    let parsed = results.len();
    for (f, e) in results {
        if let Some((e, sp)) = e {
            kept.insert(
                f.key.clone(),
                CachedFile { mtime_ms: f.mtime_ms, size: f.size, entry: e.clone(), parent_session: sp.parent_session.clone(), daily: sp.daily.clone() },
            );
            all.push(Scanned { entry: e, spend: sp });
        }
    }
    if parsed > 0 || kept.len() != cache.files.len() {
        let _ = save_cache(opts.cache_path, &CacheFile { version: CACHE_VERSION, files: kept });
    }
    let spend = build_spend(&all);
    let entries: Vec<SessionEntry> = all
        .into_iter()
        .filter(|s| opts.include_subagents || !is_subagent_folder(&s.entry.folder))
        .map(|s| s.entry)
        .collect();
    ScanResult { entries, spend, total, parsed, cached, skipped_folders, ms: t0.elapsed().as_millis() }
}

/* ------------------------------------------------------------------ spend */

/// One aggregated row of `/api/index`'s `spend`: (date, project, model, sub) -> sums.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendRow {
    pub date: String,
    pub project: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
    pub messages: u64,
    pub sub: bool,
}

/// Path key for matching `parentSession` against session files (separator / Windows case insensitive).
fn path_key(p: &str) -> String {
    let n = p.replace('\\', "/");
    if cfg!(windows) { n.to_lowercase() } else { n }
}

/// Aggregate the daily spend of ALL sessions (sub-agents included). A sub-agent session counts for the first
/// non-sub-agent session found by following `parentSession` (at most `MAX_PARENT_HOPS` hops, cycle-safe);
/// when the chain breaks it stays under its own project. Normal sessions always count for themselves.
pub fn build_spend(all: &[Scanned]) -> Vec<SpendRow> {
    let by_file: HashMap<String, usize> = all.iter().enumerate().map(|(i, s)| (path_key(&s.entry.file), i)).collect();
    let is_sub = |i: usize| is_subagent_folder(&all[i].entry.folder);
    let owner_project = |start: usize| -> &str {
        let own = all[start].entry.project.as_str();
        if !is_sub(start) {
            return own;
        }
        let mut seen: HashSet<usize> = HashSet::new();
        let mut cur = start;
        for _ in 0..MAX_PARENT_HOPS {
            seen.insert(cur);
            let parent = &all[cur].spend.parent_session;
            let Some(&p) = (!parent.is_empty()).then(|| by_file.get(&path_key(parent))).flatten() else { return own };
            if seen.contains(&p) {
                return own;
            }
            if !is_sub(p) {
                return all[p].entry.project.as_str();
            }
            cur = p;
        }
        own
    };
    let mut agg: BTreeMap<(String, String, String, bool), SpendRow> = BTreeMap::new();
    for (i, s) in all.iter().enumerate() {
        if s.spend.daily.is_empty() {
            continue;
        }
        let project = owner_project(i);
        let sub = is_sub(i);
        for d in &s.spend.daily {
            let row = agg
                .entry((d.date.clone(), project.to_string(), d.model.clone(), sub))
                .or_insert_with(|| SpendRow { date: d.date.clone(), project: project.to_string(), model: d.model.clone(), sub, ..Default::default() });
            row.input += d.input;
            row.output += d.output;
            row.cache_read += d.cache_read;
            row.cache_write += d.cache_write;
            row.cost += d.cost;
            row.messages += d.messages;
        }
    }
    agg.into_values().collect()
}

/* ------------------------------------------------------------------ index */

pub struct Index {
    pub entries: Vec<SessionEntry>,
    by_id: HashMap<String, usize>,
}

impl Index {
    /// Only ids that exist in the index resolve; anything else (traversal attempts included) is rejected.
    pub fn resolve(&self, raw_id: &str) -> Option<&SessionEntry> {
        let id = decode_uri_component(raw_id)?;
        if id.is_empty() || id.encode_utf16().count() > 200 || id.contains(['\\', '/', '\0']) || id.contains("..") {
            return None;
        }
        self.by_id.get(&id).map(|&i| &self.entries[i])
    }
}

/// Make ids unique (the same session id can live in several files) and index them.
pub fn build_index(entries: Vec<SessionEntry>) -> Index {
    let mut sorted = entries;
    sorted.sort_by(|a, b| a.file.cmp(&b.file));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &mut sorted {
        let base = e.id.clone();
        let mut id = base.clone();
        let mut n = 2;
        while seen.contains(&id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        seen.insert(id.clone());
        e.id = id;
    }
    sorted.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    let by_id = sorted.iter().enumerate().map(|(i, e)| (e.id.clone(), i)).collect();
    Index { entries: sorted, by_id }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    pub fn tmp_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("pi-history-test-{}-{}-{}", std::process::id(), tag, N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn write(dir: &Path, folder: &str, name: &str, content: &str) -> PathBuf {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
        let f = dir.join(folder).join(name);
        std::fs::write(&f, content).unwrap();
        f
    }

    pub fn jl(objs: &[String]) -> String {
        objs.join("\n") + "\n"
    }
    pub fn user_msg(text: &str, ts: &str) -> String {
        serde_json::json!({"type":"message","id":format!("u{ts}"),"timestamp":ts,"message":{"role":"user","content":text}}).to_string()
    }
    fn asst_msg(cost: f64, ts: &str, model: &str) -> String {
        serde_json::json!({"type":"message","id":format!("a{ts}"),"timestamp":ts,"message":{"role":"assistant","model":model,"content":[{"type":"text","text":"ok"}],"usage":{"cost":{"total":cost}}}}).to_string()
    }
    fn s(v: &str) -> String {
        v.to_string()
    }

    #[test]
    fn subagent_folder_and_project_name() {
        assert!(is_subagent_folder("--C--Users-9808g-AppData-Local-Temp-pi-agent-03af2515-54c5--"));
        assert!(!is_subagent_folder("--C--Users-9808g-Dev-Pi--"));
        assert_eq!(project_name(Some("G:\\Dev\\TS\\Pi"), "x"), "TS/Pi");
        assert_eq!(project_name(Some("C:/a/b/c/"), "x"), "b/c");
        assert_eq!(project_name(Some("/home"), "x"), "home");
        assert_eq!(project_name(None, "--C--Users-x--"), "C--Users-x");
        assert_eq!(project_name(Some(""), "--"), "--");
    }

    #[test]
    fn sanitize_id_prevents_path_chars() {
        assert_eq!(sanitize_id("a/b"), "a_b");
        let s = sanitize_id("../../x\\y:z");
        assert!(!s.contains(['\\', '/', ':']));
        assert!(!sanitize_id("../x").starts_with('.'));
        assert_eq!(sanitize_id("a😀"), "a__");
    }

    #[test]
    fn iso_and_uri() {
        assert_eq!(iso_from_ms(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(951_782_400_000.0), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_700_000_000_123.9), "2023-11-14T22:13:20.123Z");
        assert_eq!(decode_uri_component("a%2Fb").as_deref(), Some("a/b"));
        assert_eq!(decode_uri_component("%E0%A4%A"), None);
        assert_eq!(decode_uri_component("%ff"), None);
        assert_eq!(decode_uri_component("%+1"), None);
        assert_eq!(encode_uri_component("w/x y"), "w%2Fx%20y");
    }

    #[test]
    fn utf16_slicing() {
        assert_eq!(slice_utf16("あいう", 2), "あい");
        assert_eq!(slice_utf16("a😀b", 2), "a"); // the pair does not fit -> dropped
        assert_eq!(slice_utf16("a😀b", 3), "a😀");
        assert_eq!(len_utf16("a😀"), 3);
    }

    #[test]
    fn parse_session_file_full() {
        let dir = tmp_dir("parse");
        let big = "x".repeat(5000);
        let lines = vec![
            r#"{"type":"session","version":3,"id":"abc-123","timestamp":"2026-10-31T10:00:00.000Z","cwd":"G:\\Dev\\TS\\Pi"}"#.to_string(),
            s(r#"{"type":"model_change","id":"m","timestamp":"2026-10-31T10:00:01.000Z"}"#),
            s(r#"{"type":"message","broken"#),
            s("not json at all"),
            user_msg("  Hello\n  World  ", "2026-10-31T10:00:02.000Z"),
            asst_msg(0.5, "2026-10-31T10:00:03.000Z", "model-a"),
            s(r#"{"type":"message","id":"u2","timestamp":"2026-10-31T10:00:04.000Z","message":{"role":"user","content":[{"type":"text","text":"second "},{"type":"image","data":"zz"},{"type":"text","text":"part"}]}}"#),
            asst_msg(0.25, "2026-10-31T10:00:05.000Z", "model-b"),
            asst_msg(0.25, "2026-10-31T10:00:06.000Z", "model-a"),
            format!(r#"{{"type":"message","id":"t","parentId":"x","timestamp":"2026-10-31T10:00:07.000Z","message":{{"role":"toolResult","content":"{big}"}}}}"#),
            format!(r#"{{"type":"message","id":"s","timestamp":"2026-10-31T10:00:08.000Z","message":{{"role":"system","content":"{big}"}}}}"#),
            serde_json::json!({"type":"message","id":"ab","timestamp":"2026-10-31T10:00:09.000Z","message":{"role":"assistant","model":"model-c","content":[{"type":"text","text":big}],"usage":{"cost":{"total":1}}}}).to_string(),
            s(r#"{"type":"usage","id":"us","timestamp":"2026-10-31T10:00:10.000Z","kind":"cache_warm","usage":{"cost":{"total":0.125}}}"#),
            s(r#"{"type":"session_info","id":"si","timestamp":"2026-10-31T10:00:11.000Z","name":"My Title"}"#),
            format!(r#"{{"type":"custom","id":"c","timestamp":"2026-10-31T10:30:00.000Z","data":"{big}"}}"#),
            s(r#"{"type":"message","id":"cut"#),
        ];
        let file = write(&dir, "--proj--", "2026-10-31T10-00-00-000Z_abc-123.jsonl", &jl(&lines));
        let e = parse_session_file(&file, "--proj--", 0.0).unwrap();
        assert_eq!(e.id, "abc-123");
        assert_eq!(e.folder, "--proj--");
        assert_eq!(e.project, "TS/Pi");
        assert_eq!(e.cwd, "G:\\Dev\\TS\\Pi");
        assert_eq!(e.started_at, "2026-10-31T10:00:00.000Z");
        assert_eq!(e.ended_at, "2026-10-31T10:30:00.000Z");
        assert_eq!(e.first_prompt, "Hello World");
        assert_eq!(e.search_text, "Hello\n  World\nsecond \npart");
        assert_eq!(e.title, "My Title");
        assert_eq!(e.messages, 6);
        assert!((e.cost - 2.125).abs() < 1e-9, "{}", e.cost);
        assert_eq!(e.models, vec!["model-a", "model-b", "model-c"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parse_session_file_edge_cases() {
        let dir = tmp_dir("edge");
        let long = "あ".repeat(300);
        let mut lines = vec![
            s(r#"{"type":"session","id":"idx","timestamp":"2026-10-01T00:00:00.000Z"}"#),
            user_msg(&long, "2026-10-01T00:00:01.000Z"),
        ];
        for i in 0..50 {
            lines.push(user_msg(&format!("{}{}", "y".repeat(1000), i), &format!("2026-10-01T00:00:{:02}.000Z", i)));
        }
        let f1 = write(&dir, "p", "2026-10-01T00-00-00-000Z_idx.jsonl", &jl(&lines));
        let e1 = parse_session_file(&f1, "p", 0.0).unwrap();
        assert_eq!(e1.first_prompt.encode_utf16().count(), 200);
        assert_eq!(e1.title, e1.first_prompt);
        assert_eq!(e1.search_text.encode_utf16().count(), 20000);
        assert_eq!(e1.project, "p");
        // no header: id / startedAt derive from the file name
        let f2 = write(&dir, "p", "2026-10-02T03-04-05-678Z_deadbeef.jsonl", &jl(&[user_msg("hi", "2026-10-02T03:04:06.000Z")]));
        let e2 = parse_session_file(&f2, "p", 0.0).unwrap();
        assert_eq!(e2.id, "deadbeef");
        assert_eq!(e2.started_at, "2026-10-02T03:04:05.678Z");
        assert_eq!(e2.cost, 0.0);
        // CRLF + no trailing newline
        let content = format!("{{\"type\":\"session\",\"id\":\"crlf\",\"timestamp\":\"2026-10-03T00:00:00.000Z\"}}\r\n{}", user_msg("last", "2026-10-03T00:00:01.000Z"));
        let f3 = write(&dir, "p", "2026-10-03T00-00-00-000Z_crlf.jsonl", &content);
        let e3 = parse_session_file(&f3, "p", 0.0).unwrap();
        assert_eq!(e3.first_prompt, "last");
        assert_eq!(e3.messages, 1);
        let f4 = write(&dir, "p", "empty.jsonl", "");
        assert!(parse_session_file(&f4, "p", 0.0).is_none());
        assert!(parse_session_file(&dir.join("p").join("missing.jsonl"), "p", 0.0).is_none());
        // lenient: wrong types are ignored, duplicate keys are last-wins (JS semantics)
        let f5 = write(&dir, "p", "2026-10-04T00-00-00-000Z_len.jsonl", &jl(&[
            s(r#"{"type":"session","id":"len","timestamp":"2026-10-04T00:00:00.000Z","cwd":5}"#),
            s(r#"{"type":"message","timestamp":"2026-10-04T00:00:01.000Z","message":{"role":"assistant","model":7,"usage":{"cost":"x"},"content":3}}"#),
            s(r#"{"type":"message","message":"oops"}"#),
            s(r#"{"type":"message","message":{"role":"user","content":"a","content":"b"}}"#),
        ]));
        let e5 = parse_session_file(&f5, "p", 0.0).unwrap();
        assert_eq!(e5.cwd, "");
        assert_eq!(e5.messages, 2);
        assert_eq!(e5.first_prompt, "b");
        assert!(e5.models.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scan_uses_cache_and_excludes_subagents() {
        let dir = tmp_dir("scan");
        let mk = |id: &str, text: &str| jl(&[
            format!(r#"{{"type":"session","id":"{id}","timestamp":"2026-10-05T00:00:00.000Z","cwd":"C:/w/x"}}"#),
            user_msg(text, "2026-10-05T00:00:01.000Z"),
        ]);
        let sessions = dir.join("sessions");
        let f_a = write(&sessions, "--C--w-x--", "2026-10-05T00-00-00-000Z_a.jsonl", &mk("a", "alpha"));
        write(&sessions, "--C--w-y--", "2026-10-05T00-00-00-000Z_b.jsonl", &mk("b", "beta"));
        write(&sessions, "--C--AppData-Local-Temp-pi-agent-1234--", "2026-10-05T00-00-00-000Z_c.jsonl", &mk("c", "gamma"));
        write(&sessions, "--C--w-y--", "notes.txt", "ignore");
        let cache = dir.join("cache.json");
        let ids = |r: &ScanResult| {
            let mut v: Vec<String> = r.entries.iter().map(|e| e.id.clone()).collect();
            v.sort();
            v
        };
        let last = std::sync::Mutex::new((0, 0));
        let cb = |d: usize, t: usize| {
            let mut l = last.lock().unwrap();
            if d >= l.0 {
                *l = (d, t)
            }
        };
        let r1 = scan_sessions(&ScanOptions { sessions_dir: &sessions, cache_path: &cache, include_subagents: false, on_progress: Some(&cb) });
        assert_eq!(ids(&r1), vec!["a", "b"]);
        assert_eq!((r1.parsed, r1.cached, r1.skipped_folders), (3, 0, 1)); // sub-agent folders are scanned too (spend)
        assert_eq!(*last.lock().unwrap(), (3, 3));
        let opts = |all| ScanOptions { sessions_dir: &sessions, cache_path: &cache, include_subagents: all, on_progress: None };
        let r2 = scan_sessions(&opts(false));
        assert_eq!((r2.parsed, r2.cached), (0, 3));
        assert_eq!(ids(&r2), vec!["a", "b"]);
        // changed file is re-parsed
        std::fs::write(&f_a, mk("a", "alpha changed, longer")).unwrap();
        let r3 = scan_sessions(&opts(false));
        assert_eq!(r3.parsed, 1);
        assert_eq!(r3.entries.iter().find(|e| e.id == "a").unwrap().first_prompt, "alpha changed, longer");
        let r4 = scan_sessions(&opts(true));
        assert_eq!(ids(&r4), vec!["a", "b", "c"]);
        assert_eq!(r4.parsed, 0);
        assert_eq!(scan_sessions(&opts(true)).parsed, 0);
        // corrupt cache is ignored
        std::fs::write(&cache, "{not json").unwrap();
        assert_eq!(scan_sessions(&opts(false)).parsed, 3);
        // missing sessions dir
        let missing = dir.join("nope");
        let r6 = scan_sessions(&ScanOptions { sessions_dir: &missing, cache_path: &cache, include_subagents: false, on_progress: None });
        assert!(r6.entries.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_json_matches_ts_format() {
        let ts = r#"{"version":1,"files":{"C:\\x\\a.jsonl":{"mtimeMs":1792725227205.3103,"size":1981224,"entry":{"id":"a","file":"C:\\x\\a.jsonl","folder":"f","project":"p","cwd":"","startedAt":"2026-10-23T02:25:22.208Z","endedAt":"2026-10-23T02:25:22.208Z","title":"t","firstPrompt":"t","searchText":"t","cost":0,"messages":3,"models":["m"]}}}}"#;
        let c: CacheFile = serde_json::from_str(ts).unwrap();
        let f = &c.files["C:\\x\\a.jsonl"];
        assert_eq!(f.size, 1981224);
        assert_eq!(f.entry.messages, 3);
        // mtimeMs must survive a parse/serialize round trip bit-exactly (TS compares with ===)
        assert!(serde_json::to_string(&c).unwrap().contains("\"mtimeMs\":1792725227205.3103"));
        assert_eq!(f.entry.started_at, "2026-10-23T02:25:22.208Z");
        let out = serde_json::to_value(&f.entry).unwrap();
        assert_eq!(out["firstPrompt"], "t");
        assert_eq!(out["searchText"], "t");
        assert_eq!(out.as_object().unwrap().len(), 13);
    }

    /* ---------------------------------------------------------------- spend */

    fn asst_full(ts: &str, model: &str, cost: f64, input: u64, output: u64, cr: u64, cw: u64) -> String {
        serde_json::json!({"type":"message","id":format!("a{ts}"),"timestamp":ts,"message":{"role":"assistant","model":model,"content":"x",
            "usage":{"input":input,"output":output,"cacheRead":cr,"cacheWrite":cw,"totalTokens":input+output+cr+cw,"cost":{"total":cost}}}}).to_string()
    }

    #[test]
    fn date_in_tz_boundaries() {
        let jst = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        assert_eq!(date_in_tz("2026-10-01T14:59:59.999Z", &jst).unwrap(), "2026-10-01");
        assert_eq!(date_in_tz("2026-10-01T15:00:00.000Z", &jst).unwrap(), "2026-10-02");
        let west = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(date_in_tz("2026-01-01T04:59:59Z", &west).unwrap(), "2025-12-31");
        assert_eq!(date_in_tz("2026-01-01T05:00:00Z", &west).unwrap(), "2026-01-01");
        assert!(date_in_tz("garbage", &jst).is_none());
        // the Local variant agrees with chrono's own conversion
        let ts = "2026-06-15T12:34:56Z";
        let want = chrono::DateTime::parse_from_rfc3339(ts).unwrap().with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();
        assert_eq!(local_date(ts).unwrap(), want);
    }

    #[test]
    fn daily_split_by_day_and_model() {
        let dir = tmp_dir("daily");
        let (t1, t2, t3) = ("2026-10-01T00:00:00.000Z", "2026-10-03T00:00:00.000Z", "2026-10-03T00:00:05.000Z");
        let lines = vec![
            r#"{"type":"session","id":"d1","timestamp":"2026-10-01T00:00:00.000Z","cwd":"C:/w/x","parentSession":"C:\\p\\parent.jsonl"}"#.to_string(),
            asst_full(t1, "m-a", 1.5, 10, 20, 30, 40),
            asst_full(t2, "m-a", 0.5, 1, 2, 3, 4),
            asst_full(t3, "m-b", 0.25, 100, 200, 0, 0),
            asst_full(t3, "m-a", 0.25, 1, 1, 1, 1),
            user_msg("hi", t3),
            // no model -> "unknown"; wrong typed numbers count as 0
            r#"{"type":"message","timestamp":"2026-10-03T00:00:06.000Z","message":{"role":"assistant","usage":{"input":"x","cost":{"total":"y"}}}}"#.to_string(),
            r#"{"type":"usage","timestamp":"2026-10-03T00:00:07.000Z","kind":"cache_warm","model":"m-a","usage":{"input":2,"output":1,"cacheRead":5,"cacheWrite":0,"cost":{"total":0.125}}}"#.to_string(),
            // missing timestamp -> previous line's date
            r#"{"type":"message","message":{"role":"assistant","model":"m-b","usage":{"cost":{"total":2}}}}"#.to_string(),
        ];
        let f = write(&dir, "p", "2026-10-01T00-00-00-000Z_d1.jsonl", &jl(&lines));
        let (e, sp) = parse_session_full(&f, "p", 0.0).unwrap();
        assert_eq!(sp.parent_session, "C:\\p\\parent.jsonl");
        let (d1, d3) = (local_date(t1).unwrap(), local_date(t2).unwrap());
        assert_ne!(d1, d3); // two days apart: differs in every time zone
        let get = |d: &str, m: &str| sp.daily.iter().find(|r| r.date == d && r.model == m).cloned();
        let a1 = get(&d1, "m-a").unwrap();
        assert_eq!((a1.cost, a1.input, a1.output, a1.cache_read, a1.cache_write, a1.messages), (1.5, 10, 20, 30, 40, 1));
        let a3 = get(&d3, "m-a").unwrap();
        assert_eq!((a3.messages, a3.input), (2, 1 + 1 + 2));
        assert!((a3.cost - 0.875).abs() < 1e-12);
        assert_eq!(get(&d3, "m-b").unwrap().cost, 2.25);
        let unk = get(&d3, "unknown").unwrap();
        assert_eq!((unk.cost, unk.input, unk.messages), (0.0, 0, 1));
        // usage lines carry cost / tokens but are not messages: 2 + 1 + 1 + 1 + 1 (m-a, m-a, m-b, unknown, m-b)
        assert_eq!(sp.daily.iter().map(|r| r.messages).sum::<u64>(), 6);
        // the daily total equals the session cost; (date, model) rows are unique
        let sum: f64 = sp.daily.iter().map(|r| r.cost).sum();
        assert!((sum - e.cost).abs() < 1e-9 && (e.cost - 4.625).abs() < 1e-9, "{sum} {}", e.cost);
        let mut keys: Vec<_> = sp.daily.iter().map(|r| (r.date.clone(), r.model.clone())).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), sp.daily.len());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn sc(file: &str, folder: &str, project: &str, parent: &str, cost: f64) -> Scanned {
        Scanned {
            entry: SessionEntry { id: file.into(), file: file.into(), folder: folder.into(), project: project.into(), ..Default::default() },
            spend: SessionSpend {
                parent_session: parent.into(),
                daily: vec![DailyRow { date: "2026-10-01".into(), model: "m".into(), cost, input: 1, output: 2, cache_read: 3, cache_write: 4, messages: 1 }],
            },
        }
    }
    const SUB: &str = "--C--Temp-pi-agent-abc--";

    fn spend_of(rows: &[SpendRow], project: &str, sub: bool) -> f64 {
        rows.iter().filter(|r| r.project == project && r.sub == sub).map(|r| r.cost).sum()
    }

    #[test]
    fn spend_folds_subagents_into_owner_project() {
        let all = vec![
            sc("C:\\s\\main.jsonl", "f", "W/main", "", 1.0),
            sc("C:\\s\\fork.jsonl", "f", "W/fork", "C:\\s\\main.jsonl", 10.0), // normal session with a parent: independent
            sc("C:\\s\\t\\sub1.jsonl", SUB, "T/sub", "c:/s/main.jsonl", 100.0), // other slash style / case
            sc("C:\\s\\t\\sub2.jsonl", SUB, "T/sub", "C:\\s\\t\\sub1.jsonl", 1000.0), // sub of a sub
            sc("C:\\s\\t\\orphan.jsonl", SUB, "T/orphan", "C:\\s\\gone.jsonl", 10000.0), // parent missing
            sc("C:\\s\\t\\noparent.jsonl", SUB, "T/none", "", 100000.0),
        ];
        let rows = build_spend(&all);
        assert!((spend_of(&rows, "W/main", false) - 1.0).abs() < 1e-9);
        assert!((spend_of(&rows, "W/main", true) - 1100.0).abs() < 1e-9);
        assert!((spend_of(&rows, "W/fork", false) - 10.0).abs() < 1e-9);
        assert_eq!(spend_of(&rows, "W/fork", true), 0.0);
        assert!((spend_of(&rows, "T/orphan", true) - 10000.0).abs() < 1e-9);
        assert!((spend_of(&rows, "T/none", true) - 100000.0).abs() < 1e-9);
        assert_eq!(spend_of(&rows, "T/sub", true), 0.0);
        let total: f64 = rows.iter().map(|r| r.cost).sum();
        assert!((total - 111111.0).abs() < 1e-6);
        let r = rows.iter().find(|r| r.project == "W/main" && r.sub).unwrap();
        assert_eq!((r.input, r.output, r.cache_read, r.cache_write, r.messages), (2, 4, 6, 8, 2));
    }

    #[test]
    fn spend_parent_chain_depth_and_cycles() {
        // a chain of n sub-agents ending at a normal session
        let chain = |n: usize| {
            let mut v = vec![sc("root", "f", "W/root", "", 1.0)];
            for i in 1..=n {
                let parent = if i == 1 { "root".to_string() } else { format!("s{}", i - 1) };
                v.push(sc(&format!("s{i}"), SUB, "T/sub", &parent, 0.0));
            }
            v
        };
        let rows = build_spend(&chain(10)); // the deepest (s10) needs 10 hops: resolves
        assert_eq!(rows.iter().filter(|r| r.project == "W/root" && r.sub).count(), 1);
        assert!(rows.iter().all(|r| r.project != "T/sub"));
        let rows = build_spend(&chain(11)); // s11 needs 11 hops: stays under its own project
        assert_eq!(rows.iter().filter(|r| r.project == "T/sub" && r.sub).count(), 1);
        assert_eq!(rows.iter().filter(|r| r.project == "W/root" && r.sub).count(), 1);
        let cyc = vec![sc("a", SUB, "T/a", "b", 1.0), sc("b", SUB, "T/b", "a", 2.0), sc("c", SUB, "T/c", "c", 4.0)];
        let rows = build_spend(&cyc);
        assert_eq!(spend_of(&rows, "T/a", true), 1.0);
        assert_eq!(spend_of(&rows, "T/b", true), 2.0);
        assert_eq!(spend_of(&rows, "T/c", true), 4.0);
    }

    #[test]
    fn scan_spend_includes_subagents_even_when_hidden() {
        let dir = tmp_dir("spendscan");
        let sessions = dir.join("sessions");
        let main = write(&sessions, "--C--w-x--", "2026-10-05T00-00-00-000Z_m.jsonl", &jl(&[
            r#"{"type":"session","id":"m","timestamp":"2026-10-05T00:00:00.000Z","cwd":"C:/w/x"}"#.to_string(),
            asst_full("2026-10-05T00:00:01.000Z", "mm", 1.0, 1, 1, 1, 1),
        ]));
        let sub_line = serde_json::json!({"type":"session","id":"s","timestamp":"2026-10-05T00:01:00.000Z","cwd":"C:/AppData/Local/Temp",
            "parentSession": main.to_string_lossy()}).to_string();
        write(&sessions, "--C--AppData-Local-Temp-pi-agent-9f--", "2026-10-05T00-01-00-000Z_s.jsonl", &jl(&[
            sub_line, asst_full("2026-10-05T00:01:01.000Z", "ms", 0.5, 2, 2, 2, 2),
        ]));
        let cache = dir.join("cache.json");
        let opts = |all| ScanOptions { sessions_dir: &sessions, cache_path: &cache, include_subagents: all, on_progress: None };
        for (all, expect_entries) in [(false, 1), (true, 2), (false, 1)] {
            let r = scan_sessions(&opts(all));
            assert_eq!(r.entries.len(), expect_entries);
            assert_eq!(r.spend.len(), 2, "{:?}", r.spend);
            let m = r.spend.iter().find(|x| !x.sub).unwrap();
            let s = r.spend.iter().find(|x| x.sub).unwrap();
            assert_eq!((m.project.as_str(), s.project.as_str()), ("w/x", "w/x"));
            assert_eq!((m.cost, s.cost, s.model.as_str()), (1.0, 0.5, "ms"));
            assert_eq!((s.input, s.output, s.cache_read, s.cache_write, s.messages), (2, 2, 2, 2, 1));
            assert_eq!(m.date, local_date("2026-10-05T00:00:01.000Z").unwrap());
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_v2_roundtrip_and_v1_dropped() {
        let dir = tmp_dir("cachev2");
        let sessions = dir.join("sessions");
        write(&sessions, "p", "2026-10-05T00-00-00-000Z_a.jsonl", &jl(&[
            r#"{"type":"session","id":"a","timestamp":"2026-10-05T00:00:00.000Z","parentSession":"/x/y.jsonl"}"#.to_string(),
            asst_full("2026-10-05T00:00:01.000Z", "mm", 0.1 + 0.2, 7, 8, 9, 10),
        ]));
        let cache = dir.join("cache.json");
        let opts = ScanOptions { sessions_dir: &sessions, cache_path: &cache, include_subagents: false, on_progress: None };
        let r1 = scan_sessions(&opts);
        assert_eq!(r1.parsed, 1);
        let c = load_cache(&cache);
        assert_eq!(c.version, 2);
        let cf = c.files.values().next().unwrap();
        assert_eq!(cf.parent_session, "/x/y.jsonl");
        assert_eq!(cf.daily.len(), 1);
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        let ent = raw["files"].as_object().unwrap().values().next().unwrap();
        assert_eq!(ent["daily"][0]["cacheRead"], 9);
        assert_eq!(ent["entry"].as_object().unwrap().len(), 13); // list entry shape unchanged
        let r2 = scan_sessions(&opts);
        assert_eq!((r2.parsed, r2.cached), (0, 1));
        assert_eq!(r2.spend, r1.spend);
        assert_eq!(r2.spend[0].cost, 0.1 + 0.2); // bit-exact through the cache
        // an old (v1) cache is dropped and re-parsed
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        v["version"] = 1.into();
        std::fs::write(&cache, serde_json::to_vec(&v).unwrap()).unwrap();
        let r3 = scan_sessions(&opts);
        assert_eq!(r3.parsed, 1);
        assert_eq!(r3.spend, r1.spend);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn mk_e(id: &str, file: &str, started: &str) -> SessionEntry {
        SessionEntry { id: id.into(), file: file.into(), folder: "f".into(), project: "p".into(), started_at: started.into(), ended_at: started.into(), ..Default::default() }
    }

    #[test]
    fn build_index_and_resolve() {
        let idx = build_index(vec![
            mk_e("dup", "/a.jsonl", "2026-01-01T00:00:00Z"),
            mk_e("dup", "/b.jsonl", "2026-01-02T00:00:00Z"),
            mk_e("solo", "/c.jsonl", "2026-01-03T00:00:00Z"),
        ]);
        let ids: Vec<&str> = idx.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["solo", "dup-2", "dup"]);
        assert_eq!(idx.resolve("dup").unwrap().file, "/a.jsonl");
        assert_eq!(idx.resolve("dup-2").unwrap().file, "/b.jsonl");
        assert_eq!(idx.resolve("solo").unwrap().file, "/c.jsonl");
        for bad in ["nope", "../etc/passwd", "..%2F..%2Fetc%2Fpasswd", "%2e%2e%5csolo", "C:/Windows/win.ini", "%E0%A4%A", "", "__proto__", "constructor"] {
            assert!(idx.resolve(bad).is_none(), "{bad}");
        }
    }
}
