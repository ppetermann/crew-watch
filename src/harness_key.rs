//! Where each agent harness keeps its OpenRouter key, read-only.
//!
//! When no OpenRouter management key is configured, the quota row can still
//! show what the key of the harness in use reads about itself (its limit and
//! spend). This module finds that key: one row per runtime in
//! [`crate::detect::AGENT_KINDS`], each reading the harness's own credential
//! file, configuration file or environment in the harness's own order (the
//! first source that is set is the key the harness sends).
//!
//! ## Rules
//!
//! - **A key is taken only where its harness sends it to OpenRouter.** The base
//!   URL beside it must be `https` on `openrouter.ai` or a subdomain
//!   ([`is_openrouter_url`]); a built-in OpenRouter provider pointed at another
//!   host (a proxy) yields no key. Without this rule a proxy's key or an
//!   Anthropic token could be sent to OpenRouter.
//! - **Commands are never run.** pi's `!command`, Claude Code's
//!   `apiKeyHelper` and Codex's `auth` command produce a key by running a
//!   program; the outcome is "not readable".
//! - **Environment references are followed only as a whole value**
//!   (`$NAME`, `${NAME}`, `{env:NAME}`, and the variable names in `env_key` /
//!   `api_key_env`). A value mixing text and references is "not readable".
//! - **Files are read fresh on every lookup and capped in size**; a file that
//!   does not parse is "not readable".
//! - **The key never leaves the crate**: [`lookup_outcome`] reports only the
//!   harness, the kind of source and the outcome. The key itself is returned
//!   only to the reader in [`crate::openrouter`], wrapped in the redacting
//!   [`OpenRouterKey`].
//!
//! Not modelled: a key given on the harness's command line (pi's
//! `--api-key`), and administrator-managed settings files (Claude Code, Grok
//! Build).
//!
//! The home directory and the environment come in as a [`HarnessEnv`], so no
//! test reads the real home or the real process environment.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value as Json;

use crate::openrouter::OpenRouterKey;

/// Largest harness file the lookup reads. Credential and config files are a
/// few KiB; anything larger is not one.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The home directory and environment a lookup reads, passed in so the
/// lookup is testable without the real home or process environment.
#[derive(Clone, Default)]
pub struct HarnessEnv {
    home: Option<PathBuf>,
    vars: HashMap<String, String>,
}

impl fmt::Debug for HarnessEnv {
    // The environment can hold secrets: never print values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessEnv")
            .field("home", &self.home)
            .field("vars", &format_args!("<{} redacted>", self.vars.len()))
            .finish()
    }
}

impl HarnessEnv {
    /// This process's `$HOME` and environment.
    pub fn from_process() -> Self {
        Self {
            home: std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from),
            vars: std::env::vars().collect(),
        }
    }

    /// An explicit home directory and environment.
    pub fn new<I, K, V>(home: Option<PathBuf>, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            home,
            vars: vars
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// A variable's value; an empty value counts as unset.
    fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// `$VAR` as a directory (a leading `~/` expanded), else `$HOME/<rel>`.
    fn dir_or_home(&self, var: &str, rel: &str) -> Option<PathBuf> {
        match self.var(var) {
            Some(v) => Some(self.expand_tilde(v)),
            None => self.home.as_ref().map(|h| h.join(rel)),
        }
    }

    fn expand_tilde(&self, p: &str) -> PathBuf {
        match (p.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(p),
        }
    }
}

/// The kind of place a key was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    CredentialsFile,
    ConfigFile,
    Environment,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::CredentialsFile => "credentials file",
            SourceKind::ConfigFile => "configuration file",
            SourceKind::Environment => "environment",
        }
    }
}

/// Why a key that is set could not be read. Fixed words only: never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unreadable {
    /// The key comes from a command, which crew-watch never runs.
    Command,
    /// A value mixing text and environment references.
    Template,
    /// A file that does not parse.
    Parse,
    /// A file over the size cap.
    TooLarge,
    /// A file that exists but cannot be read.
    Io,
    /// A value that is not shaped like a key.
    Malformed,
}

impl Unreadable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unreadable::Command => "it comes from a command, which crew-watch does not run",
            Unreadable::Template => "it mixes text and environment variables",
            Unreadable::Parse => "a file does not parse",
            Unreadable::TooLarge => "a file is too large",
            Unreadable::Io => "a file cannot be read",
            Unreadable::Malformed => "it is not shaped like a key",
        }
    }
}

/// What a lookup found, without the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", content = "detail", rename_all = "kebab-case")]
pub enum LookupStatus {
    /// A key, from this kind of source.
    Found(SourceKind),
    /// The harness can use OpenRouter but has no key set.
    NoKey,
    /// The harness is set up for another provider or host.
    NotOpenRouter,
    /// The harness cannot use OpenRouter at all.
    Unsupported,
    /// A key is set but cannot be read.
    NotReadable(Unreadable),
    /// Not a runtime crew-watch detects.
    UnknownHarness,
}

/// A lookup's outcome for people: which harness, and what was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookupOutcome {
    pub harness: String,
    pub status: LookupStatus,
}

impl fmt::Display for LookupOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.harness;
        match self.status {
            LookupStatus::Found(kind) => {
                write!(f, "using the OpenRouter key {h} uses ({})", kind.as_str())
            }
            LookupStatus::NoKey => write!(f, "{h} has no OpenRouter key"),
            LookupStatus::NotOpenRouter => write!(f, "{h} is not set up for OpenRouter"),
            LookupStatus::Unsupported => write!(f, "{h} cannot use OpenRouter"),
            LookupStatus::NotReadable(why) => {
                write!(f, "{h}'s OpenRouter key is not readable: {}", why.as_str())
            }
            LookupStatus::UnknownHarness => write!(f, "{h} is not a known harness"),
        }
    }
}

/// Internal result of one harness reader: the key, or why there is none.
pub(crate) enum Found {
    Key(OpenRouterKey, SourceKind),
    Status(LookupStatus),
}

type Reader = fn(&Path, &HarnessEnv) -> Found;

