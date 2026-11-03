//! Prompt-cache statistics: cache misses and relay / compaction events, derived from the session tree
//! (`id` / `parentId`) of each jsonl file. See README "キャッシュ統計" for the definitions.
use crate::scan::{is_subagent_folder, local_date, owner_projects, Scanned};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// A miss needs at least this many tokens expected to be read from the cache (previous cacheRead + cacheWrite).
pub const MISS_MIN_EXPECTED: u64 = 4000;
/// Miss iff `cacheRead < expected * MISS_RATIO` (exactly half is still a hit).
pub const MISS_RATIO: f64 = 0.5;
/// Anthropic default prompt-cache TTL (ms) and the extended one used when the last cache write was a 1h write.
pub const TTL_SHORT_MS: f64 = 300_000.0;
pub const TTL_LONG_MS: f64 = 3_600_000.0;
/// `api` value for which a TTL expiry can be diagnosed (other providers never get cause "ttl").
pub const API_ANTHROPIC: &str = "anthropic-messages";
/// A relay-log line matches an event when the session ids agree and the times differ by at most this (ms).
pub const TRIGGER_MATCH_MS: i64 = 120_000;

/// Per (local date) miss counters of one session; stored in the cache file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CacheDay {
    pub date: String,
    pub misses: u64,
    pub miss_tokens: u64,
    pub miss_cost: f64,
    pub miss_ttl: u64,
    pub miss_ttl_tokens: u64,
    pub miss_model: u64,
    pub miss_model_tokens: u64,
}

/// One relay / compaction event of one session; stored in the cache file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CacheEventRow {
    pub ts: String,
    pub date: String,
    /// "relay" | "compact"
    pub kind: String,
    pub before: u64,
    pub after: u64,
    pub dropped: u64,
    pub recache: u64,
    pub recache_cost: f64,
}

/// A valid assistant response (has usage, not error/aborted, some context tokens).
#[derive(Debug, Clone, Default)]
pub struct AsstRec {
    /// provider + model (compared for equality only)
    pub key: String,
    pub anthropic: bool,
    /// request start (`message.timestamp`, ms epoch)
    pub start_ms: Option<f64>,
    pub input: u64,
    pub cr: u64,
    pub cw: u64,
    pub cw1h: u64,
    pub cost_cw: f64,
}

struct Asst {
    rec: AsstRec,
    /// the most recent cache write on the chain (this message's if it wrote, else inherited) was a 1h write
    tier1h: bool,
}

/// What the children of an entry see as their predecessor.
#[derive(Clone, Copy)]
enum Prev {
    None,
    Asst(usize),
    Compaction(usize),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Cause {
    Ttl,
    Model,
    Other,
}

#[derive(Debug, PartialEq)]
pub struct Miss {
    pub tokens: u64,
    pub cost: f64,
    pub cause: Cause,
}

/// Is `a` (a response following `p` on the same chain, not right after a compaction) a cache miss?
fn classify(p: &Asst, a: &AsstRec) -> Option<Miss> {
    let pr = &p.rec;
    let expected = pr.cr + pr.cw;
    if expected < MISS_MIN_EXPECTED || (a.cr as f64) >= expected as f64 * MISS_RATIO {
        return None;
    }
    let tokens = expected - a.cr; // a.cr < expected here
    let cause = if a.key != pr.key {
        Cause::Model
    } else if a.anthropic {
        let ttl = if p.tier1h { TTL_LONG_MS } else { TTL_SHORT_MS };
        match (a.start_ms, pr.start_ms) {
            (Some(now), Some(before)) if now - before > ttl => Cause::Ttl,
            _ => Cause::Other,
        }
    } else {
        Cause::Other
    };
    let recached = tokens.min(a.cw);
    let cost = if a.cw > 0 && a.cost_cw > 0.0 { recached as f64 * (a.cost_cw / a.cw as f64) } else { 0.0 };
    Some(Miss { tokens, cost, cause })
}

#[derive(Default)]
pub struct CacheTracker {
    nodes: HashMap<String, Prev>,
    assts: Vec<Asst>,
    events: Vec<CacheEventRow>,
    done: Vec<bool>,
    days: HashMap<String, CacheDay>,
}

impl CacheTracker {
    fn prev_of(&self, parent: Option<&str>) -> Prev {
        parent.and_then(|p| self.nodes.get(p)).copied().unwrap_or(Prev::None)
    }

