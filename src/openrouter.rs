//! OpenRouter usage for the quota row, read by crew-watch itself over HTTPS.
//!
//! Two sources, chosen by the caller ([`OpenRouterSource`]):
//!
//! - **A management key** gives the full line: the picked key's limit left
//!   (or the account's credits left), and spend per model over the last hour,
//!   day or week — the four most expensive models and the rest combined.
//!   Requests, in order, on one agent: `GET /key` (is it a management key,
//!   when does it expire), `GET /keys` (names, limits, hashes), `GET /credits`
//!   (the balance), `POST /analytics/query` (spend per model, in minute or
//!   hour buckets summed here).
//! - **The harness's own key**, found by [`crate::harness_key`], gives what a
//!   regular key reads about itself in one `GET /key`: its limit or effective
//!   budget, and its spend in the current UTC day, week and month.
//!
//! With no source, or a harness without a key, nothing is asked of OpenRouter.
//!
//! ## Transport
//!
//! ureq with rustls: `https` only, to the one fixed host, no redirects, no
//! proxy from the environment, a fixed user agent, a size cap on every body,
//! and the caller's deadline over the whole fetch. Status codes are data:
//! `401` is "sign-in required", `403` on the key list is "needs a management
//! key", anything else "unavailable". Each part fails alone (a refused
//! analytics query still leaves the limit), and no response body or transport
//! error text is ever relayed. Nothing here runs on the `/proc` tick path:
//! [`spawn_poller`] fetches on its own thread at the quota cadence.
//!
//! ## Secrets
//!
//! Keys live in [`OpenRouterKey`], whose `Debug` prints `<redacted>` and which
//! is not serializable. The masked key fragment (`label`) and account ids in
//! OpenRouter's answers are never read; a listed key's hash is used for the
//! analytics filter and never leaves this module.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};

use crate::format_util::format_reset;
use crate::harness_key::{find_key, HarnessEnv, LookupOutcome};
use crate::quota::{
    days_from_civil, fetch_once, parse_iso8601_utc_epoch, ModelSpend, ModelUsage, ProviderQuota,
    ProviderStatus, QuotaFetch, QuotaFigure, QuotaReport, QuotaWindow, RestSpend,
};

/// The provider id crew-watch's OpenRouter line uses. It replaces a provider
/// of the same id from quota-axi.
pub const PROVIDER_ID: &str = "openrouter";

/// The environment variable the TUI reads a management key from. A name of
/// crew-watch's own, so quota-axi and coding tools never pick the key up.
pub const KEY_ENV: &str = "CREW_WATCH_OPENROUTER_KEY";

const API_BASE: &str = "https://openrouter.ai/api/v1";
const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
/// `GET /keys` returns 100 keys a page; more than this many pages is not an
/// account the line is for.
const MAX_KEY_PAGES: usize = 10;
const KEYS_PAGE: usize = 100;
/// Analytics rows asked for (OpenRouter's maximum).
const ANALYTICS_ROW_LIMIT: u64 = 10_000;
/// A management key closer than this to expiry adds a note to the line.
const EXPIRY_NOTE_SECS: u64 = 7 * 86_400;
/// How many models the line names before "rest".
const TOP_MODELS: usize = 4;

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/// An OpenRouter key. `Debug` prints `<redacted>`; it is not serializable and
/// has no `Display`, so it cannot reach a log or a payload by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenRouterKey(String);

/// Why a string is not accepted as a key. Fixed words: never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    Empty,
    Malformed,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyError::Empty => "the key is empty",
            KeyError::Malformed => "the key may hold only letters, digits, '.', '_' and '-'",
        })
    }
}

impl std::error::Error for KeyError {}

impl OpenRouterKey {
    /// Accept a key of letters, digits, `.`, `_` and `-` (surrounding
    /// whitespace trimmed), at most 512 characters.
    pub fn new(value: &str) -> Result<Self, KeyError> {
        let v = value.trim();
        if v.is_empty() {
            return Err(KeyError::Empty);
        }
        if v.len() > 512
            || !v
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(KeyError::Malformed);
        }
        Ok(Self(v.to_string()))
    }

    /// The key in `$CREW_WATCH_OPENROUTER_KEY`: `None` when unset or empty.
    pub fn from_env() -> Option<Result<Self, KeyError>> {
        let v = std::env::var(KEY_ENV).ok()?;
        if v.trim().is_empty() {
            return None;
        }
        Some(Self::new(&v))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpenRouterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenRouterKey(<redacted>)")
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The rolling window spend per model is summed over, ending at the fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageWindow {
    /// The last 60 minutes, in minute buckets.
    Hour,
    /// The last 24 hours, in hour buckets.
    #[default]
    Day,
    /// The last 7 days, in hour buckets.
    Week,
}

impl UsageWindow {
    pub const ALL: [UsageWindow; 3] = [UsageWindow::Hour, UsageWindow::Day, UsageWindow::Week];

    pub fn as_str(self) -> &'static str {
        match self {
            UsageWindow::Hour => "1h",
            UsageWindow::Day => "1d",
            UsageWindow::Week => "1w",
        }
    }

    fn secs(self) -> u64 {
        match self {
            UsageWindow::Hour => 3600,
            UsageWindow::Day => 86_400,
            UsageWindow::Week => 7 * 86_400,
        }
    }

    fn granularity(self) -> &'static str {
        match self {
            UsageWindow::Hour => "minute",
            UsageWindow::Day | UsageWindow::Week => "hour",
        }
    }
}

impl FromStr for UsageWindow {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "1h" => Ok(UsageWindow::Hour),
            "1d" => Ok(UsageWindow::Day),
            "1w" => Ok(UsageWindow::Week),
            _ => Err("expected one of 1h, 1d, 1w".to_string()),
        }
    }
}

impl fmt::Display for UsageWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the OpenRouter line's key comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenRouterSource {
    /// A management key: the full line.
    Management {
        key: OpenRouterKey,
        window: UsageWindow,
        /// The OpenRouter key name the line is about. `None`: the account's
        /// only enabled key when there is exactly one, otherwise all keys.
        key_name: Option<String>,
    },
    /// The OpenRouter key of the harness `runtime` (an id of
    /// [`crate::detect::AGENT_KINDS`]) running in `project_dir`.
    Harness {
        runtime: String,
        project_dir: PathBuf,
    },
}

/// What one OpenRouter fetch produced.
#[derive(Debug, Clone, Default)]
pub struct OpenRouterResult {
    /// The `openrouter` provider line, or `None` when nothing was asked.
    pub provider: Option<ProviderQuota>,
    /// The names of the account's enabled keys (management key only), for a
    /// settings page's key picker.
    pub key_names: Vec<String>,
    /// The harness key lookup's outcome (harness source only).
    pub lookup: Option<LookupOutcome>,
}

