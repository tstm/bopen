//! Pure routing: config text -> `Ruleset`, `Request` -> `Route`. No AppKit, no I/O except `load`.
//!
//! Semantics (the example config promises exactly this):
//! - rules are checked in file order; the first rule whose conditions ALL match wins;
//! - a list inside a condition matches if ANY item matches;
//! - no rule matched -> `default`.

use std::fmt;
use std::ops::Range;
use std::path::Path;

use serde::Deserialize;

use crate::browsers::Target;

// ---------- what a rule can see ----------

/// Everything known about one click, captured once at event time (macos.rs) or synthesized by `bopen test`.
#[derive(Debug, Clone)]
pub struct Request {
    pub link: Link,
    pub source: Option<SourceApp>,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceApp {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    /// How we learned it. Logged, never matched on.
    pub via: SourceVia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceVia {
    /// Apple Event keySenderPIDAttr resolved to a running app.
    Sender,
    /// Sender unusable; the frontmost app (not us) was used instead.
    Frontmost,
    /// `--from` on the command line.
    Cli,
}

impl fmt::Display for SourceApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let via = match self.via {
            SourceVia::Sender => "sender",
            SourceVia::Frontmost => "frontmost",
            SourceVia::Cli => "cli",
        };
        match (&self.bundle_id, &self.name) {
            (Some(id), Some(name)) if id != name => write!(f, "{id} ({name}, {via})"),
            (Some(s), _) | (None, Some(s)) => write!(f, "{s} ({via})"),
            (None, None) => write!(f, "? ({via})"),
        }
    }
}

/// Held modifier keys. Bitset so "all of these held" is one `contains`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const SHIFT: Self = Self(1);
    pub const CONTROL: Self = Self(2);
    pub const OPTION: Self = Self(4);
    pub const COMMAND: Self = Self(8);
    const NAMES: [(&'static str, Self); 4] =
        [("shift", Self::SHIFT), ("control", Self::CONTROL), ("option", Self::OPTION), ("command", Self::COMMAND)];

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn parse_one(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "shift" => Self::SHIFT,
            "control" | "ctrl" => Self::CONTROL,
            "option" | "alt" | "opt" => Self::OPTION,
            "command" | "cmd" => Self::COMMAND,
            _ => return None,
        })
    }

    /// Comma-separated list, as `bopen test --mods option,shift` takes it.
    pub fn parse_list(s: &str) -> Result<Self, String> {
        s.split(',').filter(|p| !p.trim().is_empty()).try_fold(Self::default(), |acc, p| {
            Self::parse_one(p.trim())
                .map(|m| acc.union(m))
                .ok_or_else(|| format!("unknown modifier {p:?} (use option, shift, command, control)"))
        })
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("-");
        }
        let names: Vec<_> = Self::NAMES.iter().filter(|(_, m)| self.contains(*m)).map(|(n, _)| *n).collect();
        f.write_str(&names.join("+"))
    }
}

// ---------- the link ----------

/// A URL split just enough to route on. Deliberately not a full URL parser: routing only needs
/// the host, path, and query, and the rewritten URL is validated by NSURL when it is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    raw: String,
    scheme: String,
    /// Lowercased, no port, no userinfo. None for schemes without an authority (mailto:, file:).
    host: Option<String>,
    /// Up to '?' or '#'. Empty when absent.
    path: String,
    /// Between '?' and '#', without the '?'. Empty when absent.
    query: String,
}