/// One row per runtime of [`crate::detect::AGENT_KINDS`] (test-enforced).
const HARNESSES: &[(&str, Reader)] = &[
    ("claude", read_claude),
    ("opencode", read_opencode),
    ("codex", read_codex),
    ("grok", read_grok),
    ("kimi", read_kimi),
    ("muse", read_muse),
    ("pi", read_pi),
];

/// Find the OpenRouter key `runtime` would use when run in `project_dir`.
/// Crate-internal: the key goes only to the OpenRouter reader.
pub(crate) fn find_key(
    runtime: &str,
    project_dir: &Path,
    env: &HarnessEnv,
) -> (LookupOutcome, Option<OpenRouterKey>) {
    let found = match HARNESSES.iter().find(|(id, _)| *id == runtime) {
        Some((_, reader)) => reader(project_dir, env),
        None => Found::Status(LookupStatus::UnknownHarness),
    };
    let (status, key) = match found {
        Found::Key(k, kind) => (LookupStatus::Found(kind), Some(k)),
        Found::Status(s) => (s, None),
    };
    (
        LookupOutcome {
            harness: runtime.to_string(),
            status,
        },
        key,
    )
}

/// What a lookup for `runtime` in `project_dir` finds, without the key: for a
/// settings page or `--once` to say why there is (or is no) OpenRouter line.
pub fn lookup_outcome(runtime: &str, project_dir: &Path, env: &HarnessEnv) -> LookupOutcome {
    find_key(runtime, project_dir, env).0
}

/// True iff `url` is `https` on `openrouter.ai` or one of its subdomains,
/// with no userinfo. Case-insensitive on scheme and host.
pub fn is_openrouter_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => authority,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    host == "openrouter.ai" || host.ends_with(".openrouter.ai")
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A configured value after resolving references.
enum Resolved {
    Key(String),
    /// The value refers to something unset: this source gives no key.
    Unset,
    Unreadable(Unreadable),
}

/// Read a file capped at [`MAX_FILE_BYTES`]. A missing file is `Ok(None)`.
fn read_capped(path: &Path) -> Result<Option<String>, Unreadable> {
    use std::io::Read;
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Unreadable::Io),
    };
    let mut buf = String::new();
    match f.take(MAX_FILE_BYTES + 1).read_to_string(&mut buf) {
        Ok(n) if n as u64 > MAX_FILE_BYTES => Err(Unreadable::TooLarge),
        Ok(_) => Ok(Some(buf)),
        Err(_) => Err(Unreadable::Io),
    }
}

fn read_json(path: &Path) -> Result<Option<Json>, Unreadable> {
    match read_capped(path)? {
        None => Ok(None),
        Some(s) => parse_jsonc(&s).map(Some),
    }
}

fn read_toml(path: &Path) -> Result<Option<toml::Table>, Unreadable> {
    match read_capped(path)? {
        None => Ok(None),
        Some(s) => s
            .parse::<toml::Table>()
            .map(Some)
            .map_err(|_| Unreadable::Parse),
    }
}

/// Parse JSON that may carry `//` and `/* */` comments and trailing commas
/// (OpenCode's `.jsonc`, Claude Code's settings).
fn parse_jsonc(s: &str) -> Result<Json, Unreadable> {
    serde_json::from_str(&strip_jsonc(s)).map_err(|_| Unreadable::Parse)
}

/// Remove comments, then trailing commas, outside strings.
fn strip_jsonc(s: &str) -> String {
    strip_trailing_commas(&strip_comments(s))
}

/// Walk `s` outside JSON strings: `f` sees each character outside a string
/// with the rest of the input and returns how many characters it consumed
/// (0 to copy the character through).
fn walk_outside_strings(s: &str, mut f: impl FnMut(&[char], usize) -> usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != '"';
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        match f(&chars, i) {
            0 => {
                out.push(c);
                i += 1;
            }
            n => i += n,
        }
    }
    out
}

fn strip_comments(s: &str) -> String {
    walk_outside_strings(s, |chars, i| match (chars[i], chars.get(i + 1)) {
        ('/', Some('/')) => {
            let mut j = i;
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
            j - i
        }
        ('/', Some('*')) => {
            let mut j = i + 2;
            while j < chars.len() && !(chars[j] == '*' && chars.get(j + 1) == Some(&'/')) {
                j += 1;
            }
            (j + 2).min(chars.len()) - i
        }
        _ => 0,
    })
}