/// quota-axi's report with crew-watch's own OpenRouter line merged in.
#[derive(Debug, Clone)]
pub struct CombinedFetch {
    /// The report. When quota-axi failed but OpenRouter answered, a report
    /// holding the OpenRouter line alone.
    pub quota: QuotaFetch,
    pub key_names: Vec<String>,
    pub lookup: Option<LookupOutcome>,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Run quota-axi and the OpenRouter reader side by side, each bounded by
/// `timeout`, and merge crew-watch's `openrouter` line into the report
/// (replacing quota-axi's provider of that id). With no source the result is
/// exactly [`fetch_once`]'s.
pub fn fetch_quota(
    source: Option<&OpenRouterSource>,
    env: &HarnessEnv,
    timeout: Duration,
) -> CombinedFetch {
    let axi = thread::spawn(move || fetch_once(timeout));
    let or = match source {
        Some(s) => fetch(s, env, now_epoch(), timeout),
        None => OpenRouterResult::default(),
    };
    let axi = axi
        .join()
        .unwrap_or_else(|_| QuotaFetch::Failed("quota-axi fetch panicked".to_string()));
    CombinedFetch {
        quota: merge(axi, or.provider),
        key_names: or.key_names,
        lookup: or.lookup,
    }
}

/// Merge crew-watch's OpenRouter line into a quota-axi fetch.
fn merge(axi: QuotaFetch, provider: Option<ProviderQuota>) -> QuotaFetch {
    let Some(p) = provider else {
        return axi;
    };
    match axi {
        QuotaFetch::Report(mut r) => {
            match r.providers.iter().position(|q| q.id == PROVIDER_ID) {
                Some(i) => r.providers[i] = p,
                None => r.providers.push(p),
            }
            QuotaFetch::Report(r)
        }
        QuotaFetch::Failed(_) => QuotaFetch::Report(QuotaReport {
            schema_version: None,
            generated_at: String::new(),
            providers: vec![p],
        }),
    }
}

/// Fetch the OpenRouter line alone for `source`, bounded by `timeout`.
/// `now` (UTC seconds) is the end of the usage window and the base of the
/// reset countdowns.
pub fn fetch(
    source: &OpenRouterSource,
    env: &HarnessEnv,
    now: u64,
    timeout: Duration,
) -> OpenRouterResult {
    fetch_with(&Client::production(timeout), source, env, now)
}

fn fetch_with(
    client: &Client,
    source: &OpenRouterSource,
    env: &HarnessEnv,
    now: u64,
) -> OpenRouterResult {
    match source {
        OpenRouterSource::Management {
            key,
            window,
            key_name,
        } => {
            let (provider, key_names) =
                read_management(client, key, *window, key_name.as_deref(), now);
            OpenRouterResult {
                provider: Some(provider),
                key_names,
                lookup: None,
            }
        }
        OpenRouterSource::Harness {
            runtime,
            project_dir,
        } => {
            let (outcome, key) = find_key(runtime, project_dir, env);
            let provider = key.map(|k| read_harness_key(client, &k, runtime, now));
            OpenRouterResult {
                provider,
                key_names: Vec::new(),
                lookup: Some(outcome),
            }
        }
    }
}

/// Spawn a background poller: every `interval`, [`fetch_quota`] with
/// whatever `source` holds at that moment (so the caller can change the
/// harness or key while it runs), sending the report to `tx`. Exits when the
/// receiver is dropped. Each fetch is bounded by 10s.
pub fn spawn_poller(
    interval: Duration,
    tx: Sender<QuotaFetch>,
    source: Arc<Mutex<Option<OpenRouterSource>>>,
    env: HarnessEnv,
) -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        let current = source.lock().ok().and_then(|s| s.clone());
        let fetch = fetch_quota(current.as_ref(), &env, Duration::from_secs(10));
        if tx.send(fetch.quota).is_err() {
            return;
        }
        thread::sleep(interval);
    })
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// One reply, reduced to what the reader acts on. Never carries a body that
/// failed to parse or any transport error text.
enum Reply {
    Ok(Json),
    /// A non-200 status.
    Status(u16),
    /// Transport failure, timeout or an unparseable body.
    Failed,
}

/// The failure of one part of the fetch, as the line reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    SignIn,
    NeedsManagement,
    Unavailable,
}

impl Failure {
    fn from_reply(r: &Reply) -> Self {
        match r {
            Reply::Status(401) => Failure::SignIn,
            Reply::Status(403) => Failure::NeedsManagement,
            _ => Failure::Unavailable,
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            Failure::SignIn => "sign-in required",
            Failure::NeedsManagement => "needs a management key",
            Failure::Unavailable => "unavailable",
        }
    }
}

struct Client {
    agent: ureq::Agent,
    base: String,
    deadline: Instant,
}

impl Client {
    fn production(timeout: Duration) -> Self {
        Self::build(API_BASE.to_string(), true, timeout)
    }