impl Link {
    /// Never fails: anything we can't split still routes (to `from`/`url` rules or the default).
    pub fn parse(raw: &str) -> Link {
        let raw = raw.trim().to_string();
        let (scheme, rest) = match raw.split_once(':') {
            Some((s, rest))
                if !s.is_empty()
                    && s.starts_with(|c: char| c.is_ascii_alphabetic())
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
            {
                (s.to_ascii_lowercase(), rest)
            }
            _ => (String::new(), raw.as_str()),
        };
        let (host, rest) = match rest.strip_prefix("//") {
            Some(after) => {
                let end = after.find(['/', '?', '#']).unwrap_or(after.len());
                (Some(host_of(&after[..end])), &after[end..])
            }
            None => (None, rest),
        };
        let rest = rest.split('#').next().unwrap_or("");
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        Link { scheme, host, path: path.to_string(), query: query.to_string(), raw }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// Without query and fragment: what the log records, so tokens in links never land on disk.
    pub fn redacted(&self) -> &str {
        let end = self.raw.find(['?', '#']).unwrap_or(self.raw.len());
        &self.raw[..end]
    }
}

/// "user:pw@Example.COM:8080" -> "example.com"; "[::1]:80" -> "[::1]".
fn host_of(authority: &str) -> String {
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if hostport.starts_with('[') {
        hostport.find(']').map_or(hostport, |i| &hostport[..=i])
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

// ---------- the ruleset ----------

/// Validated, ready-to-route config. Only constructible via `parse`/`load`, so every
/// invariant listed on `parse` holds for any `Ruleset` in hand.
#[derive(Debug)]
pub struct Ruleset {
    rules: Vec<Rule>,
    default: Target,
}

#[derive(Debug)]
struct Rule {
    name: Option<String>,
    cond: Conditions,
    rewrite: Option<Template>,
    open: Target,
}

/// AND across fields; each Vec is OR. Invariant: at least one field is non-empty.
#[derive(Debug, Default)]
struct Conditions {
    from: Vec<String>,
    host: Vec<HostPattern>,
    path: Vec<Glob>,
    url: Vec<Glob>,
    /// Empty = no condition.
    modifiers: Modifiers,
}

#[derive(Debug)]
enum HostPattern {
    /// "example.com": equals, or ends with ".example.com".
    Domain(String),
    /// "=example.com": equals only.
    Exact(String),
    /// Contains '*': glob over the whole host.
    Glob(Glob),
}

/// `*` matches any run of characters (including '/'), ASCII-case-insensitively.
/// Each `*` is a numbered capture of the original text.
#[derive(Debug)]
struct Glob {
    /// Literal segments between stars; `stars() == parts.len() - 1`.
    parts: Vec<String>,
}

/// Parsed `rewrite` string. Unknown placeholders are a parse error, not literal text.
#[derive(Debug)]
struct Template(Vec<Piece>);

#[derive(Debug, PartialEq)]
enum Piece {
    Lit(String),
    Url,
    Scheme,
    Host,
    Path,
    Query,
    /// 1-based path-glob capture.
    Capture(usize),
}

/// The decision for one request.
#[derive(Debug)]
pub struct Route<'a> {
    pub rule: RuleRef,
    /// Rewritten if the rule had `rewrite`, else the request URL.
    pub url: String,
    pub target: &'a Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleRef {
    /// 0-based position in the file, and its `name` if any.
    Index {
        index: usize,
        name: Option<String>,
    },
    Default,
}

impl fmt::Display for RuleRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleRef::Index { index, name: Some(n) } => write!(f, "rule #{} {n:?}", index + 1),
            RuleRef::Index { index, name: None } => write!(f, "rule #{}", index + 1),
            RuleRef::Default => f.write_str("default"),
        }
    }
}

#[derive(Debug)]
pub struct ConfigError {
    /// 1-based line, when known.
    pub line: Option<usize>,
    pub rule: Option<RuleRef>,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(l) = self.line {
            write!(f, "line {l}: ")?;
        }
        if let Some(r) = &self.rule {
            write!(f, "{r}: ")?;
        }
        f.write_str(&self.message)
    }
}

// ---------- wire format (private, per boundary-discipline) ----------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDto {
    default: Option<toml::Spanned<String>>,
    #[serde(default)]
    rule: Vec<toml::Spanned<RuleDto>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDto {
    name: Option<String>,
    #[serde(default)]
    from: OneOrMany,
    #[serde(default)]
    host: OneOrMany,
    #[serde(default)]
    path: OneOrMany,
    #[serde(default)]
    url: OneOrMany,
    #[serde(default)]
    modifiers: OneOrMany,
    rewrite: Option<String>,
    open: String,
}