    /// Any entry that is neither a valid assistant message nor a compaction: transparent for the chain.
    pub fn other(&mut self, id: &str, parent: Option<&str>) {
        let p = self.prev_of(parent);
        self.nodes.insert(id.to_string(), p);
    }

    /// A compaction cuts the chain: its descendants have no predecessor beyond it.
    pub fn compaction(&mut self, id: &str, ts: &str, before: u64, relay: bool) {
        let idx = self.events.len();
        self.events.push(CacheEventRow {
            ts: ts.to_string(),
            date: local_date(ts).unwrap_or_default(),
            kind: if relay { "relay" } else { "compact" }.to_string(),
            before,
            ..Default::default()
        });
        self.done.push(false);
        self.nodes.insert(id.to_string(), Prev::Compaction(idx));
    }

    /// An assistant message; `rec` is None when it is not a valid response (then it is transparent).
    pub fn assistant(&mut self, id: &str, parent: Option<&str>, ts: &str, rec: Option<AsstRec>) {
        let prev = self.prev_of(parent);
        let Some(a) = rec else {
            self.nodes.insert(id.to_string(), prev);
            return;
        };
        let inherited_tier = match prev {
            Prev::Asst(i) => self.assts[i].tier1h,
            _ => false,
        };
        match prev {
            Prev::Compaction(i) => {
                if !self.done[i] {
                    self.done[i] = true;
                    let e = &mut self.events[i];
                    e.after = a.input + a.cr + a.cw;
                    e.recache = a.cw;
                    e.recache_cost = a.cost_cw;
                    e.dropped = e.before.saturating_sub(e.after);
                }
            }
            Prev::Asst(i) => {
                if let Some(m) = classify(&self.assts[i], &a) {
                    self.record_miss(ts, m);
                }
            }
            Prev::None => {}
        }
        let tier1h = if a.cw > 0 { a.cw1h > 0 } else { inherited_tier };
        self.assts.push(Asst { rec: a, tier1h });
        self.nodes.insert(id.to_string(), Prev::Asst(self.assts.len() - 1));
    }

    fn record_miss(&mut self, ts: &str, m: Miss) {
        let date = local_date(ts).unwrap_or_default();
        let d = self.days.entry(date.clone()).or_insert_with(|| CacheDay { date, ..Default::default() });
        d.misses += 1;
        d.miss_tokens += m.tokens;
        d.miss_cost += m.cost;
        match m.cause {
            Cause::Ttl => {
                d.miss_ttl += 1;
                d.miss_ttl_tokens += m.tokens;
            }
            Cause::Model => {
                d.miss_model += 1;
                d.miss_model_tokens += m.tokens;
            }
            Cause::Other => {}
        }
    }

    /// Per-date miss rows (an unparsable timestamp gives date "") and the events in file order.
    pub fn finish(self) -> (Vec<CacheDay>, Vec<CacheEventRow>) {
        (self.days.into_values().collect(), self.events)
    }
}

/* ------------------------------------------------------------------ aggregation */

/// One row of `/api/index`'s `cache`: (date, project, sub) -> sums over all sessions.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheRow {
    pub date: String,
    pub project: String,
    pub sub: bool,
    pub misses: u64,
    pub miss_tokens: u64,
    pub miss_cost: f64,
    pub miss_ttl: u64,
    pub miss_ttl_tokens: u64,
    pub miss_model: u64,
    pub miss_model_tokens: u64,
    pub relays: u64,
    pub relay_before: u64,
    pub relay_after: u64,
    pub relay_dropped: u64,
    pub relay_recache: u64,
    pub relay_recache_cost: f64,
    pub compacts: u64,
    pub compact_before: u64,
    pub compact_after: u64,
    pub compact_dropped: u64,
    pub compact_recache: u64,
    pub compact_recache_cost: f64,
}

impl CacheRow {
    fn is_zero(&self) -> bool {
        self.misses == 0 && self.relays == 0 && self.compacts == 0
    }
}

/// One element of `/api/index`'s `cacheEvents`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEvent {
    pub ts: String,
    pub date: String,
    pub project: String,
    pub sub: bool,
    pub kind: String,
    pub before: u64,
    pub after: u64,
    pub dropped: u64,
    pub recache: u64,
    pub recache_cost: f64,
    pub sid: String,
    pub trigger: Option<String>,
}

/// One successful relay from `<agentDir>/context-relay/log.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayLog {
    pub at_ms: i64,
    pub session_id: String,
    pub trigger: String,
}

