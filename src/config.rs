//! Config file, CLI argument parsing and agent-dir resolution.
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub include_subagents: bool,
    pub port: u16,
}

pub const DEFAULT_CONFIG: Config = Config { include_subagents: false, port: 0 };

/// Same rules as the TS `parseConfig`: unknown/invalid values fall back to the defaults.
pub fn parse_config(raw: &Value) -> Config {
    let o = match raw.as_object() {
        Some(o) => o,
        None => return DEFAULT_CONFIG,
    };
    let port = match o.get("port") {
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                if u <= 65535 { Some(u as u16) } else { None }
            } else if let Some(f) = n.as_f64() {
                // JS: Number.isInteger(8123.0) is true
                if f.fract() == 0.0 && (0.0..=65535.0).contains(&f) { Some(f as u16) } else { None }
            } else {
                None
            }
        }
        _ => None,
    };
    Config {
        include_subagents: o.get("includeSubagents") == Some(&Value::Bool(true)),
        port: port.unwrap_or(DEFAULT_CONFIG.port),
    }
}

pub fn load_config(agent_dir: &std::path::Path) -> Config {
    let path = agent_dir.join("session-history.json");
    match std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(s.trim_start_matches('\u{feff}')).ok()) {
        Some(v) => parse_config(&v),
        None => DEFAULT_CONFIG,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedArgs {
    Stop,
    Open { project: String, all: bool },
}

/// `stop` (alone) stops; the word `all` anywhere is special; the rest is the project substring.
pub fn parse_args(args: &str) -> ParsedArgs {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.len() == 1 && tokens[0].eq_ignore_ascii_case("stop") {
        return ParsedArgs::Stop;
    }
    let mut rest: Vec<&str> = Vec::new();
    let mut all = false;
    for t in tokens {
        if t.eq_ignore_ascii_case("all") {
            all = true;
        } else {
            rest.push(t);
        }
    }
    let joined = rest.join(" ");
    let project = if joined.len() >= 2 && joined.starts_with('"') && joined.ends_with('"') {
        joined[1..joined.len() - 1].to_string()
    } else {
        joined
    };
    ParsedArgs::Open { project, all }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_args_cases() {
        let open = |p: &str, all: bool| ParsedArgs::Open { project: p.to_string(), all };
        assert_eq!(parse_args(""), open("", false));
        assert_eq!(parse_args("stop"), ParsedArgs::Stop);
        assert_eq!(parse_args(" STOP "), ParsedArgs::Stop);
        assert_eq!(parse_args("all"), open("", true));
        assert_eq!(parse_args("my-proj"), open("my-proj", false));
        assert_eq!(parse_args("all my proj"), open("my proj", true));
        assert_eq!(parse_args("\"x y\""), open("x y", false));
        assert_eq!(parse_args("stop now"), open("stop now", false));
    }

    #[test]
    fn parse_config_defaults_and_overrides() {
        assert_eq!(parse_config(&Value::Null), DEFAULT_CONFIG);
        assert_eq!(parse_config(&json!("x")), DEFAULT_CONFIG);
        assert_eq!(parse_config(&json!({"includeSubagents": "yes", "port": -1})), DEFAULT_CONFIG);
        assert_eq!(parse_config(&json!({"port": 70000})), DEFAULT_CONFIG);
        assert_eq!(parse_config(&json!({"port": 1.5})), DEFAULT_CONFIG);
        assert_eq!(
            parse_config(&json!({"includeSubagents": true, "port": 8123})),
            Config { include_subagents: true, port: 8123 }
        );
        assert_eq!(parse_config(&serde_json::from_str::<Value>("{\"port\": 8123.0}").unwrap()).port, 8123);
    }
}