#[derive(Deserialize, Default)]
#[serde(untagged)]
enum OneOrMany {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::None => vec![],
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

impl Ruleset {
    /// What the agent uses when no config has ever parsed: no rules, default Safari.
    pub fn builtin() -> Self {
        Ruleset { rules: vec![], default: Target::parse("safari").expect("safari is known") }
    }

    /// Parse and validate. Rejects, with the line and rule:
    /// - unknown keys (typos must not silently disable a rule);
    /// - a rule with zero conditions (it would shadow everything below; that's `default`'s job);
    /// - unknown modifier names; empty patterns;
    /// - unknown `rewrite` placeholders, or `{n}` beyond the stars of any of the rule's path patterns;
    /// - targets rejected by `Target::parse` (unknown browser, profile on Safari, bopen itself).
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let line_of = |span: Range<usize>| Some(text[..span.start.min(text.len())].matches('\n').count() + 1);
        let file: FileDto = toml::from_str(text).map_err(|e| ConfigError {
            line: e.span().and_then(line_of),
            rule: None,
            message: e.message().to_string(),
        })?;

        let default = match file.default {
            None => Target::parse("safari").expect("safari is known"),
            Some(d) => Target::parse(d.get_ref()).map_err(|message| ConfigError {
                line: line_of(d.span()),
                rule: Some(RuleRef::Default),
                message,
            })?,
        };

        let rules = file
            .rule
            .into_iter()
            .enumerate()
            .map(|(index, spanned)| {
                let span = spanned.span();
                let dto = spanned.into_inner();
                let rule_ref = RuleRef::Index { index, name: dto.name.clone() };
                Rule::from_dto(dto).map_err(|message| ConfigError {
                    line: line_of(span.clone()),
                    rule: Some(rule_ref),
                    message,
                })
            })
            .collect::<Result<_, _>>()?;

        Ok(Ruleset { rules, default })
    }

    /// Read + parse. A missing file is `Ok(None)`; callers decide what that means.
    pub fn load(path: &Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError { line: None, rule: None, message: format!("{}: {e}", path.display()) }),
        }
    }

    /// First match wins.
    pub fn route(&self, req: &Request) -> Route<'_> {
        for (index, rule) in self.rules.iter().enumerate() {
            if let Some(caps) = rule.cond.matches(req) {
                let url = match &rule.rewrite {
                    Some(t) => t.expand(&req.link, &caps),
                    None => req.link.raw.clone(),
                };
                return Route { rule: RuleRef::Index { index, name: rule.name.clone() }, url, target: &rule.open };
            }
        }
        Route { rule: RuleRef::Default, url: req.link.raw.clone(), target: &self.default }
    }

    /// Every target in the file (default first), for `bopen check`.
    pub fn targets(&self) -> impl Iterator<Item = (RuleRef, &Target)> {
        std::iter::once((RuleRef::Default, &self.default)).chain(
            self.rules.iter().enumerate().map(|(index, r)| (RuleRef::Index { index, name: r.name.clone() }, &r.open)),
        )
    }

    pub fn default_target(&self) -> &Target {
        &self.default
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }
}

impl Rule {
    fn from_dto(dto: RuleDto) -> Result<Rule, String> {
        let nonempty = |field: &str, v: Vec<String>| -> Result<Vec<String>, String> {
            match v.iter().any(|s| s.trim().is_empty()) {
                true => Err(format!("`{field}` contains an empty pattern")),
                false => Ok(v),
            }
        };
        let modifiers = dto.modifiers.into_vec().iter().try_fold(Modifiers::default(), |acc, m| {
            Modifiers::parse_one(m)
                .map(|x| acc.union(x))
                .ok_or_else(|| format!("unknown modifier {m:?} (use option, shift, command, control)"))
        })?;
        let cond = Conditions {
            from: nonempty("from", dto.from.into_vec())?,
            host: nonempty("host", dto.host.into_vec())?.iter().map(|h| HostPattern::parse(h)).collect(),
            path: nonempty("path", dto.path.into_vec())?.iter().map(|p| Glob::compile(p)).collect(),
            url: nonempty("url", dto.url.into_vec())?.iter().map(|p| Glob::compile(p)).collect(),
            modifiers,
        };
        if cond.from.is_empty()
            && cond.host.is_empty()
            && cond.path.is_empty()
            && cond.url.is_empty()
            && modifiers.is_empty()
        {
            return Err("rule has no conditions (use `default = ...` for a catch-all)".into());
        }

        let rewrite = dto.rewrite.as_deref().map(Template::parse).transpose()?;
        if let Some(t) = &rewrite {
            let need = t.max_capture();
            if need > 0 {
                if cond.path.is_empty() {
                    return Err(format!("`rewrite` uses {{{need}}} but the rule has no `path` pattern to capture it"));
                }
                if let Some(short) = cond.path.iter().find(|g| g.stars() < need) {
                    return Err(format!(
                        "`rewrite` uses {{{need}}} but path {:?} has only {} `*`",
                        short.source(),
                        short.stars()
                    ));
                }
            }
        }

        Ok(Rule { name: dto.name, cond, rewrite, open: Target::parse(&dto.open)? })
    }
}