#[derive(Deserialize, Default)]
struct RelayLogLine {
    #[serde(default)]
    at: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    ok: Option<bool>,
}

fn ts_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis())
}

/// Best effort: a missing / unreadable file or broken lines just yield fewer (or no) entries.
pub fn load_relay_log(path: &Path) -> Vec<RelayLog> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(l) = serde_json::from_str::<RelayLogLine>(line) else { continue };
        if l.ok != Some(true) {
            continue;
        }
        let (Some(at), Some(sid), Some(trigger)) = (l.at.as_deref().and_then(ts_ms), l.session_id, l.trigger) else { continue };
        if matches!(trigger.as_str(), "manual" | "input" | "expiry") {
            out.push(RelayLog { at_ms: at, session_id: sid, trigger });
        }
    }
    out
}

type AggKey = (String, String, bool);

fn row<'a>(agg: &'a mut BTreeMap<AggKey, CacheRow>, date: &str, project: &str, sub: bool) -> &'a mut CacheRow {
    agg.entry((date.to_string(), project.to_string(), sub))
        .or_insert_with(|| CacheRow { date: date.to_string(), project: project.to_string(), sub, ..Default::default() })
}

/// Aggregate misses and events of ALL sessions (sub-agents folded like `build_spend`); `relay_log` fills `trigger` of relay events.
pub fn build_cache(all: &[Scanned], relay_log: &[RelayLog]) -> (Vec<CacheRow>, Vec<CacheEvent>) {
    let owners = owner_projects(all);
    let mut agg: BTreeMap<AggKey, CacheRow> = BTreeMap::new();
    let mut events: Vec<CacheEvent> = Vec::new();
    for (i, s) in all.iter().enumerate() {
        let project = owners[i];
        let sub = is_subagent_folder(&s.entry.folder);
        for d in &s.spend.cache_daily {
            let r = row(&mut agg, &d.date, project, sub);
            r.misses += d.misses;
            r.miss_tokens += d.miss_tokens;
            r.miss_cost += d.miss_cost;
            r.miss_ttl += d.miss_ttl;
            r.miss_ttl_tokens += d.miss_ttl_tokens;
            r.miss_model += d.miss_model;
            r.miss_model_tokens += d.miss_model_tokens;
        }
        for e in &s.spend.cache_events {
            let r = row(&mut agg, &e.date, project, sub);
            if e.kind == "relay" {
                r.relays += 1;
                r.relay_before += e.before;
                r.relay_after += e.after;
                r.relay_dropped += e.dropped;
                r.relay_recache += e.recache;
                r.relay_recache_cost += e.recache_cost;
            } else {
                r.compacts += 1;
                r.compact_before += e.before;
                r.compact_after += e.after;
                r.compact_dropped += e.dropped;
                r.compact_recache += e.recache;
                r.compact_recache_cost += e.recache_cost;
            }
            events.push(CacheEvent {
                ts: e.ts.clone(),
                date: e.date.clone(),
                project: project.to_string(),
                sub,
                kind: e.kind.clone(),
                before: e.before,
                after: e.after,
                dropped: e.dropped,
                recache: e.recache,
                recache_cost: e.recache_cost,
                sid: s.entry.id.clone(),
                trigger: None,
            });
        }
    }
    events.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.sid.cmp(&b.sid)));
    attach_triggers(&mut events, relay_log);
    let rows = agg.into_values().filter(|r| !r.is_zero()).collect();
    (rows, events)
}

/// Each relay event takes the nearest (within `TRIGGER_MATCH_MS`) not yet used log line of its session.
fn attach_triggers(events: &mut [CacheEvent], relay_log: &[RelayLog]) {
    if relay_log.is_empty() {
        return;
    }
    let mut by_sid: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, l) in relay_log.iter().enumerate() {
        by_sid.entry(l.session_id.as_str()).or_default().push(i);
    }
    let mut used = vec![false; relay_log.len()];
    for e in events.iter_mut().filter(|e| e.kind == "relay") {
        let (Some(cands), Some(t)) = (by_sid.get(e.sid.as_str()), ts_ms(&e.ts)) else { continue };
        let best = cands
            .iter()
            .copied()
            .filter(|&i| !used[i] && (relay_log[i].at_ms - t).abs() <= TRIGGER_MATCH_MS)
            .min_by_key(|&i| (relay_log[i].at_ms - t).abs());
        if let Some(i) = best {
            used[i] = true;
            e.trigger = Some(relay_log[i].trigger.clone());
        }
    }
}

#[cfg(test)]
mod tests;
