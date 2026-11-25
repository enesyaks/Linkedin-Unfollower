//! Parse a `curl` command copied out of the browser's Network tab, and turn it
//! into a request template.
//!
//! Why this exists: LinkedIn's internal (Voyager) endpoints, query ids and
//! required headers change without notice. Every tool that hardcodes them is
//! broken within months. So we hardcode nothing — you capture one real
//! unfollow request yourself, mark the varying part with a placeholder, and
//! this module replays it.

use std::collections::HashMap;
use std::fmt;

use anyhow::{anyhow, bail, Context, Result};

/// Placeholders substituted per company.
pub const PLACEHOLDERS: &[&str] = &["{urn}", "{urn_enc}", "{id}", "{slug}", "{name}"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl Request {
    /// Where, if anywhere, a per-company placeholder appears.
    pub fn placeholder_sites(&self) -> Vec<String> {
        let mut sites = Vec::new();
        for p in PLACEHOLDERS {
            if self.url.contains(p) {
                sites.push(format!("url contains {p}"));
            }
            if self.body.as_deref().is_some_and(|b| b.contains(p)) {
                sites.push(format!("body contains {p}"));
            }
            for (name, value) in &self.headers {
                if value.contains(p) {
                    sites.push(format!("header {name} contains {p}"));
                }
            }
        }
        sites
    }

    /// Placeholder names (without braces) this template needs filled.
    pub fn required_vars(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let haystacks: Vec<&str> = std::iter::once(self.url.as_str())
            .chain(self.body.as_deref())
            .chain(self.headers.iter().map(|(_, v)| v.as_str()))
            .collect();
        for p in PLACEHOLDERS {
            let name = p.trim_matches(|c| c == '{' || c == '}');
            if haystacks.iter().any(|h| h.contains(p)) && !out.iter().any(|o| o == name) {
                out.push(name.to_string());
            }
        }
        out
    }

    /// Substitute the per-company values.
    pub fn render(&self, vars: &HashMap<&str, String>) -> Request {
        let sub = |s: &str| -> String {
            let mut out = s.to_string();
            for (key, value) in vars {
                out = out.replace(&format!("{{{key}}}"), value);
            }
            out
        };
        Request {
            method: self.method.clone(),
            url: sub(&self.url),
            headers: self
                .headers
                .iter()
                .map(|(n, v)| (n.clone(), sub(v)))
                .collect(),
            body: self.body.as_deref().map(sub),
        }
    }

    /// Headers safe to show a human: credentials are replaced, not printed.
    pub fn redacted_headers(&self) -> Vec<(String, String)> {
        self.headers
            .iter()
            .map(|(n, v)| (n.clone(), redact(n, v)))
            .collect()
    }
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} {}", self.method, self.url)?;
        for (n, v) in self.redacted_headers() {
            writeln!(f, "  {n}: {v}")?;
        }
        match &self.body {
            Some(b) if b.len() > 400 => writeln!(f, "  body: {} bytes", b.len()),
            Some(b) => writeln!(f, "  body: {b}"),
            None => Ok(()),
        }
    }
}

/// Anything that can authenticate as you never gets printed in full.
pub fn redact(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let secret = matches!(
        lower.as_str(),
        "cookie" | "authorization" | "csrf-token" | "x-li-csrf-token" | "set-cookie"
    );
    if !secret {
        return value.to_string();
    }
    format!("<redacted, {} bytes>", value.len())
}

/// Percent-encode for use inside a URL path or query value.
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Split a shell-ish command line the way Chrome/Firefox "Copy as cURL" writes it.
pub fn tokenize(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // Line continuations, in both the POSIX and the PowerShell dialect.
            '\\' if matches!(chars.peek(), Some('\n') | Some('\r')) => {
                chars.next();
            }
            '^' if matches!(chars.peek(), Some('\n') | Some('\r')) => {
                chars.next();
            }
            '`' if matches!(chars.peek(), Some('\n') | Some('\r')) => {
                chars.next();
            }
            c if c.is_whitespace() => {
                if has_token {
                    tokens.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            '\'' => {
                has_token = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                has_token = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(esc) = chars.next() {
                                match esc {
                                    'n' => cur.push('\n'),
                                    't' => cur.push('\t'),
                                    'r' => cur.push('\r'),
                                    other => cur.push(other),
                                }
                            }
                        }
                        other => cur.push(other),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                // ANSI-C quoting: $'line1\nline2'
                chars.next();
                has_token = true;
                while let Some(c) = chars.next() {
                    match c {
                        '\'' => break,
                        '\\' => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('t') => cur.push('\t'),
                            Some('r') => cur.push('\r'),
                            Some('\\') => cur.push('\\'),
                            Some('\'') => cur.push('\''),
                            Some('u') => {
                                let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                                let code = u32::from_str_radix(&hex, 16)
                                    .with_context(|| format!("bad \\u escape: \\u{hex}"))?;
                                cur.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            }
                            Some('x') => {
                                let hex: String = (0..2).filter_map(|_| chars.next()).collect();
                                let code = u32::from_str_radix(&hex, 16)
                                    .with_context(|| format!("bad \\x escape: \\x{hex}"))?;
                                cur.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            }
                            Some(other) => cur.push(other),
                            None => break,
                        },
                        other => cur.push(other),
                    }
                }
            }
            '\\' => {
                has_token = true;
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            other => {
                has_token = true;
                cur.push(other);
            }
        }
    }
    if has_token {
        tokens.push(cur);
    }
    if tokens.is_empty() {
        bail!("the request file is empty");
    }
    Ok(tokens)
}