    fn build(base: String, https_only: bool, timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(https_only)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .proxy(None)
            .user_agent(concat!("crew-watch/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: config.into(),
            base,
            deadline: Instant::now() + timeout,
        }
    }

    fn remaining(&self) -> Option<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    fn get(&self, key: &OpenRouterKey, path: &str) -> Reply {
        let Some(left) = self.remaining() else {
            return Reply::Failed;
        };
        let resp = self
            .agent
            .get(format!("{}{}", self.base, path))
            .config()
            .timeout_global(Some(left))
            .build()
            .header("Authorization", format!("Bearer {}", key.expose()))
            .header("Accept", "application/json")
            .call();
        Self::reply(resp)
    }

    fn post(&self, key: &OpenRouterKey, path: &str, body: &Json) -> Reply {
        let Some(left) = self.remaining() else {
            return Reply::Failed;
        };
        let resp = self
            .agent
            .post(format!("{}{}", self.base, path))
            .config()
            .timeout_global(Some(left))
            .build()
            .header("Authorization", format!("Bearer {}", key.expose()))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(body.to_string());
        Self::reply(resp)
    }

    fn reply(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Reply {
        let Ok(resp) = resp else {
            return Reply::Failed;
        };
        let status = resp.status().as_u16();
        if status != 200 {
            return Reply::Status(status);
        }
        let Ok(text) = resp
            .into_body()
            .into_with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_string()
        else {
            return Reply::Failed;
        };
        match serde_json::from_str(&text) {
            Ok(v) => Reply::Ok(v),
            Err(_) => Reply::Failed,
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers (tolerant: numbers may be numeric strings, null is none)
// ---------------------------------------------------------------------------

fn num(v: Option<&Json>) -> Option<f64> {
    match v? {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

fn str_of(v: Option<&Json>) -> Option<&str> {
    v?.as_str()
}

fn data(v: &Json) -> Option<&Json> {
    v.get("data")
}

/// Sum of the present values, `None` when none is present.
fn sum_opt(vals: &[Option<f64>]) -> Option<f64> {
    vals.iter().copied().fold(None, |acc, v| match (acc, v) {
        (a, None) => a,
        (None, Some(x)) => Some(x),
        (Some(a), Some(x)) => Some(a + x),
    })
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Howard Hinnant's `civil_from_days`: (year, month, day) of a day count
/// since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for UTC seconds.
pub(crate) fn format_iso(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let secs = epoch % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The next reset of a key's limit after `now`, by OpenRouter's rule:
/// midnight UTC, weeks Monday through Sunday, months on the 1st. `None` for a
/// limit that never resets (or an unknown rule).
fn next_reset(rule: Option<&str>, now: u64) -> Option<u64> {
    let day = now / 86_400;
    match rule? {
        "daily" => Some((day + 1) * 86_400),
        "weekly" => {
            // 1970-01-01 was a Thursday: Monday-based weekday is (day + 3) % 7.
            let weekday = (day + 3) % 7;
            Some((day + 7 - weekday) * 86_400)
        }
        "monthly" => {
            let (y, m, _) = civil_from_days(day as i64);
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            Some(days_from_civil(ny, nm, 1)? as u64 * 86_400)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The line's pieces
// ---------------------------------------------------------------------------

fn new_provider(source: String) -> ProviderQuota {
    ProviderQuota {
        id: PROVIDER_ID.to_string(),
        label: "OpenRouter".to_string(),
        plan: None,
        windows: Vec::new(),
        status: ProviderStatus::Fresh,
        stale: false,
        error: None,
        figures: Vec::new(),
        usage: None,
        source: Some(source),
        note: None,
    }
}

/// Record a part's failure: the first failure names the line's status.
fn fail(p: &mut ProviderQuota, f: Failure) {
    if p.error.is_none() {
        p.status = match f {
            Failure::SignIn => ProviderStatus::AuthRequired,
            _ => ProviderStatus::Error,
        };
        p.error = Some(f.phrase().to_string());
    }
}

/// A limit as a percentage window (so the existing bar draws it) plus its
/// `limit left` figure.
fn push_limit(p: &mut ProviderQuota, limit: f64, remaining: f64, resets_at: Option<String>) {
    if limit <= 0.0 {
        return;
    }
    let used = ((limit - remaining) / limit * 100.0).clamp(0.0, 100.0);
    p.windows.push(QuotaWindow {
        id: "limit".to_string(),
        label: "limit".to_string(),
        percent_used: used,
        resets_at,
    });
    p.figures.push(QuotaFigure {
        id: "limit_left".to_string(),
        label: "left".to_string(),
        usd: remaining,
        cap_usd: Some(limit),
    });
}

fn push_figure(p: &mut ProviderQuota, id: &str, label: &str, usd: Option<f64>) {
    if let Some(usd) = usd {
        p.figures.push(QuotaFigure {
            id: id.to_string(),
            label: label.to_string(),
            usd,
            cap_usd: None,
        });
    }
}

// ---------------------------------------------------------------------------
// The harness's key: one GET /key
// ---------------------------------------------------------------------------

fn read_harness_key(
    client: &Client,
    key: &OpenRouterKey,
    runtime: &str,
    now: u64,
) -> ProviderQuota {
    let mut p = new_provider(runtime.to_string());
    let reply = client.get(key, "/key");
    let Reply::Ok(body) = &reply else {
        fail(&mut p, Failure::from_reply(&reply));
        return p;
    };
    let Some(d) = data(body) else {
        fail(&mut p, Failure::Unavailable);
        return p;
    };
    apply_self_description(&mut p, d, now);
    p
}

/// Fill the limit and the three calendar periods from a `GET /key` `data`.
fn apply_self_description(p: &mut ProviderQuota, d: &Json, now: u64) {
    let budget = d.get("effective_budget").filter(|b| b.is_object());
    let budget_limit = budget.and_then(|b| {
        let limit = num(b.get("limit_usd"))?;
        let remaining =
            num(b.get("remaining_usd")).or_else(|| num(b.get("spend_usd")).map(|s| limit - s))?;
        Some((
            limit,
            remaining,
            str_of(b.get("resets_at")).map(str::to_string),
        ))
    });
    let own_limit = || {
        let limit = num(d.get("limit"))?;
        let remaining = num(d.get("limit_remaining"))?;
        let reset = next_reset(str_of(d.get("limit_reset")), now).map(format_iso);
        Some((limit, remaining, reset))
    };
    if let Some((limit, remaining, reset)) = budget_limit.or_else(own_limit) {
        push_limit(p, limit, remaining, reset);
    }
    for (id, label) in [("daily", "today"), ("weekly", "week"), ("monthly", "month")] {
        let total = sum_opt(&[
            num(d.get(format!("usage_{id}").as_str())),
            num(d.get(format!("byok_usage_{id}").as_str())),
        ]);
        push_figure(p, label, label, total);
    }
}

// ---------------------------------------------------------------------------
// The management key: GET /key, GET /keys, GET /credits, POST analytics
// ---------------------------------------------------------------------------

/// One entry of `GET /keys`. The hash stays in this module.
#[derive(Debug, Clone, PartialEq)]
struct ListedKey {
    name: String,
    hash: String,
    last_used: Option<u64>,
    limit: Option<f64>,
    remaining: Option<f64>,
    reset: Option<String>,
}

fn parse_keys_page(body: &Json) -> Option<(Vec<ListedKey>, usize)> {
    let arr = data(body)?.as_array()?;
    let keys = arr
        .iter()
        .filter(|k| k.get("disabled").and_then(Json::as_bool) != Some(true))
        .filter_map(|k| {
            Some(ListedKey {
                name: str_of(k.get("name"))?.to_string(),
                hash: str_of(k.get("hash"))?.to_string(),
                last_used: str_of(k.get("last_used_at")).and_then(parse_iso8601_utc_epoch),
                limit: num(k.get("limit")),
                remaining: num(k.get("limit_remaining")),
                reset: str_of(k.get("limit_reset")).map(str::to_string),
            })
        })
        .collect();
    Some((keys, arr.len()))
}

/// Which key the line is about.
#[derive(Debug, PartialEq)]
enum Pick<'a> {
    One(&'a ListedKey),
    All,
    /// A name was given and no enabled key has it.
    Missing,
}

/// A given name selects the enabled key of that name (of two, the more
/// recently used); no name selects the only enabled key when there is exactly
/// one, otherwise all keys.
fn pick_key<'a>(keys: &'a [ListedKey], name: Option<&str>) -> Pick<'a> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => keys
            .iter()
            .filter(|k| k.name == n)
            .max_by_key(|k| k.last_used.unwrap_or(0))
            .map(Pick::One)
            .unwrap_or(Pick::Missing),
        None if keys.len() == 1 => Pick::One(&keys[0]),
        None => Pick::All,
    }
}

/// Sum analytics rows per model, drop models without spend, and keep the four
/// most expensive with the rest combined. Ties order by model name.
fn summarize_usage(rows: &[Json], window: UsageWindow, truncated: bool) -> ModelUsage {
    let mut per_model: Vec<(String, f64)> = Vec::new();
    for row in rows {
        let Some(model) = str_of(row.get("model")) else {
            continue;
        };
        let usd = num(row.get("total_usage")).unwrap_or(0.0);
        match per_model.iter_mut().find(|(m, _)| m == model) {
            Some((_, total)) => *total += usd,
            None => per_model.push((model.to_string(), usd)),
        }
    }
    per_model.retain(|(_, usd)| *usd > 0.0);
    per_model.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total_usd = per_model.iter().map(|(_, u)| u).sum();
    let rest_models = per_model.split_off(per_model.len().min(TOP_MODELS));
    let rest = (!rest_models.is_empty()).then(|| RestSpend {
        models: rest_models.len(),
        usd: rest_models.iter().map(|(_, u)| u).sum(),
    });
    ModelUsage {
        window: window.as_str().to_string(),
        total_usd,
        top: per_model
            .into_iter()
            .map(|(model, usd)| ModelSpend { model, usd })
            .collect(),
        rest,
        partial: truncated,
    }
}

fn analytics_body(window: UsageWindow, hash: Option<&str>, now: u64) -> Json {
    let mut body = json!({
        "metrics": ["total_usage"],
        "dimensions": ["model"],
        "granularity": window.granularity(),
        "time_range": {
            "start": format_iso(now.saturating_sub(window.secs())),
            "end": format_iso(now),
        },
        "limit": ANALYTICS_ROW_LIMIT,
    });
    if let Some(h) = hash {
        body["filters"] = json!([{ "field": "api_key_id", "operator": "eq", "value": h }]);
    }
    body
}

fn read_management(
    client: &Client,
    key: &OpenRouterKey,
    window: UsageWindow,
    key_name: Option<&str>,
    now: u64,
) -> (ProviderQuota, Vec<String>) {
    let mut p = new_provider("all keys".to_string());

    // 1. Is it a management key, and when does it expire.
    let reply = client.get(key, "/key");
    match &reply {
        Reply::Ok(body) => {
            if let Some(exp) = data(body)
                .and_then(|d| str_of(d.get("expires_at")))
                .and_then(parse_iso8601_utc_epoch)
            {
                if exp <= now {
                    p.note = Some("key expired".to_string());
                } else if exp - now <= EXPIRY_NOTE_SECS {
                    p.note = Some(format!("key expires in {}", format_reset(exp - now)));
                }
            }
        }
        // An expired or wrong key fails every request: say so and stop.
        Reply::Status(401) => {
            fail(&mut p, Failure::SignIn);
            return (p, Vec::new());
        }
        other => fail(&mut p, Failure::from_reply(other)),
    }

    // 2. The key list.
    let mut keys: Vec<ListedKey> = Vec::new();
    let mut listed = true;
    for page in 0..MAX_KEY_PAGES {
        let reply = client.get(key, &format!("/keys?offset={}", page * KEYS_PAGE));
        let Reply::Ok(body) = &reply else {
            fail(&mut p, Failure::from_reply(&reply));
            listed = false;
            break;
        };
        let Some((mut page_keys, raw_len)) = parse_keys_page(body) else {
            fail(&mut p, Failure::Unavailable);
            listed = false;
            break;
        };
        keys.append(&mut page_keys);
        if raw_len < KEYS_PAGE {
            break;
        }
    }
    let mut key_names: Vec<String> = keys.iter().map(|k| k.name.clone()).collect();
    key_names.sort();
    key_names.dedup();

    let pick = if listed {
        pick_key(&keys, key_name)
    } else {
        Pick::All
    };
    let picked = match pick {
        Pick::One(k) => {
            p.source = Some(k.name.clone());
            Some(k)
        }
        Pick::All => None,
        Pick::Missing => {
            p.source = key_name.map(str::to_string);
            p.note = Some("no enabled key of that name".to_string());
            None
        }
    };

    // 3. The account's balance.
    let reply = client.get(key, "/credits");
    let credits = match &reply {
        Reply::Ok(body) => {
            data(body).and_then(|d| Some(num(d.get("total_credits"))? - num(d.get("total_usage"))?))
        }
        other => {
            fail(&mut p, Failure::from_reply(other));
            None
        }
    };

    // The limit, and the balance where it is what stops requests first.
    let limit = picked.and_then(|k| Some((k.limit?, k.remaining?, k)));
    match limit {
        Some((l, r, k)) => {
            push_limit(
                &mut p,
                l,
                r,
                next_reset(k.reset.as_deref(), now).map(format_iso),
            );
            if credits.is_some_and(|c| c < r) {
                push_figure(&mut p, "credits_left", "credits", credits);
            }
        }
        None => push_figure(&mut p, "credits_left", "credits", credits),
    }

    // 4. Spend per model. A missing name shows no usage rather than the
    // whole account's under that name.
    if pick != Pick::Missing {
        let body = analytics_body(window, picked.map(|k| k.hash.as_str()), now);
        let reply = client.post(key, "/analytics/query", &body);
        match &reply {
            Reply::Ok(body) => {
                let inner = data(body);
                let rows = inner.and_then(|d| d.get("data")).and_then(Json::as_array);
                match rows {
                    Some(rows) => {
                        let truncated = inner
                            .and_then(|d| d.get("metadata"))
                            .and_then(|m| m.get("truncated"))
                            .and_then(Json::as_bool)
                            .unwrap_or(false);
                        p.usage = Some(summarize_usage(rows, window, truncated));
                    }
                    None => fail(&mut p, Failure::Unavailable),
                }
            }
            other => fail(&mut p, Failure::from_reply(other)),
        }
    }
    (p, key_names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    const KEY: &str = "fake-openrouter-test-key-reader";
    /// 2026-11-06T12:00:00Z, a Friday.
    const NOW: u64 = 1_793_966_400;

    #[test]
    fn now_constant_is_what_it_says() {
        assert_eq!(format_iso(NOW), "2026-11-06T12:00:00Z");
    }

    // --- a local stand-in for OpenRouter ---

    #[derive(Debug, Clone)]
    struct Recorded {
        method: String,
        path: String,
        auth: Option<String>,
        body: String,
    }

    struct Route {
        method: &'static str,
        path: &'static str,
        status: u16,
        body: String,
        location: Option<String>,
    }

    fn route(method: &'static str, path: &'static str, status: u16, body: &str) -> Route {
        Route {
            method,
            path,
            status,
            body: body.to_string(),
            location: None,
        }
    }

    struct Stub {
        base: String,
        port: u16,
        log: Arc<Mutex<Vec<Recorded>>>,
    }

    impl Stub {
        fn start(routes: Vec<Route>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let log = Arc::new(Mutex::new(Vec::new()));
            let log2 = Arc::clone(&log);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    serve(stream, &routes, &log2);
                }
            });
            Self {
                base: format!("http://127.0.0.1:{port}/api/v1"),
                port,
                log,
            }
        }

        fn client(&self) -> Client {
            Client::build(self.base.clone(), false, Duration::from_secs(5))
        }

        fn requests(&self) -> Vec<Recorded> {
            self.log.lock().unwrap().clone()
        }
    }

    fn serve(stream: TcpStream, routes: &[Route], log: &Mutex<Vec<Recorded>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let mut auth = None;
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                break;
            }
            let (name, value) = h.split_once(':').unwrap_or((&h, ""));
            let value = value.trim().to_string();
            match name.to_ascii_lowercase().as_str() {
                "authorization" => auth = Some(value),
                "content-length" => len = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        let mut body = vec![0u8; len];
        let _ = reader.read_exact(&mut body);
        let path = target.split('?').next().unwrap_or("").to_string();
        log.lock().unwrap().push(Recorded {
            method: method.clone(),
            path: target.clone(),
            auth,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        let found = routes
            .iter()
            .find(|r| r.method == method && format!("/api/v1{}", r.path) == path);
        let (status, text, location) = match found {
            Some(r) => (r.status, r.body.clone(), r.location.clone()),
            None => (404, "{}".to_string(), None),
        };
        let mut resp = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            text.len()
        );
        if let Some(l) = location {
            resp.push_str(&format!("Location: {l}\r\n"));
        }
        resp.push_str("\r\n");
        resp.push_str(&text);
        let mut s = stream;
        let _ = s.write_all(resp.as_bytes());
        let _ = s.flush();
    }

    fn key() -> OpenRouterKey {
        OpenRouterKey::new(KEY).unwrap()
    }

    fn management(name: Option<&str>) -> OpenRouterSource {
        OpenRouterSource::Management {
            key: key(),
            window: UsageWindow::Day,
            key_name: name.map(str::to_string),
        }
    }

    // Fixtures built from the OpenAPI document's examples.
    const SELF_MGMT: &str = r#"{"data": {"is_management_key": true, "expires_at": "2027-12-31T23:59:59Z", "label": "sk-or-v1-au7...890", "usage": 0}}"#;
    const KEYS_ONE: &str = r#"{"data": [{"name": "My Production Key", "hash": "f01d52606dc8f0a8303a7b5cc3fa07109c2e346cec7c0a16b40de462992ce943", "label": "Production API Key", "disabled": false, "last_used_at": "2026-09-15T15:45:00Z", "limit": 100, "limit_remaining": 74.5, "limit_reset": "monthly", "usage": 25.5, "workspace_id": "0df9e665-d932-5740-b2c7-b52af166bc11"}]}"#;
    const CREDITS: &str = r#"{"data": {"total_credits": 100.5, "total_usage": 25.75}}"#;
    const ANALYTICS: &str = r#"{"data": {"data": [
        {"date__hour": "2026-11-06T10:00:00.000Z", "model": "openai/gpt-4.1", "total_usage": 1.5},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "openai/gpt-4.1", "total_usage": "0.6"},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "google/gemini-3-flash-preview", "total_usage": 0.8},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "anthropic/claude-sonnet-4", "total_usage": 0.21},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "openai/gpt-4o", "total_usage": 0.07},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "a/one", "total_usage": 0.01},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "a/two", "total_usage": 0.01},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "a/free", "total_usage": 0},
        {"date__hour": "2026-11-06T11:00:00.000Z", "model": "a/null", "total_usage": null}
    ], "metadata": {"query_time_ms": 42, "row_count": 9, "truncated": false}}}"#;
    const SELF_REGULAR: &str = r#"{"data": {"is_management_key": false, "label": "sk-or-v1-au7...890", "limit": 100, "limit_remaining": 74.5, "limit_reset": "monthly", "usage_daily": 3.0, "byok_usage_daily": 0.2, "usage_weekly": 11.4, "byok_usage_weekly": null, "usage_monthly": "25.5", "effective_budget": null, "creator_user_id": "user_x"}}"#;

    fn full_routes() -> Vec<Route> {
        vec![
            route("GET", "/key", 200, SELF_MGMT),
            route("GET", "/keys", 200, KEYS_ONE),
            route("GET", "/credits", 200, CREDITS),
            route("POST", "/analytics/query", 200, ANALYTICS),
        ]
    }

    fn run(stub: &Stub, source: &OpenRouterSource, env: &HarnessEnv) -> OpenRouterResult {
        fetch_with(&stub.client(), source, env, NOW)
    }

    #[test]
    fn management_key_full_line() {
        let stub = Stub::start(full_routes());
        let r = run(&stub, &management(None), &HarnessEnv::default());
        let p = r.provider.unwrap();
        assert_eq!(p.status, ProviderStatus::Fresh);
        assert_eq!(p.error, None);
        assert_eq!(p.source.as_deref(), Some("My Production Key"));
        assert_eq!(r.key_names, vec!["My Production Key".to_string()]);
        // Limit: 25.5% used, resetting on 1 Dec.
        assert_eq!(p.windows.len(), 1);
        assert!((p.windows[0].percent_used - 25.5).abs() < 1e-9);
        assert_eq!(
            p.windows[0].resets_at.as_deref(),
            Some("2026-12-01T00:00:00Z")
        );
        assert_eq!(p.figures[0].id, "limit_left");
        assert_eq!(p.figures[0].usd, 74.5);
        assert_eq!(p.figures[0].cap_usd, Some(100.0));
        // Balance 74.75 is not below the limit left: not shown.
        assert_eq!(p.figures.len(), 1);
        let u = p.usage.unwrap();
        assert_eq!(u.window, "1d");
        assert_eq!(
            u.top.iter().map(|m| m.model.as_str()).collect::<Vec<_>>(),
            [
                "openai/gpt-4.1",
                "google/gemini-3-flash-preview",
                "anthropic/claude-sonnet-4",
                "openai/gpt-4o"
            ]
        );
        assert!((u.top[0].usd - 2.1).abs() < 1e-9);
        assert_eq!(u.rest.as_ref().unwrap().models, 2);
        assert!((u.rest.unwrap().usd - 0.02).abs() < 1e-9);
        assert!((u.total_usd - 3.2).abs() < 1e-9);
        assert!(!u.partial);
        assert_eq!(p.note, None, "expiry far away");

        // Four requests, in order, all with the key and to the stub only.
        let reqs = stub.requests();
        let seen: Vec<String> = reqs
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        assert_eq!(
            seen,
            [
                "GET /api/v1/key",
                "GET /api/v1/keys?offset=0",
                "GET /api/v1/credits",
                "POST /api/v1/analytics/query"
            ]
        );
        assert!(reqs
            .iter()
            .all(|r| r.auth.as_deref() == Some(&format!("Bearer {KEY}")[..])));
        let body: Json = serde_json::from_str(&reqs[3].body).unwrap();
        assert_eq!(body["metrics"], json!(["total_usage"]));
        assert_eq!(body["dimensions"], json!(["model"]));
        assert_eq!(body["granularity"], "hour");
        assert_eq!(body["time_range"]["start"], "2026-11-05T12:00:00Z");
        assert_eq!(body["time_range"]["end"], "2026-11-06T12:00:00Z");
        assert_eq!(
            body["filters"][0]["value"],
            "f01d52606dc8f0a8303a7b5cc3fa07109c2e346cec7c0a16b40de462992ce943"
        );
        assert_eq!(body["filters"][0]["field"], "api_key_id");
    }

    #[test]
    fn hour_window_asks_for_minutes() {
        let b = analytics_body(UsageWindow::Hour, None, NOW);
        assert_eq!(b["granularity"], "minute");
        assert_eq!(b["time_range"]["start"], "2026-11-06T11:00:00Z");
        assert!(b.get("filters").is_none());
        let b = analytics_body(UsageWindow::Week, None, NOW);
        assert_eq!(b["granularity"], "hour");
        assert_eq!(b["time_range"]["start"], "2026-10-30T12:00:00Z");
    }

    #[test]
    fn usage_window_parses_only_the_three_values() {
        for w in UsageWindow::ALL {
            assert_eq!(w.as_str().parse::<UsageWindow>(), Ok(w));
        }
        assert!("2h".parse::<UsageWindow>().is_err());
        assert_eq!(UsageWindow::default(), UsageWindow::Day);
    }

    fn listed(name: &str, last_used: Option<u64>) -> ListedKey {
        ListedKey {
            name: name.to_string(),
            hash: format!("hash-{name}-{}", last_used.unwrap_or(0)),
            last_used,
            limit: None,
            remaining: None,
            reset: None,
        }
    }

    #[test]
    fn key_pick_rule() {
        let one = vec![listed("a", None)];
        assert_eq!(pick_key(&one, None), Pick::One(&one[0]));
        let several = vec![
            listed("a", Some(5)),
            listed("b", Some(9)),
            listed("a", Some(7)),
        ];
        assert_eq!(pick_key(&several, None), Pick::All);
        assert_eq!(pick_key(&several, Some("b")), Pick::One(&several[1]));
        // Duplicate name: the more recently used.
        assert_eq!(pick_key(&several, Some("a")), Pick::One(&several[2]));
        assert_eq!(pick_key(&several, Some("zzz")), Pick::Missing);
        // A blank name is no name.
        assert_eq!(pick_key(&one, Some("  ")), Pick::One(&one[0]));
        assert_eq!(pick_key(&[], None), Pick::All);
    }

    #[test]
    fn several_keys_without_a_name_show_all_keys_and_credits() {
        let keys = r#"{"data": [
            {"name": "a", "hash": "h1", "disabled": false, "limit": 10, "limit_remaining": 5},
            {"name": "b", "hash": "h2", "disabled": false, "limit": null, "limit_remaining": null},
            {"name": "off", "hash": "h3", "disabled": true}
        ]}"#;
        let stub = Stub::start(vec![
            route("GET", "/key", 200, SELF_MGMT),
            route("GET", "/keys", 200, keys),
            route("GET", "/credits", 200, CREDITS),
            route("POST", "/analytics/query", 200, ANALYTICS),
        ]);
        let r = run(&stub, &management(None), &HarnessEnv::default());
        let p = r.provider.unwrap();
        assert_eq!(p.source.as_deref(), Some("all keys"));
        assert_eq!(r.key_names, vec!["a".to_string(), "b".to_string()]);
        assert!(p.windows.is_empty());
        assert_eq!(p.figures.len(), 1);
        assert_eq!(p.figures[0].id, "credits_left");
        assert!((p.figures[0].usd - 74.75).abs() < 1e-9);
        let body: Json = serde_json::from_str(&stub.requests()[3].body).unwrap();
        assert!(body.get("filters").is_none(), "all keys: no filter");
    }

    #[test]
    fn missing_name_shows_no_usage_under_that_name() {
        let stub = Stub::start(full_routes());
        let r = run(&stub, &management(Some("nope")), &HarnessEnv::default());
        let p = r.provider.unwrap();
        assert!(p.usage.is_none());
        assert_eq!(p.note.as_deref(), Some("no enabled key of that name"));
        assert!(stub
            .requests()
            .iter()
            .all(|r| r.path != "/api/v1/analytics/query"));
    }

    #[test]
    fn balance_below_limit_shows_both() {
        let credits = r#"{"data": {"total_credits": 10, "total_usage": 7.5}}"#;
        let stub = Stub::start(vec![
            route("GET", "/key", 200, SELF_MGMT),
            route("GET", "/keys", 200, KEYS_ONE),
            route("GET", "/credits", 200, credits),
            route("POST", "/analytics/query", 200, ANALYTICS),
        ]);
        let p = run(&stub, &management(None), &HarnessEnv::default())
            .provider
            .unwrap();
        let ids: Vec<&str> = p.figures.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["limit_left", "credits_left"]);
        assert!((p.figures[1].usd - 2.5).abs() < 1e-9);
    }

    #[test]
    fn keys_pages_until_a_short_page() {
        let full: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"name": "k{i}", "hash": "h{i}", "disabled": false}}"#))
            .collect();
        let page1 = format!(r#"{{"data": [{}]}}"#, full.join(","));
        // The stub ignores the query, so every page is full: the page bound
        // must stop the loop.
        let stub = Stub::start(vec![
            route("GET", "/key", 200, SELF_MGMT),
            route("GET", "/keys", 200, &page1),
            route("GET", "/credits", 200, CREDITS),
            route("POST", "/analytics/query", 200, ANALYTICS),
        ]);
        let r = run(&stub, &management(None), &HarnessEnv::default());
        let pages = stub
            .requests()
            .iter()
            .filter(|r| r.path.starts_with("/api/v1/keys"))
            .count();
        assert_eq!(pages, MAX_KEY_PAGES);
        assert_eq!(r.key_names.len(), 100, "names deduplicated");
    }

    #[test]
    fn expired_management_key_is_sign_in_required_and_stops() {
        let stub = Stub::start(vec![route(
            "GET",
            "/key",
            401,
            r#"{"error": {"message": "API key expired"}}"#,
        )]);
        let p = run(&stub, &management(None), &HarnessEnv::default())
            .provider
            .unwrap();
        assert_eq!(p.status, ProviderStatus::AuthRequired);
        assert_eq!(p.error.as_deref(), Some("sign-in required"));
        assert!(p.figures.is_empty() && p.usage.is_none());
        assert_eq!(stub.requests().len(), 1);
    }

    #[test]
    fn regular_key_in_management_slot_needs_a_management_key() {
        let stub = Stub::start(vec![
            route("GET", "/key", 200, SELF_REGULAR),
            route(
                "GET",
                "/keys",
                403,
                r#"{"error": {"message": "Only management keys can perform this operation"}}"#,
            ),
            route("GET", "/credits", 403, "{}"),
            route("POST", "/analytics/query", 403, "{}"),
        ]);
        let p = run(&stub, &management(None), &HarnessEnv::default())
            .provider
            .unwrap();
        assert_eq!(p.status, ProviderStatus::Error);
        assert_eq!(p.error.as_deref(), Some("needs a management key"));
        assert!(
            !format!("{p:?}").contains("Only management"),
            "no body relayed"
        );
    }

    #[test]
    fn failed_analytics_leaves_the_limit() {
        let stub = Stub::start(vec![
            route("GET", "/key", 200, SELF_MGMT),
            route("GET", "/keys", 200, KEYS_ONE),
            route("GET", "/credits", 200, CREDITS),
            route("POST", "/analytics/query", 408, "{}"),
        ]);
        let p = run(&stub, &management(None), &HarnessEnv::default())
            .provider
            .unwrap();
        assert_eq!(p.figures[0].id, "limit_left");
        assert!(p.usage.is_none());
        assert_eq!(p.error.as_deref(), Some("unavailable"));
    }

    #[test]
    fn truncated_answer_is_partial() {
        let truncated = r#"{"data": {"data": [{"model": "a/b", "total_usage": 1}], "metadata": {"truncated": true}}}"#;
        let stub = Stub::start(vec![
            route("GET", "/key", 200, SELF_MGMT),
            route("GET", "/keys", 200, KEYS_ONE),
            route("GET", "/credits", 200, CREDITS),
            route("POST", "/analytics/query", 200, truncated),
        ]);
        let p = run(&stub, &management(None), &HarnessEnv::default())
            .provider
            .unwrap();
        assert!(p.usage.unwrap().partial);
    }

    #[test]
    fn expiry_within_a_week_adds_a_note() {
        let soon = format!(
            r#"{{"data": {{"is_management_key": true, "expires_at": "{}"}}}}"#,
            format_iso(NOW + 3 * 86_400 + 3600)
        );
        let stub = Stub::start(vec![
            route("GET", "/key", 200, &soon),
            route("GET", "/keys", 200, KEYS_ONE),
            route("GET", "/credits", 200, CREDITS),
            route("POST", "/analytics/query", 200, ANALYTICS),
        ]);
        let p = run(&stub, &management(None), &HarnessEnv::default())
            .provider
            .unwrap();
        assert_eq!(p.note.as_deref(), Some("key expires in 3d1h"));
    }

    #[test]
    fn summarize_usage_sums_sorts_and_ties() {
        let rows: Vec<Json> = serde_json::from_str(
            r#"[{"model": "b", "total_usage": 1}, {"model": "a", "total_usage": 1},
                {"model": "c", "total_usage": 0.5}, {"model": "c", "total_usage": 0.5}]"#,
        )
        .unwrap();
        let u = summarize_usage(&rows, UsageWindow::Week, false);
        let names: Vec<&str> = u.top.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"], "ties by name; buckets summed");
        assert!(u.rest.is_none(), "fewer than five: no rest");
        let empty = summarize_usage(&[], UsageWindow::Day, false);
        assert!(empty.top.is_empty() && empty.rest.is_none());
        assert_eq!(empty.total_usd, 0.0);
    }

    // --- the harness's key ---

    fn harness_env(dir: &std::path::Path) -> HarnessEnv {
        HarnessEnv::new(Some(dir.to_path_buf()), [("OPENROUTER_API_KEY", KEY)])
    }

    #[test]
    fn harness_key_makes_one_request() {
        let stub = Stub::start(vec![route("GET", "/key", 200, SELF_REGULAR)]);
        let tmp = std::env::temp_dir().join(format!("cw-or-h-{}", std::process::id()));
        let source = OpenRouterSource::Harness {
            runtime: "pi".to_string(),
            project_dir: tmp.clone(),
        };
        let r = run(&stub, &source, &harness_env(&tmp));
        let p = r.provider.unwrap();
        assert_eq!(stub.requests().len(), 1);
        assert_eq!(stub.requests()[0].path, "/api/v1/key");
        assert_eq!(p.source.as_deref(), Some("pi"));
        assert!(r.key_names.is_empty());
        assert_eq!(
            r.lookup.unwrap().status,
            crate::harness_key::LookupStatus::Found(crate::harness_key::SourceKind::Environment)
        );
        let figs: Vec<(&str, f64)> = p.figures.iter().map(|f| (f.id.as_str(), f.usd)).collect();
        assert_eq!(figs[0], ("limit_left", 74.5));
        assert_eq!(figs[1].0, "today");
        assert!((figs[1].1 - 3.2).abs() < 1e-9, "byok added");
        assert_eq!(figs[2], ("week", 11.4));
        assert_eq!(figs[3], ("month", 25.5));
        assert_eq!(
            p.windows[0].resets_at.as_deref(),
            Some("2026-12-01T00:00:00Z")
        );
        assert!(p.usage.is_none());
    }

    #[test]
    fn harness_effective_budget_wins_over_own_limit() {
        let body = r#"{"data": {"limit": 100, "limit_remaining": 74.5, "limit_reset": "monthly",
            "effective_budget": {"limit_usd": 50, "remaining_usd": 17.5, "spend_usd": 32.5, "resets_at": "2026-11-09T00:00:00.000Z"},
            "usage_daily": 1, "usage_weekly": 2, "usage_monthly": 3}}"#;
        let mut p = new_provider("pi".to_string());
        apply_self_description(
            &mut p,
            &serde_json::from_str::<Json>(body).unwrap()["data"],
            NOW,
        );
        assert!((p.windows[0].percent_used - 65.0).abs() < 1e-9);
        assert_eq!(
            p.windows[0].resets_at.as_deref(),
            Some("2026-11-09T00:00:00.000Z")
        );
        assert_eq!(p.figures[0].usd, 17.5);
        // No limit and no budget: the three periods alone.
        let mut p = new_provider("pi".to_string());
        let d = json!({"limit": null, "limit_remaining": null, "usage_daily": 1, "usage_weekly": 2, "usage_monthly": 3});
        apply_self_description(&mut p, &d, NOW);
        assert!(p.windows.is_empty());
        let ids: Vec<&str> = p.figures.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["today", "week", "month"]);
    }

    #[test]
    fn harness_without_key_asks_nothing() {
        let stub = Stub::start(vec![route("GET", "/key", 200, SELF_REGULAR)]);
        let tmp = std::env::temp_dir().join(format!("cw-or-none-{}", std::process::id()));
        let source = OpenRouterSource::Harness {
            runtime: "pi".to_string(),
            project_dir: tmp.clone(),
        };
        let r = run(
            &stub,
            &source,
            &HarnessEnv::new(Some(tmp), Vec::<(String, String)>::new()),
        );
        assert!(r.provider.is_none());
        assert_eq!(
            r.lookup.unwrap().status,
            crate::harness_key::LookupStatus::NoKey
        );
        assert!(stub.requests().is_empty());
    }

    #[test]
    fn harness_key_refused_is_sign_in_required() {
        let stub = Stub::start(vec![route("GET", "/key", 401, "{}")]);
        let tmp = std::env::temp_dir();
        let source = OpenRouterSource::Harness {
            runtime: "opencode".to_string(),
            project_dir: tmp.join("cw-or-nonexistent-project"),
        };
        let env = HarnessEnv::new(
            Some(tmp.join("cw-or-nonexistent-home")),
            [("OPENROUTER_API_KEY", KEY)],
        );
        let p = run(&stub, &source, &env).provider.unwrap();
        assert_eq!(p.status, ProviderStatus::AuthRequired);
        assert!(p.figures.is_empty());
    }

    // --- transport and secrecy ---

    #[test]
    fn redirect_is_not_followed() {
        let elsewhere = Stub::start(vec![route("GET", "/key", 200, SELF_REGULAR)]);
        let mut r = route("GET", "/key", 302, "{}");
        r.location = Some(format!("http://127.0.0.1:{}/api/v1/key", elsewhere.port));
        let stub = Stub::start(vec![r]);
        let reply = stub.client().get(&key(), "/key");
        assert!(matches!(reply, Reply::Status(302)));
        assert!(
            elsewhere.requests().is_empty(),
            "key sent to a redirect target"
        );
    }

    #[test]
    fn production_client_refuses_plain_http() {
        // https_only: a plain-http base fails before anything is sent.
        let stub = Stub::start(vec![route("GET", "/key", 200, SELF_REGULAR)]);
        let c = Client::build(stub.base.clone(), true, Duration::from_secs(5));
        assert!(matches!(c.get(&key(), "/key"), Reply::Failed));
        assert!(stub.requests().is_empty());
        assert_eq!(API_BASE, "https://openrouter.ai/api/v1");
    }

    #[test]
    fn unparseable_body_and_unreachable_host_are_unavailable() {
        let stub = Stub::start(vec![route("GET", "/key", 200, "not json")]);
        assert!(matches!(stub.client().get(&key(), "/key"), Reply::Failed));
        // A closed port.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let c = Client::build(
            format!("http://127.0.0.1:{port}/api/v1"),
            false,
            Duration::from_secs(2),
        );
        assert!(matches!(c.get(&key(), "/key"), Reply::Failed));
    }

    #[test]
    fn key_never_appears_in_debug_or_errors() {
        let k = key();
        assert_eq!(format!("{k:?}"), "OpenRouterKey(<redacted>)");
        let src = management(Some("x"));
        assert!(!format!("{src:?}").contains(KEY));
        let stub = Stub::start(vec![route("GET", "/key", 500, KEY)]);
        let r = run(&stub, &src, &HarnessEnv::default());
        let text = format!("{:?}", r);
        assert!(!text.contains(KEY), "{text}");
        assert_eq!(OpenRouterKey::new("").unwrap_err(), KeyError::Empty);
        assert_eq!(
            OpenRouterKey::new("sk or v1").unwrap_err(),
            KeyError::Malformed
        );
        assert!(!KeyError::Malformed.to_string().contains("sk"));
        assert_eq!(OpenRouterKey::new(&format!("  {KEY}\n")).unwrap(), k);
    }

    #[test]
    fn next_reset_rules() {
        // 2026-11-06 is a Friday.
        assert_eq!(
            next_reset(Some("daily"), NOW).map(format_iso).unwrap(),
            "2026-11-07T00:00:00Z"
        );
        assert_eq!(
            next_reset(Some("weekly"), NOW).map(format_iso).unwrap(),
            "2026-11-09T00:00:00Z"
        );
        assert_eq!(
            next_reset(Some("monthly"), NOW).map(format_iso).unwrap(),
            "2026-12-01T00:00:00Z"
        );
        // A Monday's weekly reset is the next Monday.
        let monday = parse_iso8601_utc_epoch("2026-11-09T00:00:00Z").unwrap();
        assert_eq!(
            next_reset(Some("weekly"), monday).map(format_iso).unwrap(),
            "2026-11-16T00:00:00Z"
        );
        // Month and year ends.
        let dec31 = parse_iso8601_utc_epoch("2026-12-31T23:59:59Z").unwrap();
        assert_eq!(
            next_reset(Some("monthly"), dec31).map(format_iso).unwrap(),
            "2027-01-01T00:00:00Z"
        );
        assert_eq!(
            next_reset(Some("daily"), dec31).map(format_iso).unwrap(),
            "2027-01-01T00:00:00Z"
        );
        let feb = parse_iso8601_utc_epoch("2028-02-29T10:00:00Z").unwrap();
        assert_eq!(
            next_reset(Some("monthly"), feb).map(format_iso).unwrap(),
            "2028-03-01T00:00:00Z"
        );
        assert_eq!(next_reset(None, NOW), None);
        assert_eq!(next_reset(Some("yearly"), NOW), None);
    }

    #[test]
    fn format_iso_round_trips() {
        for t in [0u64, 951_782_400, NOW, 4_102_444_799] {
            assert_eq!(parse_iso8601_utc_epoch(&format_iso(t)), Some(t));
        }
    }

    #[test]
    fn merge_replaces_quota_axi_openrouter() {
        let mut ours = new_provider("pi".to_string());
        ours.figures.push(QuotaFigure {
            id: "today".into(),
            label: "today".into(),
            usd: 1.0,
            cap_usd: None,
        });
        let mut axi_or = new_provider("x".to_string());
        axi_or.source = None;
        let mut claude = new_provider("x".to_string());
        claude.id = "claude".into();
        let report = QuotaReport {
            schema_version: Some(5),
            generated_at: String::new(),
            providers: vec![claude.clone(), axi_or],
        };
        let QuotaFetch::Report(r) = merge(QuotaFetch::Report(report), Some(ours.clone())) else {
            panic!()
        };
        assert_eq!(r.providers.len(), 2);
        assert_eq!(r.providers[1], ours);
        // quota-axi failed: our line alone.
        let QuotaFetch::Report(r) = merge(QuotaFetch::Failed("x".into()), Some(ours.clone()))
        else {
            panic!()
        };
        assert_eq!(r.providers, vec![ours]);
        // No line: quota-axi's fetch unchanged.
        assert!(matches!(
            merge(QuotaFetch::Failed("x".into()), None),
            QuotaFetch::Failed(_)
        ));
    }
}
