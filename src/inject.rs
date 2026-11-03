//! Serve-time decoration of the `pi --export` HTML: our CSS goes in just before `</head>`, our JS just before
//! `</body>`. The cached export file is never modified, so editing inject.css / inject.js needs no re-export.
//! Set `PI_SESSION_HISTORY_NO_INJECT=1` to serve the plain export.

const CSS: &str = include_str!("inject.css");
const JS: &str = include_str!("inject.js");
const HEAD_JS: &str = include_str!("inject_head.js");

/// Present in everything we inject; used to avoid double injection.
pub const MARKER: &str = "<!--pi-history-inject-->";

pub fn disabled() -> bool {
    std::env::var("PI_SESSION_HISTORY_NO_INJECT").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// Insert the style before the first `</head>` (else at the very start) and the script before the
/// last `</body>` (else at the very end). Already-injected input is returned unchanged.
#[allow(dead_code)]
pub fn inject(html: &str) -> String {
    inject_with(html, None)
}

/// Like [`inject`], plus the saved font preferences: a `<style id="pi-prefs">` (applied before first paint, no
/// flicker), the same values as JSON for the Aa popover, and the head helper script.
pub fn inject_with(html: &str, prefs: Option<&crate::prefs::Prefs>) -> String {
    if html.contains(MARKER) {
        return html.to_string();
    }
    let mut style = format!("{MARKER}<style data-pi-history>\n{CSS}\n</style>\n");
    if let Some(p) = prefs {
        if let Some(css) = p.css() {
            style.push_str(&format!("<style id=\"pi-prefs\" data-pi-history>{css}</style>\n"));
        }
        // present even when empty ("reset"): tells the page the server is authoritative, so no localStorage fallback.
        // values are allowlisted already (no `<`); the replace is belt and braces
        let json = serde_json::to_string(p).unwrap_or_else(|_| "{}".into()).replace('<', "\\u003c");
        style.push_str(&format!("<script type=\"application/json\" id=\"pi-prefs-json\" data-pi-history>{json}</script>\n"));
    }
    style.push_str(&format!("<script data-pi-head>\n{HEAD_JS}\n</script>\n"));
    let script = format!("<script data-pi-history>\n{JS}\n</script>\n");
    let head_at = html.find("</head>").unwrap_or(0);
    let mut out = String::with_capacity(html.len() + style.len() + script.len());
    out.push_str(&html[..head_at]);
    out.push_str(&style);
    let rest = &html[head_at..];
    // search the body marker only after the head insertion point (offsets relative to `rest`)
    match rest.rfind("</body>") {
        Some(i) => {
            out.push_str(&rest[..i]);
            out.push_str(&script);
            out.push_str(&rest[i..]);
        }
        None => {
            out.push_str(rest);
            out.push_str(&script);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_before_head_and_body_end() {
        let out = inject("<html><head><title>x</title></head><body><p>hi</p></body></html>");
        let style = out.find("<style data-pi-history>").unwrap();
        let head_end = out.find("</head>").unwrap();
        let script = out.find("<script data-pi-history>").unwrap();
        let body_end = out.find("</body>").unwrap();
        assert!(out.find("<title>").unwrap() < style && style < head_end && head_end < script && script < body_end);
        assert!(out.ends_with("</body></html>"));
        assert!(out.contains("<p>hi</p>"));
    }

    #[test]
    fn fallbacks_without_head_or_body() {
        let out = inject("<p>raw</p>");
        assert!(out.starts_with(MARKER));
        assert!(out.find("<style data-pi-history>").unwrap() < out.find("<p>raw</p>").unwrap());
        assert!(out.ends_with("</script>\n"));
        assert!(out.find("<p>raw</p>").unwrap() < out.find("<script data-pi-history>").unwrap());
    }

    #[test]
    fn uses_last_body_end_and_first_head_end() {
        let out = inject("<head></head><body><script>var s='</body>';</script></body>");
        let script = out.find("<script data-pi-history>").unwrap();
        assert!(out.find("var s='</body>'").unwrap() < script);
        assert!(out.ends_with("</body>"));
        assert_eq!(out.matches("<style data-pi-history>").count(), 1);
    }

    #[test]
    fn idempotent() {
        let once = inject("<head></head><body></body>");
        assert_eq!(inject(&once), once);
    }

    #[test]
    fn assets_cannot_break_out_of_their_tags() {
        assert!(!CSS.to_ascii_lowercase().contains("</style"));
        assert!(!JS.to_ascii_lowercase().contains("</script"));
        assert!(!JS.contains("<!--"));
        assert!(!HEAD_JS.to_ascii_lowercase().contains("</script") && !HEAD_JS.contains("<!--"));
    }

    #[test]
    fn prefs_are_embedded_in_head_only_when_set() {
        let html = "<html><head></head><body></body></html>";
        let plain = inject_with(html, None);
        assert!(!plain.contains("<style id=\"pi-prefs\""));
        let p = crate::prefs::Prefs::from_value(&serde_json::json!({"font": "Foo Bar", "size": 17, "mono": "x</style><script>alert(1)</script>"}));
        let out = inject_with(html, Some(&p));
        let at = out.find("<style id=\"pi-prefs\"").unwrap();
        assert!(out.contains(":root{--ph-font:Foo Bar,var(--pi-sans);--ph-font-size:17px}</style>"));
        assert!(at < out.find("</head>").unwrap() && out.find("</head>").unwrap() < out.find("<script data-pi-history>
/* pi-history: injected").unwrap());
        assert!(!out.contains("alert(1)"));
        assert_eq!(out.matches("<script").count(), 3);
        // server-side reset (empty prefs): no style, but the (empty) JSON marker so the page skips its localStorage fallback
        let empty = inject_with(html, Some(&crate::prefs::Prefs::default()));
        assert!(!empty.contains("<style id=\"pi-prefs\"") && empty.contains("id=\"pi-prefs-json\" data-pi-history>{}</script>"));
        assert!(!plain.contains("id=\"pi-prefs-json\""));
    }
}