fn strip_trailing_commas(s: &str) -> String {
    walk_outside_strings(s, |chars, i| {
        if chars[i] != ',' {
            return 0;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        // Consume (drop) a comma that closes nothing but a container.
        usize::from(matches!(chars.get(j), Some('}') | Some(']')))
    })
}

fn is_env_name(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Look up a variable named by a harness's config (`env_key`,
/// `api_key_env`). An invalid name is "not readable"; an unset one is no key.
fn env_named(name: &str, env: &HarnessEnv) -> Resolved {
    if !is_env_name(name) {
        return Resolved::Unreadable(Unreadable::Malformed);
    }
    match env.var(name) {
        Some(v) => Resolved::Key(v.to_string()),
        None => Resolved::Unset,
    }
}

/// pi's value forms: a literal, `$NAME` / `${NAME}` as the whole value, or a
/// leading `!command`.
fn resolve_dollar(v: &str, env: &HarnessEnv) -> Resolved {
    let v = v.trim();
    if v.starts_with('!') {
        return Resolved::Unreadable(Unreadable::Command);
    }
    let name = v
        .strip_prefix("${")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| v.strip_prefix('$'));
    if let Some(name) = name {
        return if is_env_name(name) {
            env.var(name)
                .map(|k| Resolved::Key(k.to_string()))
                .unwrap_or(Resolved::Unset)
        } else {
            Resolved::Unreadable(Unreadable::Template)
        };
    }
    if v.contains('$') {
        return Resolved::Unreadable(Unreadable::Template);
    }
    Resolved::Key(v.to_string())
}

/// OpenCode's value forms: a literal, or `{env:NAME}` / `{file:path}` as the
/// whole value. `base` is the directory of the config file, for a relative
/// `{file:}` path.
fn resolve_braces(v: &str, env: &HarnessEnv, base: &Path) -> Resolved {
    let v = v.trim();
    if let Some(name) = v.strip_prefix("{env:").and_then(|r| r.strip_suffix('}')) {
        return if is_env_name(name) {
            env.var(name)
                .map(|k| Resolved::Key(k.to_string()))
                .unwrap_or(Resolved::Unset)
        } else {
            Resolved::Unreadable(Unreadable::Template)
        };
    }
    if let Some(p) = v.strip_prefix("{file:").and_then(|r| r.strip_suffix('}')) {
        if p.contains('{') {
            return Resolved::Unreadable(Unreadable::Template);
        }
        let path = if p.starts_with("~/") {
            env.expand_tilde(p)
        } else {
            base.join(p)
        };
        return match read_capped(&path) {
            Ok(Some(s)) => Resolved::Key(s.trim().to_string()),
            Ok(None) => Resolved::Unset,
            Err(e) => Resolved::Unreadable(e),
        };
    }
    if v.contains("{env:") || v.contains("{file:") {
        return Resolved::Unreadable(Unreadable::Template);
    }
    Resolved::Key(v.to_string())
}

/// Turn a resolved value into a reader result; `Unset` returns `None` so the
/// caller moves on to the harness's next source.
fn finish(r: Resolved, kind: SourceKind) -> Option<Found> {
    match r {
        Resolved::Key(k) if k.is_empty() => None,
        Resolved::Key(k) => Some(match OpenRouterKey::new(&k) {
            Ok(key) => Found::Key(key, kind),
            Err(_) => Found::Status(LookupStatus::NotReadable(Unreadable::Malformed)),
        }),
        Resolved::Unset => None,
        Resolved::Unreadable(u) => Some(Found::Status(LookupStatus::NotReadable(u))),
    }
}

fn unreadable(u: Unreadable) -> Found {
    Found::Status(LookupStatus::NotReadable(u))
}

/// `?`-style early return for a reader: a file error ends the lookup.
macro_rules! try_read {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(u) => return unreadable(u),
        }
    };
}

fn json_str<'a>(v: &'a Json, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str()
}

fn toml_get<'a>(t: &'a toml::Table, path: &[&str]) -> Option<&'a toml::Value> {
    let (first, rest) = path.split_first()?;
    let mut cur = t.get(*first)?;
    for p in rest {
        cur = cur.as_table()?.get(*p)?;
    }
    Some(cur)
}

fn toml_str<'a>(t: &'a toml::Table, path: &[&str]) -> Option<&'a str> {
    toml_get(t, path)?.as_str()
}

// ---------------------------------------------------------------------------
// Claude Code: a gateway via ANTHROPIC_BASE_URL + ANTHROPIC_AUTH_TOKEN
// ---------------------------------------------------------------------------

/// Claude Code has no OpenRouter provider; it reaches OpenRouter as a gateway
/// when `ANTHROPIC_BASE_URL` is OpenRouter's. Each variable comes from the
/// `env` block of the highest-precedence settings file that sets it (the
/// project's `.claude/settings.local.json`, then `.claude/settings.json`,
/// then the user's `settings.json` in `$CLAUDE_CONFIG_DIR` or `~/.claude`),
/// else the environment. Auth order: `ANTHROPIC_AUTH_TOKEN`, then
/// `ANTHROPIC_API_KEY`, then `apiKeyHelper` (a command: not read).
fn read_claude(project: &Path, env: &HarnessEnv) -> Found {
    let mut files = vec![
        project.join(".claude").join("settings.local.json"),
        project.join(".claude").join("settings.json"),
    ];
    if let Some(dir) = env.dir_or_home("CLAUDE_CONFIG_DIR", ".claude") {
        files.push(dir.join("settings.json"));
    }
    let mut docs = Vec::new();
    for f in &files {
        if let Some(doc) = try_read!(read_json(f)) {
            docs.push(doc);
        }
    }
    // (value, came from a settings file)
    let lookup = |name: &str| -> Option<(String, bool)> {
        for d in &docs {
            if let Some(v) = json_str(d, &["env", name]) {
                return Some((v.to_string(), true));
            }
        }
        env.var(name).map(|v| (v.to_string(), false))
    };
    match lookup("ANTHROPIC_BASE_URL") {
        Some((url, _)) if is_openrouter_url(&url) => {}
        _ => return Found::Status(LookupStatus::NotOpenRouter),
    }
    for name in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"] {
        if let Some((v, from_file)) = lookup(name) {
            let kind = if from_file {
                SourceKind::ConfigFile
            } else {
                SourceKind::Environment
            };
            if let Some(found) = finish(Resolved::Key(v), kind) {
                return found;
            }
        }
    }
    if docs.iter().any(|d| d.get("apiKeyHelper").is_some()) {
        return unreadable(Unreadable::Command);
    }
    Found::Status(LookupStatus::NoKey)
}

// ---------------------------------------------------------------------------
// OpenCode: built-in `openrouter` provider
// ---------------------------------------------------------------------------