impl Conditions {
    /// `Some(captures)` if every non-empty condition matches. Captures come from the first
    /// matching path glob (empty if there's no path condition).
    fn matches(&self, req: &Request) -> Option<Vec<String>> {
        if !req.modifiers.contains(self.modifiers) {
            return None;
        }
        if !self.from.is_empty() {
            let src = req.source.as_ref()?;
            let hit =
                |v: &Option<String>| v.as_deref().is_some_and(|v| self.from.iter().any(|p| p.eq_ignore_ascii_case(v)));
            if !hit(&src.bundle_id) && !hit(&src.name) {
                return None;
            }
        }
        if !self.host.is_empty() {
            let host = req.link.host()?;
            if !self.host.iter().any(|p| p.matches(host)) {
                return None;
            }
        }
        if !self.url.is_empty() && !self.url.iter().any(|g| g.captures(&req.link.raw).is_some()) {
            return None;
        }
        if self.path.is_empty() {
            return Some(vec![]);
        }
        self.path.iter().find_map(|g| g.captures(&req.link.path))
    }
}

impl HostPattern {
    fn parse(s: &str) -> Self {
        let s = s.trim().to_ascii_lowercase();
        if let Some(exact) = s.strip_prefix('=') {
            HostPattern::Exact(exact.to_string())
        } else if s.contains('*') {
            HostPattern::Glob(Glob::compile(&s))
        } else {
            HostPattern::Domain(s)
        }
    }

    /// `host` is already lowercase.
    fn matches(&self, host: &str) -> bool {
        match self {
            HostPattern::Exact(h) => host == h,
            HostPattern::Domain(d) => host == d || host.strip_suffix(d.as_str()).is_some_and(|p| p.ends_with('.')),
            HostPattern::Glob(g) => g.captures(host).is_some(),
        }
    }
}

impl Glob {
    fn compile(pattern: &str) -> Self {
        Glob { parts: pattern.split('*').map(str::to_string).collect() }
    }

    fn stars(&self) -> usize {
        self.parts.len() - 1
    }

    fn source(&self) -> String {
        self.parts.join("*")
    }

    /// Anchored at both ends; shortest match per star, backtracking on failure (patterns are tiny).
    fn captures(&self, s: &str) -> Option<Vec<String>> {
        fn starts_with_ci(s: &str, lit: &str) -> bool {
            s.len() >= lit.len() && s.as_bytes()[..lit.len()].eq_ignore_ascii_case(lit.as_bytes())
        }
        fn go(parts: &[String], s: &str, caps: &mut Vec<String>) -> bool {
            let (first, rest) = parts.split_first().expect("glob has at least one part");
            if !starts_with_ci(s, first) {
                return false;
            }
            let s = &s[first.len()..];
            if rest.is_empty() {
                return s.is_empty();
            }
            for i in (0..=s.len()).filter(|&i| s.is_char_boundary(i)) {
                caps.push(s[..i].to_string());
                if go(rest, &s[i..], caps) {
                    return true;
                }
                caps.pop();
            }
            false
        }
        let mut caps = Vec::with_capacity(self.stars());
        go(&self.parts, s, &mut caps).then_some(caps)
    }
}