/// Parse a captured `curl ...` command.
pub fn parse(input: &str) -> Result<Request> {
    // Strip comment lines so users can annotate their capture file.
    let cleaned: String = input
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let tokens = tokenize(&cleaned)?;
    let mut iter = tokens.iter().peekable();

    if iter.peek().is_some_and(|t| {
        let t = t.to_ascii_lowercase();
        t == "curl" || t.ends_with("/curl") || t.ends_with("curl.exe")
    }) {
        iter.next();
    } else {
        bail!("expected the capture to start with `curl` — paste the browser's \"Copy as cURL\" output verbatim");
    }

    let mut url: Option<String> = None;
    let mut method: Option<String> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut body: Option<String> = None;

    let need = |flag: &str, it: &mut std::iter::Peekable<std::slice::Iter<'_, String>>| {
        it.next()
            .cloned()
            .ok_or_else(|| anyhow!("{flag} needs a value"))
    };

    while let Some(tok) = iter.next() {
        match tok.as_str() {
            "-H" | "--header" => {
                let raw = need("-H", &mut iter)?;
                let (name, value) = raw
                    .split_once(':')
                    .ok_or_else(|| anyhow!("malformed header (no colon): {raw}"))?;
                let name = name.trim();
                // HTTP/2 pseudo-headers and hop-by-hop headers are the client's job.
                if name.starts_with(':')
                    || name.eq_ignore_ascii_case("content-length")
                    || name.eq_ignore_ascii_case("host")
                    || name.eq_ignore_ascii_case("connection")
                {
                    continue;
                }
                headers.push((name.to_string(), value.trim().to_string()));
            }
            "-X" | "--request" => method = Some(need("-X", &mut iter)?.to_uppercase()),
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => {
                let value = need(tok, &mut iter)?;
                match &mut body {
                    Some(existing) => {
                        existing.push('&');
                        existing.push_str(&value);
                    }
                    None => body = Some(value),
                }
            }
            "-b" | "--cookie" => {
                let value = need("-b", &mut iter)?;
                headers.push(("cookie".to_string(), value));
            }
            "-A" | "--user-agent" => {
                let value = need("-A", &mut iter)?;
                headers.push(("user-agent".to_string(), value));
            }
            "-e" | "--referer" => {
                let value = need("-e", &mut iter)?;
                headers.push(("referer".to_string(), value));
            }
            "--url" => url = Some(need("--url", &mut iter)?),
            // Flags we knowingly ignore: the Rust client handles these itself.
            "--compressed" | "-s" | "--silent" | "-i" | "--include" | "-k" | "--insecure"
            | "-L" | "--location" | "-v" | "--verbose" | "-g" | "--globoff" | "--no-keepalive"
            | "-f" | "--fail" => {}
            "-o" | "--output" | "--max-time" | "--connect-timeout" | "--retry" | "-m" => {
                let _ = iter.next();
            }
            other if other.starts_with("http://") || other.starts_with("https://") => {
                url = Some(other.to_string());
            }
            other if other.starts_with('-') => {
                // Unknown flag: skip it, and its value if the next token is not a flag.
                if iter.peek().is_some_and(|t| !t.starts_with('-')) {
                    let _ = iter.next();
                }
            }
            other => {
                if url.is_none() {
                    url = Some(format!("https://{other}"));
                }
            }
        }
    }

    let url = url.ok_or_else(|| anyhow!("no URL found in the captured command"))?;
    let method = method.unwrap_or_else(|| {
        if body.is_some() {
            "POST".to_string()
        } else {
            "GET".to_string()
        }
    });

    Ok(Request {
        method,
        url,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_CAPTURE: &str = r#"curl 'https://www.linkedin.com/voyager/api/example/follows?action=unfollow' \
  -H 'accept: application/json' \
  -H 'content-type: application/json; charset=UTF-8' \
  -H 'csrf-token: ajax:1234567890' \
  -H 'cookie: li_at=SECRETVALUE; JSESSIONID="ajax:1234567890"' \
  -H 'content-length: 61' \
  -H ':authority: www.linkedin.com' \
  --data-raw '{"urn":"{urn}"}' \
  --compressed"#;

    #[test]
    fn parses_a_chrome_capture() {
        let req = parse(CHROME_CAPTURE).expect("parses");
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.url,
            "https://www.linkedin.com/voyager/api/example/follows?action=unfollow"
        );
        assert_eq!(req.body.as_deref(), Some(r#"{"urn":"{urn}"}"#));
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"csrf-token"));
        assert!(names.contains(&"cookie"));
        // Hop-by-hop and pseudo-headers must be dropped.
        assert!(!names
            .iter()
            .any(|n| n.eq_ignore_ascii_case("content-length")));
        assert!(!names.iter().any(|n| n.starts_with(':')));
    }

    #[test]
    fn credentials_are_never_printed() {
        let req = parse(CHROME_CAPTURE).expect("parses");
        let shown = req.to_string();
        assert!(!shown.contains("SECRETVALUE"), "cookie leaked:\n{shown}");
        assert!(!shown.contains("ajax:1234567890"), "csrf leaked:\n{shown}");
        assert!(shown.contains("<redacted"));
    }

    #[test]
    fn reports_where_placeholders_live() {
        let req = parse(CHROME_CAPTURE).expect("parses");
        let sites = req.placeholder_sites();
        assert_eq!(sites.len(), 1);
        assert!(sites[0].contains("body"));

        let no_vars = parse("curl 'https://x.test/a' --data-raw '{\"urn\":\"fixed\"}'").unwrap();
        assert!(no_vars.placeholder_sites().is_empty());
    }

    #[test]
    fn required_vars_lists_what_the_list_must_provide() {
        let req = parse(
            "curl 'https://x.test/{urn_enc}' -H 'x-a: {slug}' --data-raw '{\"id\":\"{id}\"}'",
        )
        .unwrap();
        let mut vars = req.required_vars();
        vars.sort();
        assert_eq!(vars, vec!["id", "slug", "urn_enc"]);
        assert!(parse("curl 'https://x.test/a'")
            .unwrap()
            .required_vars()
            .is_empty());
    }

    #[test]
    fn renders_every_placeholder_site() {
        let req = parse(
            "curl 'https://x.test/follow/{urn_enc}' -H 'x-target: {slug}' --data-raw '{\"id\":\"{id}\"}'",
        )
        .unwrap();
        let vars = HashMap::from([
            ("urn_enc", percent_encode("urn:li:fsd_company:99")),
            ("slug", "acme".to_string()),
            ("id", "99".to_string()),
        ]);
        let out = req.render(&vars);
        assert_eq!(out.url, "https://x.test/follow/urn%3Ali%3Afsd_company%3A99");
        assert_eq!(out.headers[0].1, "acme");
        assert_eq!(out.body.as_deref(), Some(r#"{"id":"99"}"#));
    }

    #[test]
    fn handles_ansi_c_and_double_quoting() {
        let req = parse("curl \"https://x.test/a\" -X put --data $'line1\\nline2'").unwrap();
        assert_eq!(req.method, "PUT");
        assert_eq!(req.body.as_deref(), Some("line1\nline2"));
    }

    #[test]
    fn defaults_method_from_presence_of_a_body() {
        assert_eq!(parse("curl 'https://x.test/a'").unwrap().method, "GET");
        assert_eq!(
            parse("curl 'https://x.test/a' -d 'x=1'").unwrap().method,
            "POST"
        );
    }

    #[test]
    fn rejects_input_that_is_not_a_curl_command() {
        assert!(parse("GET /voyager/api HTTP/1.1").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn skips_comment_lines() {
        let req = parse("# captured 2026-11-25\ncurl 'https://x.test/a'").unwrap();
        assert_eq!(req.url, "https://x.test/a");
    }

    #[test]
    fn percent_encoding_matches_rfc3986_unreserved() {
        assert_eq!(percent_encode("urn:li:x_y-1.2~3"), "urn%3Ali%3Ax_y-1.2~3");
        assert_eq!(percent_encode("a b"), "a%20b");
    }
}