/// OpenCode's order: `provider.openrouter.options.apiKey` in its merged
/// configuration, then the `openrouter` entry of `auth.json`, then
/// `OPENROUTER_API_KEY`. Config layers, highest first:
/// `OPENCODE_CONFIG_CONTENT`, the project's `opencode.jsonc`/`opencode.json`,
/// the file `OPENCODE_CONFIG` names, then the global
/// `opencode.jsonc`/`opencode.json`/`config.json` in
/// `${XDG_CONFIG_HOME:-~/.config}/opencode`. `OPENCODE_AUTH_CONTENT`, when
/// set, replaces `auth.json` (`${XDG_DATA_HOME:-~/.local/share}/opencode`).
fn read_opencode(project: &Path, env: &HarnessEnv) -> Found {
    // (document, directory a relative `{file:}` resolves against)
    let mut layers: Vec<(Json, PathBuf)> = Vec::new();
    if let Some(content) = env.var("OPENCODE_CONFIG_CONTENT") {
        layers.push((try_read!(parse_jsonc(content)), project.to_path_buf()));
    }
    let mut files = vec![
        project.join("opencode.jsonc"),
        project.join("opencode.json"),
    ];
    if let Some(custom) = env.var("OPENCODE_CONFIG") {
        files.push(env.expand_tilde(custom));
    }
    if let Some(dir) = env.dir_or_home("XDG_CONFIG_HOME", ".config") {
        let dir = dir.join("opencode");
        for name in ["opencode.jsonc", "opencode.json", "config.json"] {
            files.push(dir.join(name));
        }
    }
    for f in &files {
        if let Some(doc) = try_read!(read_json(f)) {
            let base = f.parent().map(Path::to_path_buf).unwrap_or_default();
            layers.push((doc, base));
        }
    }
    let first = |field: &str| {
        layers.iter().find_map(|(d, base)| {
            json_str(d, &["provider", "openrouter", "options", field]).map(|v| (v, base))
        })
    };
    if let Some((url, base)) = first("baseURL") {
        match resolve_braces(url, env, base) {
            Resolved::Key(u) if is_openrouter_url(&u) => {}
            Resolved::Unset => {}
            _ => return Found::Status(LookupStatus::NotOpenRouter),
        }
    }
    if let Some((v, base)) = first("apiKey") {
        if let Some(found) = finish(resolve_braces(v, env, base), SourceKind::ConfigFile) {
            return found;
        }
    }
    let auth = match env.var("OPENCODE_AUTH_CONTENT") {
        Some(content) => Some(try_read!(parse_jsonc(content))),
        None => match env.dir_or_home("XDG_DATA_HOME", ".local/share") {
            Some(dir) => try_read!(read_json(&dir.join("opencode").join("auth.json"))),
            None => None,
        },
    };
    if let Some(entry) = auth.as_ref().and_then(|a| a.get("openrouter")) {
        if entry.get("type").and_then(Json::as_str) == Some("api") {
            if let Some(k) = entry.get("key").and_then(Json::as_str) {
                if let Some(found) =
                    finish(Resolved::Key(k.to_string()), SourceKind::CredentialsFile)
                {
                    return found;
                }
            }
        }
    }
    if let Some(found) = finish(
        env_named("OPENROUTER_API_KEY", env),
        SourceKind::Environment,
    ) {
        return found;
    }
    Found::Status(LookupStatus::NoKey)
}

// ---------------------------------------------------------------------------
// pi: built-in `openrouter` provider
// ---------------------------------------------------------------------------

/// pi's order: the `openrouter` entry of `auth.json` (an `api_key` entry's
/// `key`, or an `oauth` entry's `access` from "Sign in with OpenRouter"),
/// then `providers.openrouter.apiKey` in `models.json`, then
/// `OPENROUTER_API_KEY`. Both files live in `${PI_CODING_AGENT_DIR:-~/.pi/agent}`.
/// `providers.openrouter.baseUrl` in `models.json` moves the provider to
/// another host.
fn read_pi(_project: &Path, env: &HarnessEnv) -> Found {
    let Some(dir) = env.dir_or_home("PI_CODING_AGENT_DIR", ".pi/agent") else {
        return Found::Status(LookupStatus::NoKey);
    };
    let models = try_read!(read_json(&dir.join("models.json")));
    let provider = models
        .as_ref()
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.get("openrouter"));
    if let Some(url) = provider
        .and_then(|p| p.get("baseUrl"))
        .and_then(Json::as_str)
    {
        match resolve_dollar(url, env) {
            Resolved::Key(u) if is_openrouter_url(&u) => {}
            Resolved::Unset => {}
            _ => return Found::Status(LookupStatus::NotOpenRouter),
        }
    }
    let auth = try_read!(read_json(&dir.join("auth.json")));
    if let Some(entry) = auth.as_ref().and_then(|a| a.get("openrouter")) {
        let value = match entry.get("type").and_then(Json::as_str) {
            Some("api_key") => entry.get("key").and_then(Json::as_str),
            Some("oauth") => entry.get("access").and_then(Json::as_str),
            _ => None,
        };
        if let Some(v) = value {
            let resolved = if entry.get("type").and_then(Json::as_str) == Some("oauth") {
                Resolved::Key(v.to_string())
            } else {
                resolve_dollar(v, env)
            };
            if let Some(found) = finish(resolved, SourceKind::CredentialsFile) {
                return found;
            }
        }
    }
    if let Some(v) = provider
        .and_then(|p| p.get("apiKey"))
        .and_then(Json::as_str)
    {
        if let Some(found) = finish(resolve_dollar(v, env), SourceKind::ConfigFile) {
            return found;
        }
    }
    if let Some(found) = finish(
        env_named("OPENROUTER_API_KEY", env),
        SourceKind::Environment,
    ) {
        return found;
    }
    Found::Status(LookupStatus::NoKey)
}

// ---------------------------------------------------------------------------
// Codex, Grok Build, Kimi Code: a custom OpenAI-compatible provider in TOML
// ---------------------------------------------------------------------------

/// Codex: in `${CODEX_HOME:-~/.codex}/config.toml`, the provider that
/// `model_provider` names (a `profile`'s own `model_provider` wins), when its
/// `base_url` is OpenRouter's: its `auth` command (not read), its
/// `experimental_bearer_token`, or the variable its `env_key` names.
fn read_codex(_project: &Path, env: &HarnessEnv) -> Found {
    let Some(dir) = env.dir_or_home("CODEX_HOME", ".codex") else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(cfg) = try_read!(read_toml(&dir.join("config.toml"))) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let profile_provider = toml_str(&cfg, &["profile"])
        .and_then(|p| toml_str(&cfg, &["profiles", p, "model_provider"]));
    let Some(id) = profile_provider.or_else(|| toml_str(&cfg, &["model_provider"])) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(prov) = toml_get(&cfg, &["model_providers", id]).and_then(|v| v.as_table()) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    if !toml_str(prov, &["base_url"]).is_some_and(is_openrouter_url) {
        return Found::Status(LookupStatus::NotOpenRouter);
    }
    if prov.get("auth").is_some() {
        return unreadable(Unreadable::Command);
    }
    if let Some(t) = toml_str(prov, &["experimental_bearer_token"]) {
        if let Some(found) = finish(Resolved::Key(t.to_string()), SourceKind::ConfigFile) {
            return found;
        }
    }
    if let Some(name) = toml_str(prov, &["env_key"]) {
        if let Some(found) = finish(env_named(name, env), SourceKind::Environment) {
            return found;
        }
    }
    Found::Status(LookupStatus::NoKey)
}