impl Template {
    fn parse(s: &str) -> Result<Self, String> {
        let mut pieces = vec![];
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            if open > 0 {
                pieces.push(Piece::Lit(rest[..open].to_string()));
            }
            let close = rest[open..].find('}').ok_or_else(|| format!("unclosed `{{` in rewrite {s:?}"))? + open;
            let name = &rest[open + 1..close];
            pieces.push(match name {
                "url" => Piece::Url,
                "scheme" => Piece::Scheme,
                "host" => Piece::Host,
                "path" => Piece::Path,
                "query" => Piece::Query,
                n => match n.parse::<usize>() {
                    Ok(i @ 1..=9) => Piece::Capture(i),
                    _ => {
                        return Err(format!(
                            "unknown placeholder {{{n}}} in rewrite (use url, scheme, host, path, query, 1-9)"
                        ));
                    }
                },
            });
            rest = &rest[close + 1..];
        }
        if !rest.is_empty() {
            pieces.push(Piece::Lit(rest.to_string()));
        }
        Ok(Template(pieces))
    }

    fn max_capture(&self) -> usize {
        self.0.iter().filter_map(|p| if let Piece::Capture(i) = p { Some(*i) } else { None }).max().unwrap_or(0)
    }

    /// Captures are guaranteed long enough by `Rule::from_dto`.
    fn expand(&self, link: &Link, caps: &[String]) -> String {
        self.0
            .iter()
            .map(|p| match p {
                Piece::Lit(s) => s.as_str(),
                Piece::Url => &link.raw,
                Piece::Scheme => &link.scheme,
                Piece::Host => link.host.as_deref().unwrap_or(""),
                Piece::Path => &link.path,
                Piece::Query => &link.query,
                Piece::Capture(i) => caps.get(i - 1).map_or("", String::as_str),
            })
            .collect()
    }
}

