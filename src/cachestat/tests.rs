use super::*;
use crate::scan::tests::{jl, tmp_dir, write};
use crate::scan::{iso_from_ms, parse_session_full, scan_sessions, ScanOptions, SessionEntry, SessionSpend};

const T0: f64 = 1_790_000_000_000.0;
const SUB: &str = "--C--Temp-pi-agent-abc--";

fn hdr() -> String {
    format!(r#"{{"type":"session","version":3,"id":"s1","timestamp":"{}","cwd":"C:/w/x"}}"#, iso_from_ms(T0))
}

fn parent_json(parent: Option<&str>) -> String {
    parent.map_or("null".to_string(), |p| format!("\"{p}\""))
}

/// Assistant line; `start` = request start (ms after T0), the entry timestamp is 1 s later.
/// `usage` = (input, cacheRead, cacheWrite, cacheWrite1h).
#[allow(clippy::too_many_arguments)]
fn asst(id: &str, parent: Option<&str>, start: f64, provider: &str, model: &str, api: &str, usage: (u64, u64, u64, u64), cost_cw: f64, stop: &str) -> String {
    let (input, cr, cw, cw1h) = usage;
    let msg = serde_json::json!({"role":"assistant","provider":provider,"model":model,"api":api,"stopReason":stop,"timestamp":T0 + start,
        "content":[],"usage":{"input":input,"output":5,"cacheRead":cr,"cacheWrite":cw,"cacheWrite1h":cw1h,
        "cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":cost_cw,"total":cost_cw}}});
    format!(r#"{{"type":"message","id":"{id}","parentId":{},"timestamp":"{}","message":{msg}}}"#, parent_json(parent), iso_from_ms(T0 + start + 1000.0))
}

/// A normal anthropic response (cache read / write only).
fn a(id: &str, parent: Option<&str>, start: f64, cr: u64, cw: u64) -> String {
    asst(id, parent, start, "anthropic", "opus", API_ANTHROPIC, (10, cr, cw, 0), 0.0, "stop")
}

fn comp(id: &str, parent: Option<&str>, at: f64, before: u64, relay: bool, pad: usize) -> String {
    let details = if relay { r#"{"relay":true,"version":1}"# } else { r#"{"version":1}"# };
    let big = "x".repeat(pad);
    format!(
        r#"{{"type":"compaction","id":"{id}","parentId":{},"timestamp":"{}","summary":"{big}","firstKeptEntryId":"zz","tokensBefore":{before},"details":{details},"fromHook":true,"systemMessage":{{"role":"system","content":"{big}","nested":[1,2,{{"a":"b"}}]}}}}"#,
        parent_json(parent),
        iso_from_ms(T0 + at)
    )
}

fn big_tool(id: &str, parent: Option<&str>, at: f64) -> String {
    let big = "y".repeat(3000);
    format!(
        r#"{{"type":"message","id":"{id}","parentId":{},"timestamp":"{}","message":{{"role":"toolResult","toolCallId":"c","content":[{{"type":"text","text":"{big}"}}]}}}}"#,
        parent_json(parent),
        iso_from_ms(T0 + at)
    )
}

fn parse(lines: Vec<String>) -> SessionSpend {
    let dir = tmp_dir("cachestat");
    let mut v = vec![hdr()];
    v.extend(lines);
    let f = write(&dir, "p", "2026-10-01T00-00-00-000Z_s1.jsonl", &jl(&v));
    let (_, sp) = parse_session_full(&f, "p", 0.0).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    sp
}

/// (misses, missTokens, ttl, model) summed over days
fn totals(sp: &SessionSpend) -> (u64, u64, u64, u64) {
    let d = &sp.cache_daily;
    (d.iter().map(|d| d.misses).sum(), d.iter().map(|d| d.miss_tokens).sum(), d.iter().map(|d| d.miss_ttl).sum(), d.iter().map(|d| d.miss_model).sum())
}

/// Previous response (cr, cw, cw1h, start) followed by one with (cr, cw, start).
fn two(p: (u64, u64, u64, f64), m: (u64, u64, f64), same_model: bool, api: &str) -> SessionSpend {
    let pm = asst("p", None, p.3, "anthropic", "opus", api, (10, p.0, p.1, p.2), 0.0, "stop");
    let model = if same_model { "opus" } else { "sonnet" };
    parse(vec![pm, asst("m", Some("p"), m.2, "anthropic", model, api, (10, m.0, m.1, 0), 0.0, "stop")])
}

#[test]
fn relay_and_compact_events() {
    let lines = vec![
        asst("a1", None, 0.0, "anthropic", "opus", API_ANTHROPIC, (10, 0, 28_000, 0), 0.15, "stop"),
        big_tool("t1", Some("a1"), 5_000.0),
        comp("c1", Some("t1"), 10_000.0, 180_000, true, 4000),
        asst("a2", Some("c1"), 20_000.0, "anthropic", "opus", API_ANTHROPIC, (100, 28_000, 50_000, 0), 0.3, "stop"),
        // the response after the relay is never a miss, and neither is its follower
        a("a3", Some("a2"), 30_000.0, 78_100, 20),
        // a normal compaction that is small (< BIG_LINE), followed by a message that has a larger context than `before`
        comp("c2", Some("a3"), 40_000.0, 1000, false, 0),
        asst("a4", Some("c2"), 50_000.0, "anthropic", "opus", API_ANTHROPIC, (5000, 0, 0, 0), 0.0, "stop"),
        // a compaction without any later message
        comp("c3", Some("a4"), 60_000.0, 90_000, false, 3000),
    ];
    assert!(lines[2].len() > 2000 && lines[1].len() > 2000 && lines[7].len() > 2000);
    assert!(comp("x", None, 0.0, 1, true, 0).len() < 2000);
    let sp = parse(lines);
    assert_eq!(totals(&sp), (0, 0, 0, 0), "{:?}", sp.cache_daily);
    let ev = &sp.cache_events;
    assert_eq!(ev.len(), 3);
    let e = &ev[0];
    assert_eq!((e.kind.as_str(), e.before, e.after, e.dropped, e.recache), ("relay", 180_000, 100 + 28_000 + 50_000, 180_000 - 78_100, 50_000));
    assert!((e.recache_cost - 0.3).abs() < 1e-12);
    assert_eq!(e.ts, iso_from_ms(T0 + 10_000.0));
    assert_eq!(e.date, local_date(&e.ts).unwrap());
    // dropped never underflows: after (5000) > before (1000)
    let e = &ev[1];
    assert_eq!((e.kind.as_str(), e.before, e.after, e.dropped, e.recache), ("compact", 1000, 5000, 0, 0));
    // no following message: dropped unknown
    let e = &ev[2];
    assert_eq!((e.kind.as_str(), e.before, e.after, e.dropped, e.recache, e.recache_cost), ("compact", 90_000, 0, 0, 0, 0.0));
}

#[test]
fn compaction_children_are_never_misses() {
    // two children of one compaction: only the first completes it; neither is a miss
    let sp = parse(vec![
        a("a1", None, 0.0, 0, 20_000),
        comp("c1", Some("a1"), 1_000.0, 50_000, true, 2500),
        a("b1", Some("c1"), 2_000.0, 0, 10_000),
        a("b2", Some("c1"), 3_000.0, 0, 30_000),
    ]);
    assert_eq!(totals(&sp), (0, 0, 0, 0));
    assert_eq!(sp.cache_events.len(), 1);
    assert_eq!((sp.cache_events[0].after, sp.cache_events[0].recache), (10_010, 10_000));
}

#[test]
fn miss_thresholds_and_boundaries() {
    let s = |p, m| totals(&two(p, m, true, API_ANTHROPIC));
    // expected 3999 -> never a miss; 4000 -> miss when nothing is read
    assert_eq!(s((2000, 1999, 0, 0.0), (0, 100, 1000.0)).0, 0);
    assert_eq!(s((2000, 2000, 0, 0.0), (0, 100, 1000.0)).0, 1);
    // exactly half read is a hit, one token less is a miss (missTokens = expected - cr)
    assert_eq!(s((5000, 5000, 0, 0.0), (5000, 0, 1000.0)).0, 0);
    let t = s((5000, 5000, 0, 0.0), (4999, 0, 1000.0));
    assert_eq!((t.0, t.1), (1, 5001));
    // growing context with full read: no miss
    assert_eq!(s((5000, 5000, 0, 0.0), (10_000, 3000, 1000.0)).0, 0);
}

#[test]
fn miss_cause_ttl_model_other() {
    let base = (10_000, 0, 0, 0.0); // cr, cw, cw1h, start
    let ttl = |p: (u64, u64, u64, f64), gap: f64, api: &str| {
        let t = totals(&two(p, (0, 0, gap), true, api));
        assert_eq!(t.0, 1);
        t.2
    };
    // short tier: > 300 s is ttl, exactly 300 s and below is "other"
    assert_eq!(ttl(base, 300_001.0, API_ANTHROPIC), 1);
    assert_eq!(ttl(base, 300_000.0, API_ANTHROPIC), 0);
    assert_eq!(ttl(base, 10_000.0, API_ANTHROPIC), 0);
    // long tier (previous message wrote 1h cache): 1 h boundary
    let long = (0, 10_000, 10_000, 0.0);
    assert_eq!(ttl(long, 3_600_001.0, API_ANTHROPIC), 1);
    assert_eq!(ttl(long, 3_600_000.0, API_ANTHROPIC), 0);
    assert_eq!(ttl(long, 400_000.0, API_ANTHROPIC), 0);
    // other providers never get ttl
    assert_eq!(ttl(base, 9_999_999.0, "openai-responses"), 0);
    // model change wins over ttl
    let sp = two(base, (0, 0, 9_999_999.0), false, API_ANTHROPIC);
    let t = totals(&sp);
    assert_eq!((t.0, t.2, t.3), (1, 0, 1));
    assert_eq!((sp.cache_daily[0].miss_model_tokens, sp.cache_daily[0].miss_ttl_tokens), (10_000, 0));
}

#[test]
fn ttl_tier_is_inherited_along_the_chain() {
    // a1 wrote a 1h cache, a2 only read (cw = 0): the chain is still on the 1h tier for a3
    let lines = |gap: f64| {
        vec![
            asst("a1", None, 0.0, "anthropic", "opus", API_ANTHROPIC, (10, 0, 10_000, 10_000), 0.0, "stop"),
            a("a2", Some("a1"), 10_000.0, 10_000, 0),
            big_tool("t", Some("a2"), 11_000.0),
            a("a3", Some("t"), 10_000.0 + gap, 0, 0),
        ]
    };
    let t = totals(&parse(lines(400_000.0)));
    assert_eq!((t.0, t.2), (1, 0)); // 400 s < 1 h: not ttl
    let t = totals(&parse(lines(3_600_001.0)));
    assert_eq!((t.0, t.2), (1, 1));
    // a later 5 min write switches the tier back
    let t = totals(&parse(vec![
        asst("a1", None, 0.0, "anthropic", "opus", API_ANTHROPIC, (10, 0, 10_000, 10_000), 0.0, "stop"),
        a("a2", Some("a1"), 10_000.0, 10_000, 5000),
        a("a3", Some("a2"), 10_000.0 + 400_000.0, 0, 100),
    ]));
    assert_eq!((t.0, t.2), (1, 1));
}

#[test]
fn errors_and_aborted_are_ignored() {
    let p = a("p", None, 0.0, 0, 10_000);
    // an aborted / error message in between is neither a predecessor nor evaluated
    let mk = |stop: &str, usage: (u64, u64, u64, u64)| {
        parse(vec![
            p.clone(),
            asst("e", Some("p"), 1000.0, "anthropic", "opus", API_ANTHROPIC, usage, 0.0, stop),
            a("m", Some("e"), 2000.0, 10_000, 100),
        ])
    };
    for stop in ["error", "aborted"] {
        assert_eq!(totals(&mk(stop, (10, 0, 0, 0))).0, 0, "{stop}");
        // with big numbers it would be a miss itself / the predecessor of m if it counted
        assert_eq!(totals(&mk(stop, (100, 0, 50_000, 0))).0, 0, "{stop}");
    }
    // a message without any context tokens is not valid either; the chain passes through
    assert_eq!(totals(&mk("stop", (0, 0, 0, 0))).0, 0);
    // the error message must not hide a real miss behind it
    let sp = parse(vec![
        p.clone(),
        asst("e", Some("p"), 1000.0, "anthropic", "opus", API_ANTHROPIC, (100, 0, 0, 0), 0.0, "error"),
        a("m", Some("e"), 2000.0, 0, 100),
    ]);
    assert_eq!(totals(&sp).0, 1);
}

#[test]
fn miss_cost_is_the_rewrite_part_of_the_write_bill() {
    let cost = |m_cr: u64, m_cw: u64, cost_cw: f64| {
        let sp = parse(vec![
            a("p", None, 0.0, 0, 10_000),
            asst("m", Some("p"), 1000.0, "anthropic", "opus", API_ANTHROPIC, (10, m_cr, m_cw, 0), cost_cw, "stop"),
        ]);
        sp.cache_daily.iter().map(|d| d.miss_cost).sum::<f64>()
    };
    assert!((cost(0, 8000, 0.04) - 0.04).abs() < 1e-12); // re-cached 8000 of 8000 written
    assert!((cost(0, 20_000, 0.1) - 0.05).abs() < 1e-12); // only the missed 10000 of 20000 written
    assert_eq!(cost(0, 0, 0.0), 0.0);
    assert_eq!(cost(0, 5000, 0.0), 0.0);
}

#[test]
fn branches_compare_with_their_own_ancestor() {
    let sp = parse(vec![
        a("a1", None, 0.0, 0, 10_000),
        a("a2", Some("a1"), 10_000.0, 10_000, 90_000), // branch 1: grows the context to 100k
        a("a3", Some("a1"), 20_000.0, 10_000, 100),    // branch 2: vs a1 (10k) it is a hit; vs sibling a2 (100k) it would be a miss
        a("a4", Some("a1"), 30_000.0, 0, 100),         // branch 3: vs a1 a real miss of 10000
    ]);
    assert_eq!(totals(&sp), (1, 10_000, 0, 0), "{:?}", sp.cache_daily);
}

#[test]
fn big_lines_keep_the_parent_chain() {
    let sp = parse(vec![
        a("a1", None, 0.0, 0, 10_000),
        big_tool("t1", Some("a1"), 1000.0),
        format!(r#"{{"type":"custom","id":"k1","parentId":"t1","timestamp":"{}","data":"{}"}}"#, iso_from_ms(T0 + 1500.0), "z".repeat(3000)),
        // custom entries carry id / parentId at the end of the line
        format!(r#"{{"type":"custom_message","customType":"x","data":{{"id":"zz","v":"{}"}},"id":"k2","parentId":"k1","timestamp":"{}"}}"#, "z".repeat(3000), iso_from_ms(T0 + 1600.0)),
        a("a2", Some("k2"), 2000.0, 0, 100), // miss vs a1 through the three big lines
    ]);
    assert_eq!(totals(&sp), (1, 10_000, 0, 0));
    // a big line whose head does not have the expected layout just breaks the chain (no false miss)
    let broken = format!(r#"{{"type":"message","timestamp":"{}","id":"t1","parentId":"a1","message":{{"role":"toolResult","content":"{}"}}}}"#, iso_from_ms(T0), "y".repeat(3000));
    let sp = parse(vec![a("a1", None, 0.0, 0, 10_000), broken, a("a2", Some("t1"), 2000.0, 0, 100)]);
    assert_eq!(totals(&sp).0, 0);
    // missing parent: no predecessor, no miss
    assert_eq!(totals(&parse(vec![a("a2", Some("nope"), 2000.0, 0, 100)])).0, 0);
}

#[test]
fn peek_ids_layout() {
    use crate::scan::peek_ids;
    assert_eq!(peek_ids(r#"{"type":"message","id":"ab12","parentId":"cd34","timestamp":"t""#), Some(("ab12", Some("cd34"))));
    assert_eq!(peek_ids(r#"{"type":"message","id":"ab12","parentId":null,"timestamp":"t""#), Some(("ab12", None)));
    assert_eq!(peek_ids(r#"{"type":"message","parentId":"x","id":"ab12""#), None);
    assert_eq!(peek_ids(r#"{"type":"message","id":"ab12","timestamp":"t""#), None);
    assert_eq!(peek_ids(r#"{"type":"message","id":"a\"b","parentId":"c""#), None);
    use crate::scan::peek_ids_tail;
    assert_eq!(peek_ids_tail(r#".."x"]},"id":"69e36d23","parentId":"daecc7ef","timestamp":"2026-10-13T10:46:16.430Z"}"#), Some(("69e36d23", Some("daecc7ef"))));
    assert_eq!(peek_ids_tail(r#"{"a":1},"id":"69e36d23","parentId":null,"timestamp":"2026"}"#), Some(("69e36d23", None)));
    assert_eq!(peek_ids_tail(r#""data":{"id":"e4e4"},"timestamp":"2026"}"#), None);
    assert_eq!(peek_ids_tail(r#""customId":"e4","parentId":"p","timestamp":"2026"}"#), None);
    assert_eq!(peek_ids_tail("no json at all"), None);
}

fn scanned(file: &str, folder: &str, project: &str, parent: &str, day: CacheDay, ev: Vec<CacheEventRow>) -> Scanned {
    Scanned {
        entry: SessionEntry { id: file.into(), file: file.into(), folder: folder.into(), project: project.into(), ..Default::default() },
        spend: SessionSpend { parent_session: parent.into(), cache_daily: vec![day], cache_events: ev, ..Default::default() },
    }
}

fn day(n: u64) -> CacheDay {
    CacheDay { date: "2026-10-01".into(), misses: n, miss_tokens: n * 10, miss_cost: n as f64 * 0.5, miss_ttl: n, miss_ttl_tokens: n * 10, ..Default::default() }
}

fn event(kind: &str, ts: &str, before: u64) -> CacheEventRow {
    CacheEventRow { ts: ts.into(), date: "2026-10-01".into(), kind: kind.into(), before, after: 40, dropped: before - 40, recache: 30, recache_cost: 0.25, ..Default::default() }
}

#[test]
fn cache_folds_subagents_into_owner_project() {
    let all = vec![
        scanned("C:\\s\\main.jsonl", "f", "W/main", "", day(1), vec![event("relay", "2026-10-01T01:00:00Z", 100)]),
        scanned("C:\\s\\t\\sub1.jsonl", SUB, "T/sub", "c:/s/main.jsonl", day(10), vec![event("compact", "2026-10-01T02:00:00Z", 200)]),
        scanned("C:\\s\\t\\orphan.jsonl", SUB, "T/orphan", "C:\\s\\gone.jsonl", day(100), vec![]),
        scanned("C:\\s\\zero.jsonl", "f", "W/zero", "", day(0), vec![]),
    ];
    let (rows, events) = build_cache(&all, &[]);
    assert_eq!(rows.len(), 3, "{rows:?}");
    let main = rows.iter().find(|r| r.project == "W/main" && !r.sub).unwrap();
    assert_eq!((main.misses, main.relays, main.relay_before, main.relay_after, main.relay_dropped, main.relay_recache), (1, 1, 100, 40, 60, 30));
    assert!((main.relay_recache_cost - 0.25).abs() < 1e-12 && main.compacts == 0);
    let sub = rows.iter().find(|r| r.project == "W/main" && r.sub).unwrap();
    assert_eq!((sub.misses, sub.miss_tokens, sub.miss_ttl, sub.miss_ttl_tokens, sub.compacts, sub.compact_before, sub.compact_dropped), (10, 100, 10, 100, 1, 200, 160));
    assert!((sub.miss_cost - 5.0).abs() < 1e-12);
    assert!(rows.iter().any(|r| r.project == "T/orphan" && r.sub && r.misses == 100));
    assert!(rows.iter().all(|r| r.project != "T/sub" && r.project != "W/zero")); // zero rows omitted
    // events: sorted by ts, owner project + sub flag, sid = entry id
    assert_eq!(events.len(), 2);
    assert_eq!((events[0].kind.as_str(), events[0].project.as_str(), events[0].sub, events[0].sid.as_str()), ("relay", "W/main", false, "C:\\s\\main.jsonl"));
    assert_eq!((events[1].kind.as_str(), events[1].project.as_str(), events[1].sub), ("compact", "W/main", true));
    assert_eq!(events[0].trigger, None);
    // JSON contract
    let v = serde_json::to_value(&rows[0]).unwrap();
    for k in ["date", "project", "sub", "misses", "missTokens", "missCost", "missTtl", "missTtlTokens", "missModel", "missModelTokens", "relays", "relayBefore", "relayAfter",
        "relayDropped", "relayRecache", "relayRecacheCost", "compacts", "compactBefore", "compactAfter", "compactDropped", "compactRecache", "compactRecacheCost",
        "relayPostRequests", "relaySavings", "compactPostRequests", "compactSavings", "relayLive", "relayLiveRecacheCost", "relayLiveSavings",
        "relayLivePostRequests", "compactLive", "compactLiveRecacheCost", "compactLiveSavings", "compactLivePostRequests"] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert_eq!(v.as_object().unwrap().len(), 34);
    let v = serde_json::to_value(&events[0]).unwrap();
    for k in ["ts", "date", "project", "sub", "kind", "before", "after", "dropped", "recache", "recacheCost", "postRequests", "savings", "net", "breakEven", "open", "live", "sid", "trigger"] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert_eq!(v.as_object().unwrap().len(), 18);
    assert!(v["trigger"].is_null());
}

#[test]
fn triggers_join_from_relay_log() {
    let dir = tmp_dir("trigger");
    let sessions = dir.join("sessions");
    let relay_at = |id: &str, parent: &str, at: f64| comp(id, Some(parent), at, 150_000, true, 2500);
    let sid = "s1";
    write(&sessions, "--C--w-x--", "2026-10-01T00-00-00-000Z_s1.jsonl", &jl(&[
        hdr(),
        a("a1", None, 0.0, 0, 10_000),
        relay_at("c1", "a1", 10_000.0),
        a("a2", Some("c1"), 11_000.0, 0, 10_000),
        relay_at("c2", "a2", 100_000_000.0),
        a("a3", Some("c2"), 100_001_000.0, 0, 10_000),
        relay_at("c3", "a3", 200_000_000.0), // no log line within 120 s
        comp("c4", Some("a3"), 300_000_000.0, 1, false, 0), // a normal compaction never gets a trigger
        relay_at("c5", "a3", 400_000_000.0), // its log lines: ok:false / unknown trigger
    ]));
    let at = |ms: f64| iso_from_ms(T0 + ms);
    let log = [
        format!(r#"{{"at":"{}","sessionId":"{sid}","trigger":"input","tokensBefore":150000,"ok":true}}"#, at(10_000.0 + 3_000.0)),
        "not json".to_string(),
        format!(r#"{{"at":"{}","sessionId":"other","trigger":"manual","ok":true}}"#, at(100_000_000.0)),
        format!(r#"{{"at":"{}","sessionId":"{sid}","trigger":"expiry","ok":true}}"#, at(100_000_000.0 - 119_000.0)),
        format!(r#"{{"at":"{}","sessionId":"{sid}","trigger":"manual","ok":true}}"#, at(100_000_000.0 + 1_000.0)), // nearer, wins
        format!(r#"{{"at":"{}","sessionId":"{sid}","trigger":"manual","ok":true}}"#, at(200_000_000.0 + 121_000.0)),
        format!(r#"{{"at":"{}","sessionId":"{sid}","trigger":"manual","ok":false}}"#, at(400_000_000.0)),
        format!(r#"{{"at":"{}","sessionId":"{sid}","trigger":"bogus","ok":true}}"#, at(400_000_000.0)),
    ];
    let lp = dir.join("context-relay");
    std::fs::create_dir_all(&lp).unwrap();
    std::fs::write(lp.join("log.jsonl"), jl(&log)).unwrap();
    let cache = dir.join("session-history").join("cache.json");
    let opts = ScanOptions { sessions_dir: &sessions, cache_path: &cache, include_subagents: false, on_progress: None };
    let r = scan_sessions(&opts);
    let trig: Vec<(&str, Option<&str>)> = r.cache_events.iter().map(|e| (e.kind.as_str(), e.trigger.as_deref())).collect();
    assert_eq!(
        trig,
        vec![("relay", Some("input")), ("relay", Some("manual")), ("relay", None), ("compact", None), ("relay", None)],
        "{:?}",
        r.cache_events
    );
    assert!(r.cache_events.iter().all(|e| e.sid == "s1" && e.project == "w/x"));
    // a warm scan (from the cache file) gives the same, and triggers are not stored in the cache file
    let r2 = scan_sessions(&opts);
    assert_eq!((r2.parsed, r2.cached), (0, 1));
    assert_eq!(r2.cache_events, r.cache_events);
    assert_eq!(r2.cache, r.cache);
    assert!(!std::fs::read_to_string(&cache).unwrap().contains("\"trigger\""));
    // no log -> null
    std::fs::remove_file(lp.join("log.jsonl")).unwrap();
    assert!(scan_sessions(&opts).cache_events.iter().all(|e| e.trigger.is_none()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn miss_rows_roundtrip_through_cache_file() {
    let dir = tmp_dir("cacheroundtrip");
    let sessions = dir.join("sessions");
    write(&sessions, "p", "2026-10-01T00-00-00-000Z_s1.jsonl", &jl(&[
        hdr(),
        a("a1", None, 0.0, 0, 10_000),
        asst("a2", Some("a1"), 400_000.0, "anthropic", "opus", API_ANTHROPIC, (10, 0, 500, 0), 0.01, "stop"),
    ]));
    let cache = dir.join("cache.json");
    let opts = ScanOptions { sessions_dir: &sessions, cache_path: &cache, include_subagents: false, on_progress: None };
    let r1 = scan_sessions(&opts);
    assert_eq!((r1.cache.len(), r1.cache[0].misses, r1.cache[0].miss_ttl, r1.cache[0].miss_tokens), (1, 1, 1, 10_000));
    assert!((r1.cache[0].miss_cost - 0.01).abs() < 1e-12);
    let r2 = scan_sessions(&opts);
    assert_eq!((r2.parsed, r2.cached), (0, 1));
    assert_eq!(r2.cache, r1.cache);
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(raw["version"], 5);
    assert_eq!(raw["files"].as_object().unwrap().values().next().unwrap()["cacheDaily"][0]["missTtl"], 1);
    // a v2 cache is dropped and re-parsed
    let mut v = raw;
    v["version"] = 2.into();
    std::fs::write(&cache, serde_json::to_vec(&v).unwrap()).unwrap();
    let r3 = scan_sessions(&opts);
    assert_eq!((r3.parsed, r3.cache), (1, r1.cache));
    let _ = std::fs::remove_dir_all(dir);
}

/* ------------------------------------------------------------------ break-even */

/// Assistant line with explicit `usage` (input, cacheRead, cacheWrite) and cost (cacheRead, cacheWrite); `cost_cr` None = field missing.
fn req(id: &str, parent: Option<&str>, start: f64, usage: (u64, u64, u64), cost_cr: Option<f64>, cost_cw: f64, stop: &str) -> String {
    let mut cost = serde_json::json!({"cacheWrite": cost_cw, "total": cost_cw});
    if let Some(c) = cost_cr {
        cost["cacheRead"] = c.into();
    }
    let msg = serde_json::json!({"role":"assistant","provider":"anthropic","model":"opus","api":API_ANTHROPIC,"stopReason":stop,"timestamp":T0 + start,
        "content":[],"usage":{"input":usage.0,"output":5,"cacheRead":usage.1,"cacheWrite":usage.2,"cost":cost}});
    format!(r#"{{"type":"message","id":"{id}","parentId":{},"timestamp":"{}","message":{msg}}}"#, parent_json(parent), iso_from_ms(T0 + start + 1000.0))
}

/// Cache-read request worth 30_000 tokens / $0.009.
fn rd(id: &str, parent: &str, start: f64) -> String {
    req(id, Some(parent), start, (10, 30_000, 0), Some(0.009), 0.0, "stop")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// `build_cache` over one session file's events.
fn cache_of(sp: SessionSpend) -> (Vec<CacheRow>, Vec<CacheEvent>) {
    let s = Scanned { entry: SessionEntry { id: "s1".into(), file: "C:/s/a.jsonl".into(), folder: "f".into(), project: "W/a".into(), ..Default::default() }, spend: sp };
    build_cache(&[s], &[])
}

#[test]
fn break_even_of_a_relay() {
    let sp = parse(vec![
        a("a1", None, 0.0, 0, 40_000),
        comp("c1", Some("a1"), 1_000.0, 100_000, true, 0),
        // first request after the relay: after = 30_000, recache 30_000 tokens = $0.15, reads nothing
        req("r1", Some("c1"), 2_000.0, (0, 0, 30_000), Some(0.0), 0.15, "stop"),
        rd("r2", "r1", 3_000.0),
        // invalid requests are not counted
        req("bad", Some("r2"), 3_500.0, (10, 30_000, 0), Some(9.0), 0.0, "error"),
        rd("r3", "bad", 4_000.0),
        rd("r4", "r3", 5_000.0),
    ]);
    let e = &sp.cache_events[0];
    assert_eq!((e.post_requests, e.post_cache_read, e.dropped), (4, 90_000, 70_000));
    let (rows, ev) = cache_of(sp);
    let e = &ev[0];
    // rate = 0.027 / 90_000 = 3e-7 $/token; savings = 4 * 70_000 * 3e-7
    assert_eq!(e.post_requests, 4);
    assert!(close(e.savings, 0.084), "{}", e.savings);
    assert!(close(e.net, 0.084 - 0.15), "{}", e.net);
    assert!(close(e.break_even.unwrap(), 0.15 / (70_000.0 * 3e-7)), "{:?}", e.break_even);
    assert!(e.open);
    // daily rows carry the relay sums only
    let r = &rows[0];
    assert_eq!((r.relays, r.relay_post_requests, r.compacts, r.compact_post_requests, r.compact_savings), (1, 4, 0, 0, 0.0));
    assert!(close(r.relay_savings, 0.084));
    let v = serde_json::to_value(&ev[0]).unwrap();
    assert_eq!((v["postRequests"].clone(), v["open"].clone()), (4.into(), true.into()));
    assert!(v["breakEven"].is_f64() && v["savings"].is_f64() && v["net"].is_f64());
}

#[test]
fn epochs_end_at_the_next_compaction_and_only_the_last_is_open() {
    let sp = parse(vec![
        comp("c1", None, 1_000.0, 100_000, true, 0),
        req("r1", Some("c1"), 2_000.0, (0, 0, 30_000), Some(0.0), 0.15, "stop"),
        rd("r2", "r1", 3_000.0),
        comp("c2", Some("r2"), 4_000.0, 90_000, false, 0),
        req("r3", Some("c2"), 5_000.0, (0, 0, 20_000), Some(0.0), 0.1, "stop"),
        rd("r4", "r3", 6_000.0),
        rd("r5", "r4", 7_000.0),
    ]);
    let (rows, ev) = cache_of(sp);
    assert_eq!(ev.len(), 2);
    assert_eq!((ev[0].kind.as_str(), ev[0].post_requests, ev[0].open), ("relay", 2, false));
    assert_eq!((ev[1].kind.as_str(), ev[1].post_requests, ev[1].open), ("compact", 3, true));
    // compact: rate 3e-7, dropped 70_000, savings = 3 * 70_000 * 3e-7 = 0.063 ... r3 reads 0 -> rate = 0.018 / 60_000
    assert!(close(ev[1].savings, 3.0 * 70_000.0 * 3e-7), "{}", ev[1].savings);
    assert!(close(ev[0].savings, 2.0 * 70_000.0 * 3e-7), "{}", ev[0].savings);
    let sum = |f: fn(&CacheRow) -> f64| rows.iter().map(f).sum::<f64>();
    assert!(close(sum(|r| r.relay_savings), ev[0].savings) && close(sum(|r| r.compact_savings), ev[1].savings));
    assert_eq!(rows.iter().map(|r| (r.relay_post_requests, r.compact_post_requests)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)), (2, 3));
}

#[test]
fn branches_count_on_their_own_ancestor_chain() {
    let sp = parse(vec![
        comp("c1", None, 1_000.0, 100_000, true, 0),
        req("r1", Some("c1"), 2_000.0, (0, 0, 30_000), Some(0.0), 0.15, "stop"),
        rd("r2", "r1", 3_000.0),
        // compaction on the r2 branch ...
        comp("c2", Some("r2"), 4_000.0, 90_000, false, 0),
        rd("r3", "c2", 5_000.0),
        // ... a sibling branch from r1 still belongs to the first epoch, including its descendants
        rd("b2", "r1", 6_000.0),
        rd("b3", "b2", 7_000.0),
    ]);
    let ev = &sp.cache_events;
    assert_eq!((ev[0].post_requests, ev[1].post_requests), (4, 1));
    assert_eq!(ev[0].post_cache_read, 90_000);
}

#[test]
fn break_even_unknown_without_dropped_or_read_cost() {
    // dropped == 0 (the context did not shrink)
    let sp = parse(vec![
        comp("c1", None, 1_000.0, 1_000, true, 0),
        req("r1", Some("c1"), 2_000.0, (0, 0, 30_000), Some(0.0), 0.15, "stop"),
        rd("r2", "r1", 3_000.0),
    ]);
    let (_, ev) = cache_of(sp);
    assert_eq!((ev[0].dropped, ev[0].post_requests, ev[0].break_even, ev[0].savings), (0, 2, None, 0.0));
    assert!(close(ev[0].net, -0.15));
    // cost.cacheRead missing: the rate is 0, so nothing is saved and the break-even is unknown
    let sp = parse(vec![
        comp("c1", None, 1_000.0, 100_000, true, 0),
        req("r1", Some("c1"), 2_000.0, (0, 0, 30_000), None, 0.15, "stop"),
        req("r2", Some("r1"), 3_000.0, (10, 30_000, 0), None, 0.0, "stop"),
    ]);
    let (_, ev) = cache_of(sp);
    assert_eq!((ev[0].dropped, ev[0].post_requests, ev[0].break_even, ev[0].savings), (70_000, 2, None, 0.0));
    // nothing read at all: rate unknown
    let sp = parse(vec![comp("c1", None, 1_000.0, 100_000, true, 0), req("r1", Some("c1"), 2_000.0, (0, 0, 30_000), Some(0.0), 0.15, "stop")]);
    let e = &sp.cache_events[0];
    assert_eq!((e.rate(), e.break_even(), e.savings()), (None, None, 0.0));
    // no later request at all
    let sp = parse(vec![comp("c1", None, 1_000.0, 100_000, true, 0)]);
    assert_eq!((sp.cache_events[0].post_requests, sp.cache_events[0].break_even()), (0, None));
}

/* ------------------------------------------------------------------ live (cache still alive at the event) */

fn a1h(id: &str, parent: Option<&str>, start: f64) -> String {
    asst(id, parent, start, "anthropic", "opus", API_ANTHROPIC, (10, 0, 40_000, 40_000), 0.0, "stop")
}

fn warm(id: &str, parent: &str, at: f64, cr: u64) -> String {
    format!(
        r#"{{"type":"usage","id":"{id}","parentId":"{parent}","timestamp":"{}","kind":"cache_warm","usage":{{"cacheRead":{cr},"cost":{{"total":0.01}}}}}}"#,
        iso_from_ms(T0 + at)
    )
}

const MIN: f64 = 60_000.0;

#[test]
fn live_one_hour_write_shortly_before_the_event() {
    let sp = parse(vec![a1h("a1", None, 0.0), comp("c1", Some("a1"), 10.0 * MIN, 100_000, true, 0)]);
    assert!(sp.cache_events[0].live);
}

#[test]
fn not_live_two_hours_later() {
    let sp = parse(vec![a1h("a1", None, 0.0), comp("c1", Some("a1"), 120.0 * MIN, 100_000, true, 0)]);
    assert!(!sp.cache_events[0].live);
}

#[test]
fn five_minute_tier_expires_but_a_warmer_keeps_it_alive() {
    let dead = parse(vec![a("a1", None, 0.0, 0, 40_000), comp("c1", Some("a1"), 6.0 * MIN, 100_000, false, 0)]);
    assert!(!dead.cache_events[0].live);
    let alive = parse(vec![
        a("a1", None, 0.0, 0, 40_000),
        warm("w1", "a1", 4.0 * MIN, 40_000),
        comp("c1", Some("w1"), 6.0 * MIN, 100_000, false, 0),
    ]);
    assert!(alive.cache_events[0].live);
    // a warmer that read nothing does not count; neither does a short expectation
    let empty = parse(vec![a("a1", None, 0.0, 0, 40_000), warm("w1", "a1", 4.0 * MIN, 0), comp("c1", Some("w1"), 6.0 * MIN, 100_000, false, 0)]);
    assert!(!empty.cache_events[0].live);
    let small = parse(vec![a("a1", None, 0.0, 0, 3_000), comp("c1", Some("a1"), 2.0 * MIN, 100_000, false, 0)]);
    assert!(!small.cache_events[0].live);
    // no predecessor
    assert!(!parse(vec![comp("c1", None, 1.0 * MIN, 100_000, true, 0)]).cache_events[0].live);
}

#[test]
fn non_anthropic_is_never_live() {
    let l = asst("a1", None, 0.0, "openai", "gpt", "openai-responses", (10, 0, 40_000, 0), 0.0, "stop");
    assert!(!parse(vec![l, comp("c1", Some("a1"), 1.0 * MIN, 100_000, true, 0)]).cache_events[0].live);
}

#[test]
fn live_splits_are_aggregated() {
    let sp = parse(vec![
        a1h("a1", None, 0.0),
        comp("c1", Some("a1"), 2.0 * MIN, 100_000, true, 0), // live relay
        req("r1", Some("c1"), 3.0 * MIN, (0, 0, 30_000), Some(0.0), 0.15, "stop"),
        rd("r2", "r1", 4.0 * MIN),
        comp("c2", Some("r2"), 400.0 * MIN, 100_000, true, 0), // dead relay
        req("r3", Some("c2"), 401.0 * MIN, (0, 0, 30_000), Some(0.0), 0.15, "stop"),
        rd("r4", "r3", 402.0 * MIN),
    ]);
    assert_eq!(sp.cache_events.iter().map(|e| e.live).collect::<Vec<_>>(), [true, false]);
    let (rows, ev) = cache_of(sp);
    assert_eq!(ev.iter().map(|e| e.live).collect::<Vec<_>>(), [true, false]);
    let mut sum = CacheRow::default();
    for x in &rows {
        sum.relays += x.relays;
        sum.relay_live += x.relay_live;
        sum.compact_live += x.compact_live;
        sum.relay_recache_cost += x.relay_recache_cost;
        sum.relay_live_recache_cost += x.relay_live_recache_cost;
        sum.relay_live_post_requests += x.relay_live_post_requests;
        sum.relay_live_savings += x.relay_live_savings;
    }
    let r = &sum;
    assert_eq!((r.relays, r.relay_live, r.compact_live), (2, 1, 0));
    assert!(close(r.relay_live_recache_cost, 0.15) && close(r.relay_recache_cost, 0.30), "{r:?}");
    assert_eq!(r.relay_live_post_requests, ev[0].post_requests);
    assert!(close(r.relay_live_savings, ev[0].savings), "{} {}", r.relay_live_savings, ev[0].savings);
    let v = serde_json::to_value(&ev[0]).unwrap();
    assert_eq!(v["live"], true);
    let v = serde_json::to_value(&rows[0]).unwrap();
    assert!(v["relayLive"].is_u64() && v["relayLiveRecacheCost"].is_f64() && v["compactLivePostRequests"].is_u64());
}