/// Grok Build: in `${GROK_HOME:-~/.grok}/config.toml`, the `[model."<name>"]`
/// table that `[models] default` names, when its `base_url` is OpenRouter's:
/// the variable its `env_key` names.
fn read_grok(_project: &Path, env: &HarnessEnv) -> Found {
    let Some(dir) = env.dir_or_home("GROK_HOME", ".grok") else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(cfg) = try_read!(read_toml(&dir.join("config.toml"))) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(name) = toml_str(&cfg, &["models", "default"]) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(model) = toml_get(&cfg, &["model", name]).and_then(|v| v.as_table()) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    if !toml_str(model, &["base_url"]).is_some_and(is_openrouter_url) {
        return Found::Status(LookupStatus::NotOpenRouter);
    }
    if let Some(var) = toml_str(model, &["env_key"]) {
        if let Some(found) = finish(env_named(var, env), SourceKind::Environment) {
            return found;
        }
    }
    Found::Status(LookupStatus::NoKey)
}

/// Kimi Code: in `${KIMI_CODE_HOME:-~/.kimi-code}/config.toml`, the provider
/// of the `default_model` (`[models.<m>] provider`), when its `base_url` is
/// OpenRouter's: its `api_key`, else the variable its `api_key_env` names,
/// else `OPENAI_API_KEY` in its `[providers.<name>.env]` table. Kimi reads
/// no other environment variable for credentials.
fn read_kimi(_project: &Path, env: &HarnessEnv) -> Found {
    let Some(dir) = env.dir_or_home("KIMI_CODE_HOME", ".kimi-code") else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(cfg) = try_read!(read_toml(&dir.join("config.toml"))) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(provider) =
        toml_str(&cfg, &["default_model"]).and_then(|m| toml_str(&cfg, &["models", m, "provider"]))
    else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    let Some(prov) = toml_get(&cfg, &["providers", provider]).and_then(|v| v.as_table()) else {
        return Found::Status(LookupStatus::NotOpenRouter);
    };
    if !toml_str(prov, &["base_url"]).is_some_and(is_openrouter_url) {
        return Found::Status(LookupStatus::NotOpenRouter);
    }
    if let Some(k) = toml_str(prov, &["api_key"]) {
        if let Some(found) = finish(Resolved::Key(k.to_string()), SourceKind::ConfigFile) {
            return found;
        }
    }
    if let Some(var) = toml_str(prov, &["api_key_env"]) {
        if let Some(found) = finish(env_named(var, env), SourceKind::Environment) {
            return found;
        }
    }
    if let Some(k) = toml_str(prov, &["env", "OPENAI_API_KEY"]) {
        if let Some(found) = finish(Resolved::Key(k.to_string()), SourceKind::ConfigFile) {
            return found;
        }
    }
    Found::Status(LookupStatus::NoKey)
}