/// The commented example config. Written by `bopen install`, and the first test fixture.
pub const EXAMPLE_CONFIG: &str = include_str!("../example-config.toml");

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str, from: Option<&str>, mods: &str) -> Request {
        Request {
            link: Link::parse(url),
            source: from.map(|f| SourceApp { bundle_id: Some(f.into()), name: None, via: SourceVia::Cli }),
            modifiers: Modifiers::parse_list(mods).unwrap(),
        }
    }

    fn example() -> Ruleset {
        Ruleset::parse(EXAMPLE_CONFIG).unwrap()
    }

    fn routed(rs: &Ruleset, url: &str, from: Option<&str>, mods: &str) -> (RuleRef, String, String) {
        let r = rs.route(&req(url, from, mods));
        (r.rule, r.url, r.target.to_string())
    }

    fn rule_name(r: &RuleRef) -> &str {
        match r {
            RuleRef::Index { name: Some(n), .. } => n,
            _ => "<default>",
        }
    }

    #[test]
    fn example_config_parses() {
        assert!(example().len() >= 5);
    }

    #[test]
    fn zoom_from_slack_hits_zoom_rule_and_rewrites() {
        let (rule, url, target) =
            routed(&example(), "https://acme.zoom.us/j/99?pwd=s", Some("com.tinyspeck.slackmacgap"), "");
        assert_eq!(rule_name(&rule), "zoom in zoom.app");
        assert_eq!(url, "zoommtg://zoom.us/join?confno=99&pwd=s");
        assert_eq!(target, "us.zoom.xos");
    }

    #[test]
    fn from_matches_name_or_bundle_id_case_insensitively() {
        let rs = example();
        let mut r = req("https://example.org/", None, "");
        r.source = Some(SourceApp {
            bundle_id: Some("com.tinyspeck.slackmacgap".into()),
            name: Some("Slack".into()),
            via: SourceVia::Sender,
        });
        assert_eq!(rs.route(&r).target.to_string(), "chrome:Work");
        assert_eq!(routed(&rs, "https://example.org/", Some("COM.APPLE.MAIL"), "").2, "safari");
    }

    #[test]
    fn modifiers_must_all_be_held_extra_ones_are_fine() {
        let rs = example();
        assert_eq!(
            rule_name(&routed(&rs, "https://acme.com/", Some("Slack"), "option,shift").0),
            "option forces safari"
        );
        assert_eq!(rule_name(&routed(&rs, "https://acme.com/", Some("Slack"), "shift").0), "work apps");
    }

    #[test]
    fn host_domain_matches_subdomains_but_not_lookalikes() {
        let p = HostPattern::parse("zoom.us");
        assert!(p.matches("zoom.us") && p.matches("acme.zoom.us"));
        assert!(!p.matches("notzoom.us") && !p.matches("zoom.us.evil.com"));
        let e = HostPattern::parse("=github.com");
        assert!(e.matches("github.com") && !e.matches("gist.github.com"));
        assert!(HostPattern::parse("*.atlassian.net").matches("acme.atlassian.net"));
    }

    #[test]
    fn url_glob_rule() {
        let rs = example();
        assert_eq!(rule_name(&routed(&rs, "https://github.com/acme/api/pull/1", None, "").0), "work github");
        assert_eq!(routed(&rs, "https://github.com/other", None, "").0, RuleRef::Default);
    }

    #[test]
    fn unknown_source_never_matches_from() {
        assert_eq!(routed(&example(), "https://example.org/", None, "").0, RuleRef::Default);
    }

    #[test]
    fn link_parse_edge_cases() {
        let l = Link::parse("https://user:pw@Example.COM:8443/a/b?x=1#frag");
        assert_eq!((l.host(), l.path.as_str(), l.query.as_str()), (Some("example.com"), "/a/b", "x=1"));
        assert_eq!(l.redacted(), "https://user:pw@Example.COM:8443/a/b");
        assert_eq!(Link::parse("http://[::1]:80/").host(), Some("[::1]"));
        assert_eq!(Link::parse("mailto:a@b.c").host(), None);
        assert_eq!(Link::parse("https://example.com").path, "");
        assert_eq!(Link::parse("garbage").host(), None);
    }

    #[test]
    fn glob_captures_shortest_with_backtracking() {
        let g = Glob::compile("/*/x/*");
        assert_eq!(g.captures("/a/x/b/x/c"), Some(vec!["a".into(), "b/x/c".into()]));
        assert_eq!(Glob::compile("/J/*").captures("/j/123"), Some(vec!["123".into()]));
        assert_eq!(Glob::compile("/j/*").captures("/k/1"), None);
    }

    fn err(text: &str) -> String {
        Ruleset::parse(text).unwrap_err().to_string()
    }

    #[test]
    fn rejects_rule_without_conditions_with_line() {
        let e = err("default = \"safari\"\n\n[[rule]]\nopen = \"chrome\"\n");
        assert!(e.contains("no conditions") && e.contains("line 3"), "{e}");
    }

    #[test]
    fn rejects_typo_keys() {
        assert!(err("[[rule]]\nhots = \"a.com\"\nopen = \"safari\"\n").contains("hots"));
    }

    #[test]
    fn rejects_capture_without_enough_stars() {
        let e = err("[[rule]]\nhost = \"zoom.us\"\npath = \"/j/*\"\nrewrite = \"x://{2}\"\nopen = \"safari\"\n");
        assert!(e.contains("{2}"), "{e}");
        let e = err("[[rule]]\nhost = \"zoom.us\"\nrewrite = \"x://{1}\"\nopen = \"safari\"\n");
        assert!(e.contains("no `path`"), "{e}");
        assert!(err("[[rule]]\nhost = \"a\"\nrewrite = \"{nope}\"\nopen = \"safari\"\n").contains("nope"));
    }

    #[test]
    fn rejects_bad_modifier_and_self_target() {
        assert!(err("[[rule]]\nmodifiers = \"hyper\"\nopen = \"safari\"\n").contains("hyper"));
        assert!(err("[[rule]]\nhost = \"a.com\"\nopen = \"dev.bopen\"\n").contains("bopen"));
        assert!(err("default = \"safari:Work\"\n").contains("profile"));
    }
}