/// Muse Code's only working provider is Meta's own.
fn read_muse(_project: &Path, _env: &HarnessEnv) -> Found {
    Found::Status(LookupStatus::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const KEY: &str = "fake-openrouter-test-key-one";
    const KEY2: &str = "fake-openrouter-test-key-two";

    /// A throwaway home + project under the system temp dir, removed on drop.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "crew-watch-harness-key-{}-{}-{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join("home")).unwrap();
            fs::create_dir_all(root.join("project")).unwrap();
            Self { root }
        }
        fn home(&self) -> PathBuf {
            self.root.join("home")
        }
        fn project(&self) -> PathBuf {
            self.root.join("project")
        }
        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
            p
        }
        fn env(&self, vars: &[(&str, &str)]) -> HarnessEnv {
            HarnessEnv::new(
                Some(self.home()),
                vars.iter().map(|(k, v)| (k.to_string(), v.to_string())),
            )
        }
        fn find(&self, runtime: &str, vars: &[(&str, &str)]) -> (LookupStatus, Option<String>) {
            let (o, k) = find_key(runtime, &self.project(), &self.env(vars));
            (o.status, k.map(|k| k.expose().to_string()))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn found(kind: SourceKind, key: &str) -> (LookupStatus, Option<String>) {
        (LookupStatus::Found(kind), Some(key.to_string()))
    }

    fn status(s: LookupStatus) -> (LookupStatus, Option<String>) {
        (s, None)
    }

    #[test]
    fn every_detected_runtime_has_a_row() {
        for kind in crate::detect::AGENT_KINDS {
            assert!(
                HARNESSES.iter().any(|(id, _)| *id == kind.id),
                "runtime {} has no OpenRouter key row",
                kind.id
            );
        }
        assert_eq!(HARNESSES.len(), crate::detect::AGENT_KINDS.len());
    }

    #[test]
    fn openrouter_url_rule() {
        for ok in [
            "https://openrouter.ai/api/v1",
            "https://openrouter.ai/api",
            "HTTPS://OpenRouter.AI/api",
            "https://us.openrouter.ai/api/v1",
            "https://openrouter.ai:443/api",
            "https://openrouter.ai",
        ] {
            assert!(is_openrouter_url(ok), "{ok}");
        }
        for bad in [
            "http://openrouter.ai/api/v1",
            "https://openrouter.ai.evil.example/api",
            "https://evilopenrouter.ai/api",
            "https://user@openrouter.ai/api",
            "https://proxy.example/openrouter.ai",
            "https://api.anthropic.com",
            "openrouter.ai/api",
            "",
        ] {
            assert!(!is_openrouter_url(bad), "{bad}");
        }
    }

    #[test]
    fn unknown_harness_and_muse() {
        let f = Fixture::new("misc");
        assert_eq!(f.find("nope", &[]), status(LookupStatus::UnknownHarness));
        assert_eq!(
            f.find("muse", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::Unsupported)
        );
    }

    // --- pi ---

    #[test]
    fn pi_sources_in_order() {
        let f = Fixture::new("pi-order");
        // Environment only.
        assert_eq!(
            f.find("pi", &[("OPENROUTER_API_KEY", KEY)]),
            found(SourceKind::Environment, KEY)
        );
        // models.json beats the environment.
        f.write(
            "home/.pi/agent/models.json",
            &format!(r#"{{"providers": {{"openrouter": {{"apiKey": "{KEY2}"}}}}}}"#),
        );
        assert_eq!(
            f.find("pi", &[("OPENROUTER_API_KEY", KEY)]),
            found(SourceKind::ConfigFile, KEY2)
        );
        // auth.json beats models.json.
        f.write(
            "home/.pi/agent/auth.json",
            &format!(
                r#"{{"anthropic": {{"type": "oauth", "access": "x"}}, "openrouter": {{"type": "api_key", "key": "{KEY}"}}}}"#
            ),
        );
        assert_eq!(f.find("pi", &[]), found(SourceKind::CredentialsFile, KEY));
    }

    #[test]
    fn pi_oauth_and_env_reference_and_agent_dir() {
        let f = Fixture::new("pi-oauth");
        f.write(
            "custom/auth.json",
            &format!(
                r#"{{"openrouter": {{"type": "oauth", "access": "{KEY}", "refresh": "", "expires": 9007199254740991}}}}"#
            ),
        );
        let dir = f.root.join("custom");
        let dir = dir.to_str().unwrap();
        assert_eq!(
            f.find("pi", &[("PI_CODING_AGENT_DIR", dir)]),
            found(SourceKind::CredentialsFile, KEY)
        );
        f.write(
            "custom/auth.json",
            r#"{"openrouter": {"type": "api_key", "key": "${MY_OR_KEY}"}}"#,
        );
        assert_eq!(
            f.find("pi", &[("PI_CODING_AGENT_DIR", dir), ("MY_OR_KEY", KEY2)]),
            found(SourceKind::CredentialsFile, KEY2)
        );
        // An unset reference gives nothing; the next source answers.
        assert_eq!(
            f.find(
                "pi",
                &[("PI_CODING_AGENT_DIR", dir), ("OPENROUTER_API_KEY", KEY)]
            ),
            found(SourceKind::Environment, KEY)
        );
        // A mixed template is not readable.
        f.write(
            "custom/auth.json",
            r#"{"openrouter": {"type": "api_key", "key": "sk-$MY_OR_KEY"}}"#,
        );
        assert_eq!(
            f.find("pi", &[("PI_CODING_AGENT_DIR", dir), ("MY_OR_KEY", KEY2)]),
            status(LookupStatus::NotReadable(Unreadable::Template))
        );
    }

    #[test]
    fn pi_command_is_never_run() {
        let f = Fixture::new("pi-cmd");
        let marker = f.root.join("ran");
        f.write(
            "home/.pi/agent/auth.json",
            &format!(
                r#"{{"openrouter": {{"type": "api_key", "key": "!touch {}"}}}}"#,
                marker.display()
            ),
        );
        assert_eq!(
            f.find("pi", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NotReadable(Unreadable::Command))
        );
        assert!(!marker.exists(), "a command from a harness file ran");
    }

    #[test]
    fn pi_other_base_url_gives_no_key() {
        let f = Fixture::new("pi-proxy");
        f.write(
            "home/.pi/agent/models.json",
            r#"{"providers": {"openrouter": {"baseUrl": "https://proxy.example/v1"}}}"#,
        );
        assert_eq!(
            f.find("pi", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NotOpenRouter)
        );
        f.write(
            "home/.pi/agent/models.json",
            r#"{"providers": {"openrouter": {"baseUrl": "https://us.openrouter.ai/api/v1"}}}"#,
        );
        assert_eq!(
            f.find("pi", &[("OPENROUTER_API_KEY", KEY)]),
            found(SourceKind::Environment, KEY)
        );
    }

    #[test]
    fn pi_unparseable_file_and_missing_files() {
        let f = Fixture::new("pi-bad");
        assert_eq!(f.find("pi", &[]), status(LookupStatus::NoKey));
        f.write("home/.pi/agent/auth.json", "{ not json");
        assert_eq!(
            f.find("pi", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NotReadable(Unreadable::Parse))
        );
    }

    #[test]
    fn oversized_file_is_not_read() {
        let f = Fixture::new("big");
        let big = format!(
            r#"{{"openrouter": {{"type": "api_key", "key": "{KEY}"}}, "pad": "{}"}}"#,
            "x".repeat(MAX_FILE_BYTES as usize)
        );
        f.write("home/.pi/agent/auth.json", &big);
        assert_eq!(
            f.find("pi", &[]),
            status(LookupStatus::NotReadable(Unreadable::TooLarge))
        );
    }

    #[test]
    fn malformed_key_is_not_readable_and_not_echoed() {
        let f = Fixture::new("malformed");
        f.write(
            "home/.pi/agent/auth.json",
            r#"{"openrouter": {"type": "api_key", "key": "has spaces and \"quotes\""}}"#,
        );
        let (o, k) = find_key("pi", &f.project(), &f.env(&[]));
        assert!(k.is_none());
        assert_eq!(o.status, LookupStatus::NotReadable(Unreadable::Malformed));
        assert!(!o.to_string().contains("spaces"));
    }

    // --- OpenCode ---

    #[test]
    fn opencode_sources_in_order() {
        let f = Fixture::new("oc-order");
        assert_eq!(f.find("opencode", &[]), status(LookupStatus::NoKey));
        assert_eq!(
            f.find("opencode", &[("OPENROUTER_API_KEY", KEY)]),
            found(SourceKind::Environment, KEY)
        );
        f.write(
            "home/.local/share/opencode/auth.json",
            &format!(r#"{{"openrouter": {{"type": "api", "key": "{KEY2}"}}}}"#),
        );
        assert_eq!(
            f.find("opencode", &[("OPENROUTER_API_KEY", KEY)]),
            found(SourceKind::CredentialsFile, KEY2)
        );
        // OPENCODE_AUTH_CONTENT replaces the file.
        assert_eq!(
            f.find(
                "opencode",
                &[(
                    "OPENCODE_AUTH_CONTENT",
                    r#"{"other": {"type": "api", "key": "x"}}"#
                )]
            ),
            status(LookupStatus::NoKey)
        );
        // Global config, with comments and a trailing comma, beats auth.json.
        f.write(
            "home/.config/opencode/opencode.jsonc",
            r#"{
  // the key
  "provider": { "openrouter": { "options": { "apiKey": "{env:OR_KEY}", }, }, },
  /* done */
}"#,
        );
        assert_eq!(
            f.find("opencode", &[("OR_KEY", KEY)]),
            found(SourceKind::ConfigFile, KEY)
        );
        // The project's file beats the global one.
        f.write(
            "project/opencode.json",
            &format!(r#"{{"provider": {{"openrouter": {{"options": {{"apiKey": "{KEY2}"}}}}}}}}"#),
        );
        assert_eq!(
            f.find("opencode", &[("OR_KEY", KEY)]),
            found(SourceKind::ConfigFile, KEY2)
        );
    }

    #[test]
    fn opencode_file_reference_and_template() {
        let f = Fixture::new("oc-file");
        f.write("home/.config/opencode/or.key", &format!("{KEY}\n"));
        f.write(
            "home/.config/opencode/opencode.json",
            r#"{"provider": {"openrouter": {"options": {"apiKey": "{file:or.key}"}}}}"#,
        );
        assert_eq!(f.find("opencode", &[]), found(SourceKind::ConfigFile, KEY));
        f.write(
            "home/.config/opencode/opencode.json",
            r#"{"provider": {"openrouter": {"options": {"apiKey": "sk-{env:OR_KEY}"}}}}"#,
        );
        assert_eq!(
            f.find("opencode", &[("OR_KEY", KEY)]),
            status(LookupStatus::NotReadable(Unreadable::Template))
        );
    }

    #[test]
    fn opencode_other_base_url_gives_no_key() {
        let f = Fixture::new("oc-proxy");
        f.write(
            "project/opencode.json",
            r#"{"provider": {"openrouter": {"options": {"baseURL": "https://gateway.example/v1"}}}}"#,
        );
        assert_eq!(
            f.find("opencode", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NotOpenRouter)
        );
    }

    // --- Claude Code ---

    #[test]
    fn claude_needs_openrouter_base_url() {
        let f = Fixture::new("claude-base");
        // A token without the base URL is Anthropic's, never sent.
        assert_eq!(
            f.find("claude", &[("ANTHROPIC_AUTH_TOKEN", KEY)]),
            status(LookupStatus::NotOpenRouter)
        );
        assert_eq!(
            f.find(
                "claude",
                &[
                    ("ANTHROPIC_BASE_URL", "https://gateway.example"),
                    ("ANTHROPIC_AUTH_TOKEN", KEY)
                ]
            ),
            status(LookupStatus::NotOpenRouter)
        );
        assert_eq!(
            f.find(
                "claude",
                &[
                    ("ANTHROPIC_BASE_URL", "https://openrouter.ai/api"),
                    ("ANTHROPIC_AUTH_TOKEN", KEY)
                ]
            ),
            found(SourceKind::Environment, KEY)
        );
    }

    #[test]
    fn claude_settings_files_in_order() {
        let f = Fixture::new("claude-files");
        f.write(
            "home/.claude/settings.json",
            &format!(
                r#"{{"env": {{"ANTHROPIC_BASE_URL": "https://openrouter.ai/api", "ANTHROPIC_AUTH_TOKEN": "{KEY}"}}}}"#
            ),
        );
        assert_eq!(f.find("claude", &[]), found(SourceKind::ConfigFile, KEY));
        // The settings file beats the shell environment.
        assert_eq!(
            f.find("claude", &[("ANTHROPIC_AUTH_TOKEN", KEY2)]),
            found(SourceKind::ConfigFile, KEY)
        );
        // The project's local settings beat the user's.
        f.write(
            "project/.claude/settings.local.json",
            &format!(r#"{{"env": {{"ANTHROPIC_AUTH_TOKEN": "{KEY2}"}}}}"#),
        );
        assert_eq!(f.find("claude", &[]), found(SourceKind::ConfigFile, KEY2));
        // A project that moves the base URL elsewhere sends nothing to OpenRouter.
        f.write(
            "project/.claude/settings.json",
            r#"{"env": {"ANTHROPIC_BASE_URL": "https://api.anthropic.com"}}"#,
        );
        assert_eq!(f.find("claude", &[]), status(LookupStatus::NotOpenRouter));
    }

    #[test]
    fn claude_api_key_helper_is_never_run() {
        let f = Fixture::new("claude-helper");
        let marker = f.root.join("ran");
        f.write(
            "home/.claude/settings.json",
            &format!(
                r#"{{"apiKeyHelper": "touch {}", "env": {{"ANTHROPIC_BASE_URL": "https://openrouter.ai/api"}}}}"#,
                marker.display()
            ),
        );
        assert_eq!(
            f.find("claude", &[]),
            status(LookupStatus::NotReadable(Unreadable::Command))
        );
        assert!(!marker.exists());
        // A token beats the helper.
        assert_eq!(
            f.find("claude", &[("ANTHROPIC_AUTH_TOKEN", KEY)]),
            found(SourceKind::Environment, KEY)
        );
    }

    // --- Codex ---

    #[test]
    fn codex_custom_provider() {
        let f = Fixture::new("codex");
        assert_eq!(
            f.find("codex", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NotOpenRouter)
        );
        f.write(
            "home/.codex/config.toml",
            r#"
model = "openai/gpt-5"
model_provider = "openrouter"

[model_providers.openrouter]
name = "OpenRouter"
base_url = "https://openrouter.ai/api/v1"
env_key = "OPENROUTER_API_KEY"
"#,
        );
        assert_eq!(
            f.find("codex", &[("OPENROUTER_API_KEY", KEY)]),
            found(SourceKind::Environment, KEY)
        );
        assert_eq!(f.find("codex", &[]), status(LookupStatus::NoKey));
        // A profile selecting another provider wins over the top level.
        f.write(
            "home/.codex/config.toml",
            r#"
model_provider = "openrouter"
profile = "work"

[profiles.work]
model_provider = "proxy"

[model_providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
env_key = "OPENROUTER_API_KEY"

[model_providers.proxy]
base_url = "https://proxy.example/v1"
env_key = "OPENROUTER_API_KEY"
"#,
        );
        assert_eq!(
            f.find("codex", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NotOpenRouter)
        );
    }

    #[test]
    fn codex_auth_command_and_bearer_token() {
        let f = Fixture::new("codex-auth");
        f.write(
            "home/.codex/config.toml",
            r#"
model_provider = "openrouter"

[model_providers.openrouter]
base_url = "https://openrouter.ai/api/v1"

[model_providers.openrouter.auth]
command = "touch"
args = ["/nonexistent/should-not-exist"]
"#,
        );
        assert_eq!(
            f.find("codex", &[]),
            status(LookupStatus::NotReadable(Unreadable::Command))
        );
        f.write(
            "home/custom-codex/config.toml",
            &format!(
                r#"
model_provider = "or"
[model_providers.or]
base_url = "https://openrouter.ai/api/v1"
experimental_bearer_token = "{KEY2}"
"#
            ),
        );
        let dir = f.home().join("custom-codex");
        assert_eq!(
            f.find("codex", &[("CODEX_HOME", dir.to_str().unwrap())]),
            found(SourceKind::ConfigFile, KEY2)
        );
        f.write("home/.codex/config.toml", "model_provider = [broken");
        assert_eq!(
            f.find("codex", &[]),
            status(LookupStatus::NotReadable(Unreadable::Parse))
        );
    }

    // --- Grok Build ---

    #[test]
    fn grok_default_model_endpoint() {
        let f = Fixture::new("grok");
        f.write(
            "home/.grok/config.toml",
            r#"
[models]
default = "or-sonnet"

[model."grok-4.7"]
base_url = "https://api.x.ai/v1"
env_key = "XAI_API_KEY"

[model."or-sonnet"]
base_url = "https://openrouter.ai/api/v1"
env_key = "OPENROUTER_API_KEY"
api_backend = "chat_completions"
"#,
        );
        assert_eq!(
            f.find(
                "grok",
                &[("OPENROUTER_API_KEY", KEY), ("XAI_API_KEY", KEY2)]
            ),
            found(SourceKind::Environment, KEY)
        );
        f.write(
            "home/.grok/config.toml",
            r#"
[models]
default = "grok-4.7"

[model."grok-4.7"]
base_url = "https://api.x.ai/v1"
env_key = "XAI_API_KEY"
"#,
        );
        assert_eq!(
            f.find("grok", &[("XAI_API_KEY", KEY2)]),
            status(LookupStatus::NotOpenRouter)
        );
    }

    // --- Kimi Code ---

    #[test]
    fn kimi_provider_sources() {
        let f = Fixture::new("kimi");
        let cfg = |key_lines: &str| {
            format!(
                r#"
default_model = "or-model"

[models.or-model]
provider = "openrouter"
model = "moonshotai/kimi-k2"

[providers.openrouter]
type = "openai"
base_url = "https://openrouter.ai/api/v1"
{key_lines}
"#
            )
        };
        f.write(
            "home/.kimi-code/config.toml",
            &cfg(&format!(r#"api_key = "{KEY}""#)),
        );
        assert_eq!(f.find("kimi", &[]), found(SourceKind::ConfigFile, KEY));
        f.write(
            "home/.kimi-code/config.toml",
            &cfg(r#"api_key_env = "MY_OR""#),
        );
        assert_eq!(
            f.find("kimi", &[("MY_OR", KEY2)]),
            found(SourceKind::Environment, KEY2)
        );
        f.write(
            "home/.kimi-code/config.toml",
            &cfg(&format!(
                "[providers.openrouter.env]\nOPENAI_API_KEY = \"{KEY}\""
            )),
        );
        assert_eq!(f.find("kimi", &[]), found(SourceKind::ConfigFile, KEY));
        // Kimi does not fall back to the shell's OPENROUTER_API_KEY.
        f.write("home/.kimi-code/config.toml", &cfg(""));
        assert_eq!(
            f.find("kimi", &[("OPENROUTER_API_KEY", KEY)]),
            status(LookupStatus::NoKey)
        );
    }

    // --- outcome text ---

    #[test]
    fn outcome_never_carries_the_key() {
        let f = Fixture::new("outcome");
        let env = f.env(&[("OPENROUTER_API_KEY", KEY)]);
        let o = lookup_outcome("pi", &f.project(), &env);
        assert_eq!(o.status, LookupStatus::Found(SourceKind::Environment));
        let text = format!("{o} {o:?} {}", serde_json::to_string(&o).unwrap());
        assert!(!text.contains(KEY), "{text}");
        assert!(!format!("{env:?}").contains(KEY));
        assert_eq!(
            lookup_outcome("pi", &f.project(), &f.env(&[])).to_string(),
            "pi has no OpenRouter key"
        );
    }

    #[test]
    fn strip_jsonc_keeps_strings_intact() {
        let s = r#"{"a": "http://x//y", "b": "/* no */", "c": [1, 2,], } // end"#;
        let v: Json = serde_json::from_str(&strip_jsonc(s)).unwrap();
        assert_eq!(v["a"], "http://x//y");
        assert_eq!(v["b"], "/* no */");
        assert_eq!(v["c"], serde_json::json!([1, 2]));
    }
}
