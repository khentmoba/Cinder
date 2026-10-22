// Cinder — coding agent usage statistics collector
//
// Parses the local session stores of the coding agents installed on this machine
// and emits per-day, per-agent, per-model rows. The UI aggregates by time range.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod alerts;
mod budget;
mod cache;
mod prefilter;
mod pricing;
mod project;
mod quota;
mod tray;
mod updater;

#[cfg(test)]
mod testserver;

use alerts::Alert;
use budget::{Plan, PlanStatus};
use cache::{file_stamp, CachedDb, CachedFile, CachedLimit, ScanCache};
use prefilter::Prefilter;
use pricing::Pricing;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

/// How the money behind a row is actually paid for.
///
/// Cinder prices every token against public list prices, so the dollar figure on
/// screen is an *equivalent* value, never an invoice. A flat-rate plan shows up
/// as a subscription even though the card is not charged per token, and a local
/// model costs nothing at all.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Billing {
    /// No route information was available, so the mode is unknown.
    #[default]
    Unknown,
    /// Covered by a flat-rate plan (Claude Pro/Max, ChatGPT, Copilot, ...).
    Subscription,
    /// Billed per token through a provider API key.
    Api,
    /// Local inference: no provider bill at all.
    Local,
}

impl Billing {
    /// The short badge the UI puts next to a cost figure.
    pub fn label(self) -> &'static str {
        match self {
            Billing::Unknown => "",
            Billing::Subscription => "Plan",
            Billing::Api => "API",
            Billing::Local => "Local",
        }
    }
}

/// How an agent's own route is paid for.
///
/// Claude Code and Codex both record in their credential file whether they run
/// on a flat-rate plan (Claude Pro/Max, ChatGPT) or on an API key, so the
/// answer is read from the machine rather than guessed from the agent name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// A flat-rate plan covers the usage.
    Plan,
    /// Billed per token through an API key.
    Api,
    /// The machine holds no credential that says either way.
    Unknown,
}

impl Route {
    pub fn billing(self) -> Billing {
        match self {
            Route::Plan => Billing::Subscription,
            Route::Api => Billing::Api,
            Route::Unknown => Billing::Unknown,
        }
    }
}

/// Read a small JSON file, if it is there and it parses.
///
/// A file written by Notepad or by PowerShell starts with a byte order mark,
/// which is not part of the document and which the parser refuses, so it is
/// taken off first.
pub fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_json::from_str(text).ok()
}

/// Tell Windows which app this is.
///
/// Without it, a notification raised by a run that was started from a terminal
/// is filed under the terminal's name, and the toast says "Windows
/// PowerShell" instead of "Cinder". The id is the one the installer puts on
/// the shortcut, so the notifications group with the installed app.
#[cfg(windows)]
fn claim_app_id(id: &str) {
    #[link(name = "shell32")]
    extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(pszappid: *const u16) -> i32;
    }
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    // The call is told which app this is; it says whether it took.
    let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
}

#[cfg(not(windows))]
fn claim_app_id(_id: &str) {}

/// How Claude Code is signed in on this machine.
fn claude_route(home: &Path) -> Route {
    // An API key in the environment bills every run per token.
    if std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        return Route::Api;
    }
    // Claude Code keeps a key in its settings when one is configured.
    for settings in [
        home.join(".claude").join("settings.json"),
        home.join(".claude").join("settings.local.json"),
    ] {
        if let Some(v) = read_json_file(&settings) {
            let key = v
                .get("env")
                .and_then(|e| e.get("ANTHROPIC_API_KEY"))
                .and_then(|x| x.as_str());
            if key.is_some_and(|k| !k.trim().is_empty()) {
                return Route::Api;
            }
        }
    }
    let Some(v) = read_json_file(&home.join(".claude").join(".credentials.json")) else {
        return Route::Unknown;
    };
    if v.get("claudeAiOauth").is_some() {
        return Route::Plan;
    }
    if v.get("apiKey").and_then(|x| x.as_str()).is_some_and(|k| !k.trim().is_empty()) {
        return Route::Api;
    }
    Route::Unknown
}

/// How Codex is signed in on this machine.
fn codex_route(home: &Path) -> Route {
    let mut candidates = vec![home.join(".codex").join("auth.json")];
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        if !dir.trim().is_empty() {
            candidates.push(PathBuf::from(dir).join("auth.json"));
        }
    }
    for path in candidates {
        let Some(v) = read_json_file(&path) else { continue };
        match v.get("auth_mode").and_then(|x| x.as_str()) {
            Some("chatgpt") => return Route::Plan,
            Some(m) if m.contains("apikey") || m.contains("api_key") => return Route::Api,
            _ => {}
        }
        if v
            .get("OPENAI_API_KEY")
            .and_then(|x| x.as_str())
            .is_some_and(|k| !k.trim().is_empty())
        {
            return Route::Api;
        }
    }
    Route::Unknown
}

/// The billing mode of a row, from the evidence the log carries.
///
/// A reported cost above zero is a real charge. A reported zero with no public
/// price means the route is covered by a plan. A missing report with a public
/// price is our own estimate of an API-billed run.
pub fn billing_of(reported: Option<f64>, priced: bool, local: bool) -> Billing {
    if local {
        return Billing::Local;
    }
    match reported {
        Some(c) if c > 0.0 => Billing::Api,
        Some(_) if !priced => Billing::Subscription,
        Some(_) => Billing::Api,
        None if priced => Billing::Api,
        None => Billing::Unknown,
    }
}

/// Where the money on a row came from.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CostSource {
    /// Priced against the public list prices.
    #[default]
    Estimated,
    /// The figure the agent itself reported.
    Reported,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Row {
    pub day: String,
    pub agent: String,
    pub model: String,
    pub requests: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The subset of `cache_write` billed at the 1-hour ephemeral cache rate
    /// (2x the base input price). Kept apart so the 5-minute and 1-hour
    /// write prices can both be applied.
    pub cache_write_1h: u64,
    pub output: u64,
    pub reasoning: u64,
    pub tools: u64,
    pub cost: f64,
    pub cost_known: bool,
    pub cost_partial: bool,
    pub savings: f64,
    /// Canonical repository (or project) the run happened in. Git worktrees
    /// roll up into their main checkout, so every branch of a repo is one row
    /// group instead of a separate one.
    pub project: String,
    /// How this row is billed. See `Billing`.
    pub billing: Billing,
    /// True when the usage came from a delegated subagent run rather than
    /// from the parent session.
    pub subagent: bool,
    /// For local models: what the same tokens would have cost on a cloud
    /// provider, i.e. the money running locally saved.
    pub cloud_savings: f64,
    /// What the agent itself said the request cost, when it wrote it down.
    #[serde(default)]
    pub cost_reported: Option<f64>,
    /// Where the money on this row came from.
    #[serde(default)]
    pub cost_source: CostSource,
}

impl Row {
    pub fn tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct SessionRow {
    day: String,
    agent: String,
    sessions: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct LimitRow {
    pub agent: String,
    pub window: String,
    pub used_percent: f64,
    pub window_minutes: i64,
    pub resets_at: i64,
    pub plan: String,
    /// "log" when the window was read out of a session file, "live" when it was
    /// fetched from the provider.
    #[serde(default)]
    pub source: String,
    /// Human description of the window, e.g. "5-hour session window".
    #[serde(default)]
    pub detail: String,
    /// How much of the allowance is gone, in dollars, when the provider counts
    /// its allowance in money.
    #[serde(default)]
    pub used_amount: f64,
    /// The size of that allowance, in dollars.
    #[serde(default)]
    pub limit_amount: f64,
}

#[derive(Serialize, Clone)]
struct Source {
    agent: String,
    path: String,
    found: bool,
    files: u64,
    /// How long this source took to scan, in milliseconds.
    scan_ms: u64,
    /// Why this source could not be read. Empty when it could.
    error: String,
}

/// Read one source, and write down how it went.
///
/// A source that goes wrong is noted and skipped: the other agents are still
/// worth counting, and one parser that trips over a file should not cost the
/// user the whole dashboard.
fn scan_source(
    sources: &mut Vec<Source>,
    agent: &str,
    path: &str,
    f: impl FnOnce() -> u64,
) -> u64 {
    let started = std::time::Instant::now();
    let (files, error) = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(n) => (n, String::new()),
        Err(e) => {
            let what = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "the parser stopped unexpectedly".to_string());
            (0, what)
        }
    };
    sources.push(Source {
        agent: agent.into(),
        path: path.into(),
        found: files > 0,
        files,
        scan_ms: started.elapsed().as_millis() as u64,
        error,
    });
    files
}

/// A usage row produced by one agent, for one model, on one day, in one
/// project. The same model used in two repositories is two rows.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Artifact {
    pub agent: String,
    pub url: String,
    pub day: String,
    pub title: String,
}

#[derive(Serialize)]
struct Stats {
    rows: Vec<Row>,
    sessions: Vec<SessionRow>,
    limits: Vec<LimitRow>,
    sources: Vec<Source>,
    pricing: PricingInfo,
    unpriced: Vec<String>,
    /// providers covered by a subscription/plan instead of per-token billing
    plan_routes: Vec<String>,
    /// Live quota windows fetched from the providers, merged into `limits`.
    /// `limits[].source` says which ones came from the network.
    quota_error: String,
    /// When the live quotas were last refreshed (ISO-8601, UTC).
    quota_checked_at: String,
    /// Artifacts the agents published during the scanned period.
    artifacts: Vec<Artifact>,
    /// The things worth interrupting the user about.
    alerts: Vec<Alert>,
    /// How each declared plan looks against the usage it covered.
    plans: Vec<PlanStatus>,
}

#[derive(Serialize)]
struct PricingInfo {
    source: String,
    embedded: usize,
    local: usize,
}

/// (day, agent, model, project, delegated)
pub type Rows = HashMap<(String, String, String, String, bool), Row>;
pub type Sess = HashMap<(String, String), u64>;

/// Provider prefix of a model string ("opencode-go/deepseek-v4-flash" -> "opencode-go").
pub(crate) fn model_provider(model: &str) -> Option<&str> {
    model.split_once('/').map(|(p, _)| p).filter(|p| !p.is_empty() && !p.contains(' '))
}

pub fn add(rows: &mut Rows, sess: &mut Sess, agent: &str, model: &str, day: &str, r: Row) {
    // The project and the delegated flag are part of the key: the same model
    // used in two repositories is two different rows, and a delegated run is
    // counted apart from the session that spawned it.
    let key = (
        day.to_string(),
        agent.to_string(),
        model.to_string(),
        r.project.clone(),
        r.subagent,
    );
    let e = rows.entry(key).or_insert_with(|| Row {
        day: day.to_string(),
        agent: agent.to_string(),
        model: model.to_string(),
        project: r.project.clone(),
        billing: r.billing,
        subagent: r.subagent,
        ..Default::default()
    });
    e.requests += r.requests;
    e.input += r.input;
    e.cache_read += r.cache_read;
    e.cache_write += r.cache_write;
    e.cache_write_1h += r.cache_write_1h;
    e.output += r.output;
    e.reasoning += r.reasoning;
    e.tools += r.tools;
    e.cost += r.cost;
    e.cost_known = e.cost_known || r.cost_known;
    e.cost_partial |= r.cost_partial || (!r.cost_known && r.tokens() > 0);
    e.savings += r.savings;
    e.cloud_savings += r.cloud_savings;
    // What the agent said it cost is kept apart from the estimate, so the two
    // can be compared without one overwriting the other.
    if let Some(reported) = r.cost_reported {
        e.cost_reported = Some(e.cost_reported.unwrap_or(0.0) + reported);
    }
    if r.cost_source == CostSource::Reported {
        e.cost_source = CostSource::Reported;
    }
    // A route that turned out to be billed per token wins over a guess made
    // before the route was known.
    if e.billing == Billing::Unknown {
        e.billing = r.billing;
    }
    e.subagent |= r.subagent;
    let _ = sess;
}

pub fn session(sess: &mut Sess, agent: &str, day: &str) {
    *sess.entry((day.to_string(), agent.to_string())).or_insert(0) += 1;
}

pub(crate) fn day_of(ts: &str) -> String {
    ts.chars().take(10).filter(|c| *c != '\u{0}').collect()
}

fn list_files(root: &Path, ext: &str, name: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, ext: &str, name: Option<&str>, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, ext, name, out);
                } else {
                    let ext_ok = p.extension().map(|x| x == ext).unwrap_or(false);
                    let name_ok = match name {
                        Some(n) => p.file_name().map(|f| f == n).unwrap_or(false),
                        None => true,
                    };
                    if ext_ok && name_ok {
                        out.push(p);
                    }
                }
            }
        }
    }
    if root.exists() {
        walk(root, ext, name, &mut out);
    }
    out
}

fn each_line(path: &Path, mut f: impl FnMut(&str)) {
    // Stream the file instead of read_to_string: session files reach 13 MB and
    // the trees total ~1.6 GB, so holding whole files (plus split lines) spikes
    // RSS and stalls a cold start on page-cache misses. A 256 KiB BufReader with
    // a reused buffer keeps one line resident at a time.
    let Ok(file) = fs::File::open(path) else { return };
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let line = buf.trim_end();
                if !line.is_empty() {
                    f(line);
                }
            }
            Err(_) => break,
        }
    }
}

/// Every line that can contribute a Pi row: the session header (day marker) or an
/// assistant message carrying a usage block.
static PI_KEYS: std::sync::LazyLock<Prefilter> =
    std::sync::LazyLock::new(|| Prefilter::new(&["usage", "session"]));
/// Every line that can contribute a Codex row: the header/context lines, tool calls
/// and token-count events. Response items and turn events are ignored.
static CODEX_KEYS: std::sync::LazyLock<Prefilter> = std::sync::LazyLock::new(|| {
    Prefilter::new(&[
        "session_meta",
        "turn_context",
        "response_item",
        "event_msg",
        "function_call",
        "custom_tool_call",
        "token_count",
    ])
});
/// Claude assistant messages carrying usage blocks. Without this prefilter every
/// line of every project jsonl goes through serde_json for nothing.
static CLAUDE_KEYS: std::sync::LazyLock<Prefilter> =
    std::sync::LazyLock::new(|| Prefilter::new(&["assistant", "usage"]));
/// Planner responses, plus the lines that switch the model.
static AGY_KEYS: std::sync::LazyLock<Prefilter> =
    std::sync::LazyLock::new(|| Prefilter::new(&["PLANNER_RESPONSE", "Model Selection"]));

/// True when a line carries at least one of the markers. A JSONL line that carries
/// none of them cannot change the collected stats, so it is never handed to
/// serde_json. The logs are ~1.6 GB across ~1000 files; skipping half of the lines
/// halves the parse work, which is what a cold start (empty page cache) spends its
/// time on. The screen itself is FFF's SIMD substring search.
fn has_key(line: &str, keys: &std::sync::LazyLock<Prefilter>) -> bool {
    keys.matches(line)
}

// ---------------- Pi ----------------
fn parse_single_pi(
    file: &Path,
    pricing: &Pricing,
    rows: &mut Rows,
    sess: &mut Sess,
    plan_routes: &mut HashMap<String, u64>,
) {
    let mut day = String::new();
    let mut project = String::new();
    each_line(file, |line| {
        if !has_key(line, &PI_KEYS) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        if v.get("type").and_then(|t| t.as_str()) == Some("session") {
            if day.is_empty() {
                day = day_of(ts);
            }
            // The session header names the working directory, which is what
            // ties the run to a repository.
            if project.is_empty() {
                project = project::label_for_cwd(v.get("cwd").and_then(|c| c.as_str()));
            }
        }
        if v.get("type").and_then(|t| t.as_str()) != Some("message") {
            return;
        }
        let Some(msg) = v.get("message") else { return };
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            return;
        }
        let u = msg.get("usage").cloned().unwrap_or(serde_json::Value::Null);
        let input = u.get("input").and_then(|x| x.as_u64()).unwrap_or(0);
        let output = u.get("output").and_then(|x| x.as_u64()).unwrap_or(0);
        let cache_read = u.get("cacheRead").and_then(|x| x.as_u64()).unwrap_or(0);
        let cache_write = u.get("cacheWrite").and_then(|x| x.as_u64()).unwrap_or(0);
        let reasoning = u.get("reasoning").and_then(|x| x.as_u64()).unwrap_or(0);
        if input + output + cache_read + cache_write == 0 {
            return;
        }
        let mut tools = 0u64;
        if let Some(content) = msg.get("content").and_then(|x| x.as_array()) {
            for item in content {
                if item.get("type").and_then(|t| t.as_str()) == Some("toolCall") {
                    tools += 1;
                }
            }
        }
        let model = pricing::normalize_model(msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown"));
        let provider = msg.get("provider").and_then(|p| p.as_str()).map(|s| s.to_lowercase());
        let mut r = Row {
            day: day_of(ts),
            agent: "Pi".into(),
            model: model.clone(),
            requests: 1,
            input,
            cache_read,
            cache_write,
            output,
            reasoning,
            tools,
            project: project.clone(),
            ..Default::default()
        };
        // pi accounts for its own routes. A reported zero is meaningful: it means the
        // route is a plan/subscription or has no per-token billing (e.g. zed-ai),
        // so it must not be re-priced with a same-named model from another provider.
        let reported = u.get("cost").and_then(|c| c.get("total")).and_then(|x| x.as_f64());
        let priced = pricing.lookup(provider.as_deref(), &model);
        r.billing = billing_of(reported, priced.is_some(), false);
        match reported {
            Some(c) if c > 0.0 => {
                r.cost = c;
                r.cost_known = true;
            }
            Some(_) if provider.is_some() && priced.is_none() => {
                // plan route: covered by the subscription, not billed per token
                r.cost = 0.0;
                r.cost_known = true;
            }
            _ => {
                if let Some(p) = priced {
                    r.cost = p.cost(input, cache_read, cache_write, output);
                    r.cost_known = true;
                }
            }
        }
        if let Some(p) = priced {
            r.savings = p.savings(cache_read, cache_write);
        }
        if provider.is_some() && reported == Some(0.0) && priced.is_none() {
            if let Some(p) = provider.as_deref() {
                *plan_routes.entry(p.to_string()).or_insert(0) += input + cache_read + cache_write + output;
            }
        }
        if r.day.is_empty() {
            r.day = day.clone();
        }
        add(rows, sess, "Pi", &model, &r.day.clone(), r);
    });
    if !day.is_empty() {
        session(sess, "Pi", &day);
    }
}

fn parse_pi(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
    plan_routes: &mut HashMap<String, u64>,
) -> u64 {
    let root = home.join(".pi").join("agent").join("sessions");
    let files = list_files(&root, "jsonl", None);
    if files.is_empty() {
        return 0;
    }

    let mut uncached = Vec::new();
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                for (k, v) in &entry.plan_routes {
                    *plan_routes.entry(k.clone()).or_insert(0) += v;
                }
                continue;
            }
            uncached.push((file.clone(), path_str, mtime_ns, size));
        }
    }

    if uncached.is_empty() {
        return files.len() as u64;
    }

    cache.dirty = true;
    let num_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let chunk_size = uncached.len().div_ceil(num_threads);
    let mut thread_results: Vec<Vec<(String, CachedFile)>> = (0..num_threads).map(|_| Vec::new()).collect();

    let pricing_ref = pricing;
    std::thread::scope(|s| {
        for (chunk, out) in uncached.chunks(chunk_size).zip(&mut thread_results) {
            s.spawn(move || {
                for (file, path_str, mtime_ns, size) in chunk {
                    let mut file_rows = Rows::new();
                    let mut file_sess = Sess::new();
                    let mut file_plan = HashMap::new();
                    parse_single_pi(file, pricing_ref, &mut file_rows, &mut file_sess, &mut file_plan);
                    let cached = CachedFile {
                        mtime_ns: *mtime_ns,
                        size: *size,
                        rows: file_rows.into_values().collect(),
                        sessions: file_sess.into_keys().collect(),
                        plan_routes: file_plan.into_iter().collect(),
                        ..Default::default()
                    };
                    out.push((path_str.clone(), cached));
                }
            });
        }
    });

    for list in thread_results {
        for (path_str, entry) in list {
            for r in &entry.rows {
                add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
            }
            for (day, agent) in &entry.sessions {
                session(sess, agent, day);
            }
            for (k, v) in &entry.plan_routes {
                *plan_routes.entry(k.clone()).or_insert(0) += v;
            }
            cache.files.insert(path_str, entry);
        }
    }

    files.len() as u64
}

// ---------------- Codex ----------------
fn parse_single_codex(
    file: &Path,
    pricing: &Pricing,
    route: Route,
    rows: &mut Rows,
    sess: &mut Sess,
    limits_out: &mut Vec<CachedLimit>,
) {
    let mut model = String::from("unknown");
    let mut day = String::new();
    let mut plan = String::new();
    let mut project = String::new();
    let mut file_latest: HashMap<String, (String, LimitRow)> = HashMap::new();
    // A forked rollout replays the parent's earlier turns, so the same
    // (timestamp, usage) pair can appear twice. Keeping the pairs that were
    // already seen counts the fork's own work only once.
    let mut seen_usage: HashSet<(String, u64, u64, u64)> = HashSet::new();
    each_line(file, |line| {
        if !has_key(line, &CODEX_KEYS) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let p = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        if day.is_empty() && ty == "session_meta" {
            day = day_of(ts);
        }
        if project.is_empty() && ty == "session_meta" {
            project = project::label_for_cwd(p.get("cwd").and_then(|c| c.as_str()));
        }
        match ty {
            "turn_context" => {
                if let Some(m) = p.get("model").and_then(|m| m.as_str()) {
                    model = pricing::normalize_model(m);
                }
            }
            "response_item" => {
                let pt = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if pt == "function_call" || pt == "custom_tool_call" {
                    let mut r = Row {
                        day: day_of(ts),
                        agent: "Codex".into(),
                        model: model.clone(),
                        tools: 1,
                        project: project.clone(),
                        billing: route.billing(),
                        ..Default::default()
                    };
                    if r.day.is_empty() { r.day = day.clone(); }
                    add(rows, sess, "Codex", &model, &r.day.clone(), r);
                }
            }
            "event_msg" => {
                if p.get("type").and_then(|t| t.as_str()) != Some("token_count") {
                    return;
                }
                if let Some(ptype) = p.get("rate_limits").and_then(|rl| rl.get("plan_type")).and_then(|x| x.as_str()) {
                    plan = ptype.to_string();
                }
                if let Some(rl) = p.get("rate_limits") {
                    for key in ["primary", "secondary"] {
                        if let Some(w) = rl.get(key) {
                            if let Some(pct) = w.get("used_percent").and_then(|x| x.as_f64()) {
                                if !ts.is_empty() && file_latest.get(key).is_none_or(|(stamp, _)| ts >= stamp.as_str()) {
                                    file_latest.insert(key.to_string(), (ts.to_string(), LimitRow {
                                        agent: "Codex".into(),
                                        window: key.to_string(),
                                        used_percent: pct,
                                        window_minutes: w.get("window_minutes").and_then(|x| x.as_i64()).unwrap_or(0),
                                        resets_at: w.get("resets_at").and_then(|x| x.as_i64()).unwrap_or(0),
                                        plan: plan.clone(),
                                        source: "log".into(),
                                        detail: if key == "primary" {
                                            "5-hour session window".into()
                                        } else {
                                            "7-day window".into()
                                        },
                                        ..Default::default()
                                    }));
                                }
                            }
                        }
                    }
                }
                let Some(info) = p.get("info") else { return };
                let Some(last) = info.get("last_token_usage") else { return };
                let input = last.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                let cached = last.get("cached_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                let output = last.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                let reasoning = last.get("reasoning_output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                if input + output == 0 {
                    return;
                }
                // A forked rollout replays the parent's earlier turns, so the
                // same usage can show up twice. A pair that was already counted
                // in this file is the parent's, not the fork's own work.
                if !seen_usage.insert((ts.to_string(), input, output, cached)) {
                    return;
                }
                let uncached = input.saturating_sub(cached);
                let mut r = Row {
                    day: day_of(ts),
                    agent: "Codex".into(),
                    model: model.clone(),
                    requests: 1,
                    input: uncached,
                    cache_read: cached,
                    cache_write: 0,
                    output,
                    reasoning,
                    project: project.clone(),
                    billing: route.billing(),
                    ..Default::default()
                };
                if let Some(pr) = pricing.lookup(model_provider(&model), &model) {
                    r.cost = pr.cost(uncached, cached, 0, output);
                    r.cost_known = true;
                    r.savings = pr.savings(cached, 0);
                }
                if r.day.is_empty() { r.day = day.clone(); }
                add(rows, sess, "Codex", &model, &r.day.clone(), r);
            }
            _ => {}
        }
    });
    if !day.is_empty() {
        session(sess, "Codex", &day);
    }
    for (k, (ts, limit)) in file_latest {
        limits_out.push(CachedLimit { key: k, timestamp: ts, limit });
    }
}

fn parse_codex(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
    limits: &mut Vec<LimitRow>,
) -> u64 {
    let mut latest: HashMap<String, (String, LimitRow)> = HashMap::new();
    let route = codex_route(home);
    let mut files = list_files(&home.join(".codex").join("sessions"), "jsonl", None);
    files.extend(list_files(&home.join(".codex").join("archived_sessions"), "jsonl", None));
    if files.is_empty() {
        return 0;
    }

    let mut uncached = Vec::new();
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                for cl in &entry.limits {
                    if !cl.timestamp.is_empty() && latest.get(&cl.key).is_none_or(|(stamp, _)| cl.timestamp.as_str() >= stamp.as_str()) {
                        latest.insert(cl.key.clone(), (cl.timestamp.clone(), cl.limit.clone()));
                    }
                }
                continue;
            }
            uncached.push((file.clone(), path_str, mtime_ns, size));
        }
    }

    if !uncached.is_empty() {
        cache.dirty = true;
        let num_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
        let chunk_size = uncached.len().div_ceil(num_threads);
        let mut thread_results: Vec<Vec<(String, CachedFile)>> = (0..num_threads).map(|_| Vec::new()).collect();

        let pricing_ref = pricing;
        std::thread::scope(|s| {
            for (chunk, out) in uncached.chunks(chunk_size).zip(&mut thread_results) {
                s.spawn(move || {
                    for (file, path_str, mtime_ns, size) in chunk {
                        let mut file_rows = Rows::new();
                        let mut file_sess = Sess::new();
                        let mut file_limits = Vec::new();
                        parse_single_codex(file, pricing_ref, route, &mut file_rows, &mut file_sess, &mut file_limits);
                        let cached = CachedFile {
                            mtime_ns: *mtime_ns,
                            size: *size,
                            rows: file_rows.into_values().collect(),
                            sessions: file_sess.into_keys().collect(),
                            limits: file_limits,
                            ..Default::default()
                        };
                        out.push((path_str.clone(), cached));
                    }
                });
            }
        });

        for list in thread_results {
            for (path_str, entry) in list {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                for cl in &entry.limits {
                    if !cl.timestamp.is_empty() && latest.get(&cl.key).is_none_or(|(stamp, _)| cl.timestamp.as_str() >= stamp.as_str()) {
                        latest.insert(cl.key.clone(), (cl.timestamp.clone(), cl.limit.clone()));
                    }
                }
                cache.files.insert(path_str, entry);
            }
        }
    }

    limits.extend(latest.into_values().map(|(_, limit)| limit));
    limits.sort_by(|a, b| a.window.cmp(&b.window));
    files.len() as u64
}

// ---------------- Claude Code ----------------
// A Claude Code session lives in <project>/<session-id>.jsonl, and a delegated
// child run lives in <project>/<session-id>/subagents/agent-*.jsonl. The child's
// own transcript is the only place its usage appears, so counting every file
// once counts each run exactly once.
fn claude_subagent_file(file: &Path) -> bool {
    file.components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("subagents"))
}

/// The URL of a published artifact, when a tool call carries one.
fn artifact_url(input: &serde_json::Value) -> Option<String> {
    if let Some(url) = input.get("url").and_then(|x| x.as_str()) {
        if url.contains("claude.ai/code/artifact/") {
            return Some(url.to_string());
        }
    }
    // The Artifact tool publishes under the id it was given.
    if let Some(id) = input.get("id").and_then(|x| x.as_str()) {
        if !id.is_empty() {
            return Some(format!("https://claude.ai/code/artifact/{}", id));
        }
    }
    None
}

fn parse_single_claude(
    file: &Path,
    pricing: &Pricing,
    route: Route,
    rows: &mut Rows,
    sess: &mut Sess,
    artifacts: &mut Vec<Artifact>,
) {
    let mut day = String::new();
    let mut project = String::new();
    // The CLI writes one record per content block of a turn, and every record
    // carries the whole turn's usage. The API message id is the same in all of
    // them, so the usage is counted once per id: counting every record would
    // count a turn once per tool call it made. The CLI's own session total
    // (the cost-state line it writes) agrees with the once-per-id count.
    let mut seen_messages: HashSet<String> = HashSet::new();
    // A file under a subagents/ directory is a delegated child run. The
    // transcript also flags the chain with isSidechain.
    let mut subagent = claude_subagent_file(file);
    each_line(file, |line| {
        if !has_key(line, &CLAUDE_KEYS) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        if day.is_empty() && !day_of(ts).is_empty() {
            day = day_of(ts);
        }
        if project.is_empty() {
            project = project::label_for_cwd(v.get("cwd").and_then(|c| c.as_str()));
        }
        if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) {
            subagent = true;
        }
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            return;
        }
        let Some(msg) = v.get("message") else { return };
        let Some(u) = msg.get("usage") else { return };
        let input = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let output = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let cache_read = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let cache_write = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        // A 1-hour cache write is billed at twice the 5-minute rate, so the two
        // have to be kept apart to price them correctly.
        let cache_write_1h = u
            .get("cache_creation")
            .and_then(|c| c.get("ephemeral_1h_input_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            .min(cache_write);
        let reasoning = u
            .get("output_tokens_details")
            .and_then(|d| d.get("thinking_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        if input + output + cache_read + cache_write == 0 {
            return;
        }
        let mut tools = 0u64;
        if let Some(content) = msg.get("content").and_then(|x| x.as_array()) {
            for item in content {
                if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    tools += 1;
                    if item.get("name").and_then(|n| n.as_str()) == Some("Artifact") {
                        if let Some(url) = item.get("input").and_then(artifact_url) {
                            artifacts.push(Artifact {
                                agent: "Claude Code".into(),
                                url,
                                day: day_of(ts),
                                title: item
                                    .get("input")
                                    .and_then(|i| i.get("title"))
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            });
                        }
                    }
                }
            }
        }
        let model = pricing::normalize_model(msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown"));
        // The usage belongs to the turn, not to the block. Only the first
        // record of a message counts it; the tool calls in the other records
        // still count, because each block is written exactly once.
        let first = match msg.get("id").and_then(|x| x.as_str()) {
            Some(id) => seen_messages.insert(id.to_string()),
            None => true,
        };
        let (requests, input, cache_read, cache_write, cache_write_1h, output, reasoning) = if first {
            (1u64, input, cache_read, cache_write, cache_write_1h, output, reasoning)
        } else {
            (0, 0, 0, 0, 0, 0, 0)
        };
        // Claude Code writes down what each call cost. It is not used when
        // the catalogue has a price — the dashboard prices every token the
        // same way, so the figures add up — but it is the answer when the
        // catalogue does not know the model, which is better than no number
        // at all.
        let reported = v.get("costUSD").and_then(|x| x.as_f64()).filter(|c| *c > 0.0);
        let mut r = Row {
            day: day_of(ts),
            agent: "Claude Code".into(),
            model: model.clone(),
            requests,
            input,
            cache_read,
            cache_write,
            cache_write_1h,
            output,
            reasoning,
            tools,
            project: project.clone(),
            subagent,
            billing: route.billing(),
            cost_reported: reported,
            ..Default::default()
        };
        if let Some(pr) = pricing.lookup(model_provider(&model), &model) {
            r.cost = pr.cost_split(input, cache_read, cache_write, cache_write_1h, output);
            r.cost_known = true;
            r.savings = pr.savings_split(input, cache_read, cache_write, cache_write_1h);
        } else if let Some(reported) = reported {
            // The agent knows what it paid; that beats no price at all.
            r.cost = reported;
            r.cost_known = true;
            r.cost_source = CostSource::Reported;
        }
        if r.day.is_empty() { r.day = day.clone(); }
        add(rows, sess, "Claude Code", &model, &r.day.clone(), r);
    });
    if !day.is_empty() {
        session(sess, "Claude Code", &day);
    }
}

fn parse_claude(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
    artifacts: &mut Vec<Artifact>,
) -> u64 {
    let projects = home.join(".claude").join("projects");
    if !projects.exists() {
        return 0;
    }
    let files = list_files(&projects, "jsonl", None);
    if files.is_empty() {
        return 0;
    }

    let mut uncached = Vec::new();
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                // The artifacts were collected when the file was first read.
                artifacts.extend(entry.artifacts.iter().cloned());
                continue;
            }
            uncached.push((file.clone(), path_str, mtime_ns, size));
        }
    }

    if !uncached.is_empty() {
        cache.dirty = true;
        let pricing_ref = pricing;
        let route = claude_route(home);
        for (file, path_str, mtime_ns, size) in uncached {
            let mut file_rows = Rows::new();
            let mut file_sess = Sess::new();
            let mut file_artifacts = Vec::new();
            parse_single_claude(
                &file,
                pricing_ref,
                route,
                &mut file_rows,
                &mut file_sess,
                &mut file_artifacts,
            );
            let cached = CachedFile {
                mtime_ns,
                size,
                rows: file_rows.into_values().collect(),
                sessions: file_sess.into_keys().collect(),
                artifacts: file_artifacts.clone(),
                ..Default::default()
            };
            artifacts.extend(file_artifacts);
            for r in &cached.rows {
                add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
            }
            for (day, agent) in &cached.sessions {
                session(sess, agent, day);
            }
            cache.files.insert(path_str, cached);
        }
    }

    files.len() as u64
}

// ---------------- OpenCode ----------------
fn parse_opencode(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let db = home.join(".local").join("share").join("opencode").join("opencode.db");
    let Some((mtime_ns, size)) = file_stamp(&db) else {
        return 0;
    };
    // The database is written in write-ahead mode, so a message that was
    // written a moment ago may still be in the log beside it and not in the
    // file itself. The stamp of that log is part of the key, or the numbers
    // would sit still until the next checkpoint.
    let wal = db.with_extension("wal");
    let wal_stamp = file_stamp(&wal).unwrap_or((0, 0));
    if let Some(cached) = &cache.opencode_db {
        if cached.mtime_ns == mtime_ns && cached.size == size && cached.wal == wal_stamp {
            for r in &cached.rows {
                add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
            }
            for (day, agent) in &cached.sessions {
                session(sess, agent, day);
            }
            return cached.count;
        }
    }

    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return 0;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    let table = ["session_v2", "session"].iter().find(|t| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {}", t), [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)
            .unwrap_or(false)
    });
    let Some(table) = table else { return 0 };
    let mut count = 0;
    let mut db_rows = Rows::new();
    let mut db_sess = Sess::new();

    // The session table knows which directory a session ran in and whether it
    // was a delegated child run. Older schemas have one column but not the
    // other, so each is read on its own.
    let mut session_dirs: HashMap<String, String> = HashMap::new();
    let mut subagent_sessions: HashSet<String> = HashSet::new();
    if let Ok(mut stmt) = conn.prepare(&format!("SELECT id, directory FROM {}", table)) {
        if let Ok(it) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        }) {
            for (id, dir) in it.flatten() {
                session_dirs.insert(id, dir.unwrap_or_default());
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare(&format!("SELECT id FROM {} WHERE parent_id IS NOT NULL", table)) {
        if let Ok(it) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for id in it.flatten() {
                subagent_sessions.insert(id);
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(&format!("SELECT time_created FROM {}", table)) {
        if let Ok(it) = stmt.query_map([], |r| r.get::<_, i64>(0)) {
            for ts in it.flatten() {
                let day = unix_day(if ts > 1_000_000_000_000 { ts / 1000 } else { ts });
                session(&mut db_sess, "OpenCode", &day);
                count += 1;
            }
        }
    }
    // Session totals have no daily attribution. Messages keep the actual model,
    // request time, and usage even when a session is resumed with another model.
    //
    // The aggregation runs inside SQLite: pulling every message's JSON across
    // the wire and parsing it in Rust took twenty seconds on a database with a
    // few hundred thousand messages, while SQLite does the same sum in well
    // under a second. Messages that report a cost are kept apart from those
    // that do not, because the two are priced differently.
    // The current schema keeps the messages in `session_message`, with the
    // model and the content inside the message. The older one keeps them in
    // `message` and their parts in `part`. A database that has both holds
    // the old messages twice over, so only one of the two may be read.
    let current = conn
        .query_row("SELECT COUNT(*) FROM session_message", [], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
        .unwrap_or(false);
    let (message_table, provider_col, model_col, assistant) = if current {
        (
            "session_message",
            "json_extract(m.data,'$.model.providerID')",
            "json_extract(m.data,'$.model.id')",
            // A compaction is a model call of its own: the summary it wrote
            // cost tokens, and they are spent whether or not the messages it
            // replaced are still there.
            "m.type IN ('assistant','compaction')",
        )
    } else {
        (
            "message",
            "json_extract(m.data,'$.providerID')",
            "json_extract(m.data,'$.modelID')",
            "json_extract(m.data,'$.role') = 'assistant'",
        )
    };
    let day_expr = "date(CASE WHEN m.time_created > 1000000000000 THEN m.time_created/1000 ELSE m.time_created END,'unixepoch')";
    let columns = format!(
        "{provider_col}, {model_col},
                   COUNT(*),
                   SUM(COALESCE(json_extract(m.data,'$.tokens.input'),0)),
                   SUM(COALESCE(json_extract(m.data,'$.tokens.output'),0)),
                   SUM(COALESCE(json_extract(m.data,'$.tokens.cache.read'),0)),
                   SUM(COALESCE(json_extract(m.data,'$.tokens.cache.write'),0)),
                   SUM(COALESCE(json_extract(m.data,'$.tokens.reasoning'),0)),
                   SUM(COALESCE(json_extract(m.data,'$.cost'),0)),
                   json_extract(m.data,'$.cost') IS NOT NULL"
    );
    // One grouped row of assistant messages.
    struct MsgGroup {
        day: String,
        provider: Option<String>,
        model: Option<String>,
        requests: u64,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        reasoning: u64,
        cost: f64,
        has_cost: bool,
        directory: Option<String>,
        subagent: bool,
    }
    let mut grouped: Vec<MsgGroup> = Vec::new();
    // The session table carries the working directory, but an older schema does
    // not have that column, so the join is only used when it works.
    let candidates = [
        format!(
            "SELECT {day_expr} AS d, {columns}, s.directory, s.parent_id IS NOT NULL
             FROM {message_table} m LEFT JOIN {} s ON m.session_id = s.id
             WHERE json_valid(m.data) AND {assistant}
             GROUP BY d, 2, 3, 12, 13",
            table
        ),
        format!(
            "SELECT {day_expr} AS d, {columns}, NULL, NULL
             FROM {message_table} m
             WHERE json_valid(m.data) AND {assistant}
             GROUP BY d, 2, 3"
        ),
    ];
    for sql in candidates {
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let Ok(it) = stmt.query_map([], |r| {
            Ok(MsgGroup {
                day: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                provider: r.get::<_, Option<String>>(1)?,
                model: r.get::<_, Option<String>>(2)?,
                requests: r.get::<_, u64>(3)?,
                input: r.get::<_, u64>(4)?,
                output: r.get::<_, u64>(5)?,
                cache_read: r.get::<_, u64>(6)?,
                cache_write: r.get::<_, u64>(7)?,
                reasoning: r.get::<_, u64>(8)?,
                cost: r.get::<_, f64>(9)?,
                has_cost: r.get::<_, bool>(10)?,
                directory: r.get::<_, Option<String>>(11)?,
                subagent: r.get::<_, Option<bool>>(12)?.unwrap_or(false),
            })
        }) else { continue };
        grouped = it.flatten().collect();
        break;
    }
    if !grouped.is_empty() {
        for g in grouped {
            if g.day.is_empty() {
                continue;
            }
            let model = opencode_model(&serde_json::json!({"providerID": g.provider, "modelID": g.model}));
            let price = pricing.lookup(model_provider(&model), &model);
            let reported = if g.has_cost { Some(g.cost) } else { None };
            let mut r = Row {
                day: g.day.clone(),
                requests: g.requests,
                input: g.input,
                output: g.output,
                cache_read: g.cache_read,
                cache_write: g.cache_write,
                reasoning: g.reasoning,
                project: project::label_for_cwd(g.directory.as_deref()),
                subagent: g.subagent,
                billing: billing_of(reported, price.is_some(), false),
                ..Default::default()
            };
            if let Some(c) = reported {
                r.cost = c;
                r.cost_known = true;
            } else if let Some(p) = price {
                r.cost = p.cost(r.input, r.cache_read, r.cache_write, r.output);
                r.cost_known = true;
            }
            if let Some(p) = price {
                r.savings = p.savings(r.cache_read, r.cache_write);
            }
            add(&mut db_rows, &mut db_sess, "OpenCode", &model, &g.day, r);
        }
    } else if let Ok(mut stmt) = conn.prepare(&format!("SELECT data, time_created, session_id FROM {message_table}")) {
        // An older schema without the token fields: fall back to reading the
        // messages one by one.
        if let Ok(it) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        }) {
            for (data, ts, session_id) in it.flatten() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
                if current {
                    if v.get("type").and_then(|x| x.as_str()) != Some("assistant") {
                        continue;
                    }
                } else if v.get("role").and_then(|x| x.as_str()) != Some("assistant") {
                    continue;
                }
                let model = opencode_model(&v);
                let day = unix_day(if ts > 1_000_000_000_000 { ts / 1000 } else { ts });
                let u = &v["tokens"];
                let mut r = Row {
                    day: day.clone(),
                    requests: 1,
                    input: u["input"].as_u64().unwrap_or(0),
                    output: u["output"].as_u64().unwrap_or(0),
                    cache_read: u["cache"]["read"].as_u64().unwrap_or(0),
                    cache_write: u["cache"]["write"].as_u64().unwrap_or(0),
                    reasoning: u["reasoning"].as_u64().unwrap_or(0),
                    ..Default::default()
                };
                if let Some(sid) = &session_id {
                    r.project = session_dirs
                        .get(sid)
                        .map(|d| project::label_for_cwd(Some(d)))
                        .unwrap_or_default();
                    r.subagent = subagent_sessions.contains(sid);
                }
                let price = pricing.lookup(model_provider(&model), &model);
                let reported = v.get("cost").and_then(|x| x.as_f64()).filter(|c| *c >= 0.0);
                r.billing = billing_of(reported, price.is_some(), false);
                if let Some(cost) = reported {
                    r.cost = cost;
                    r.cost_known = true;
                } else if let Some(p) = price {
                    r.cost = p.cost(r.input, r.cache_read, r.cache_write, r.output);
                    r.cost_known = true;
                }
                if let Some(p) = price {
                    r.savings = p.savings(r.cache_read, r.cache_write);
                }
                add(&mut db_rows, &mut db_sess, "OpenCode", &model, &day, r);
            }
        }
    }
    // Group tools in SQLite instead of transferring each call's JSON. The
    // session table carries the working directory, but an older schema does
    // not have that column, so the join is only used when it works.
    //
    // The current schema keeps the content inside the message, so the tool
    // calls are counted from it; the older one keeps the parts in their own
    // table.
    let tools_sql = if current {
        [
            format!(
                "SELECT {provider_col}, {model_col},
                        date(CASE WHEN m.time_created > 1000000000000 THEN m.time_created/1000 ELSE m.time_created END,'unixepoch'), s.directory, COUNT(*)
                 FROM {message_table} m LEFT JOIN {} s ON m.session_id = s.id, json_each(m.data,'$.content')
                 WHERE json_valid(m.data) AND {assistant} AND json_extract(value,'$.type') = 'tool'
                 GROUP BY 1, 2, 3, 4",
                table
            ),
            format!(
                "SELECT {provider_col}, {model_col},
                        date(CASE WHEN m.time_created > 1000000000000 THEN m.time_created/1000 ELSE m.time_created END,'unixepoch'), NULL, COUNT(*)
                 FROM {message_table} m, json_each(m.data,'$.content')
                 WHERE json_valid(m.data) AND {assistant} AND json_extract(value,'$.type') = 'tool'
                 GROUP BY 1, 2, 3"
            ),
        ]
    } else {
        [
            format!(
                "SELECT json_extract(m.data, '$.providerID'), json_extract(m.data, '$.modelID'),
                        date(p.time_created/1000,'unixepoch'), s.directory, COUNT(*)
                 FROM part p JOIN message m ON p.message_id = m.id
                 LEFT JOIN {} s ON m.session_id = s.id
                 WHERE json_valid(p.data) AND json_valid(m.data) AND json_extract(p.data, '$.type') = 'tool'
                 GROUP BY 1, 2, 3, 4",
                table
            ),
            "SELECT json_extract(m.data, '$.providerID'), json_extract(m.data, '$.modelID'),
                        date(p.time_created/1000,'unixepoch'), NULL, COUNT(*)
                 FROM part p JOIN message m ON p.message_id = m.id
                 WHERE json_valid(p.data) AND json_valid(m.data) AND json_extract(p.data, '$.type') = 'tool'
                 GROUP BY 1, 2, 3".to_string(),
        ]
    };
    // One grouped row of tool calls: (provider, model, day, directory, calls).
    type ToolRow = (Option<String>, Option<String>, Option<String>, Option<String>, u64);
    let mut tools_by_row: Vec<ToolRow> = Vec::new();
    for sql in tools_sql.iter() {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(it) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?,
                r.get::<_, u64>(4)?,
            ))
        }) else { continue };
        tools_by_row = it.flatten().collect();
        break;
    }
    for (provider, model, day, directory, tools) in tools_by_row {
        let Some(day) = day else { continue };
        let model = opencode_model(&serde_json::json!({"providerID": provider, "modelID": model}));
        add(&mut db_rows, &mut db_sess, "OpenCode", &model, &day, Row {
            tools,
            project: project::label_for_cwd(directory.as_deref()),
            ..Default::default()
        });
    }

    for r in db_rows.values() {
        add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
    }
    for (day, agent) in db_sess.keys() {
        session(sess, agent, day);
    }

    cache.opencode_db = Some(CachedDb {
        mtime_ns,
        size,
        wal: wal_stamp,
        count,
        rows: db_rows.into_values().collect(),
        sessions: db_sess.into_keys().collect(),
    });
    cache.dirty = true;
    count
}

/// The model a message ran on.
///
/// The current schema keeps it in an object, with the provider and the id
/// in it; the older one had the two fields at the top of the message.
fn opencode_model(message: &serde_json::Value) -> String {
    let model = message.get("model").filter(|m| m.is_object());
    let id = model
        .and_then(|m| m.get("id"))
        .and_then(|x| x.as_str())
        .or_else(|| message["modelID"].as_str())
        .unwrap_or("unknown");
    let provider = model
        .and_then(|m| m.get("providerID"))
        .and_then(|x| x.as_str())
        .or_else(|| message["providerID"].as_str())
        .filter(|p| !p.is_empty());
    match provider {
        Some(provider) => pricing::normalize_model(&format!("{}/{}", provider, id)),
        None => pricing::normalize_model(id),
    }
}

// ---------------- Antigravity ----------------
fn parse_antigravity(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let mut dbs = 0u64;
    for variant in ["antigravity", "antigravity-cli"] {
        let conv = home.join(".gemini").join(variant).join("conversations");
        dbs += list_files(&conv, "db", None).len() as u64;
        let brain = home.join(".gemini").join(variant).join("brain");
        let transcripts = list_files(&brain, "jsonl", Some("transcript.jsonl"));
        for file in &transcripts {
            let path_str = file.to_string_lossy().to_string();
            if let Some((mtime_ns, size)) = file_stamp(file) {
                if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                    for r in &entry.rows {
                        add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                    }
                    for (day, agent) in &entry.sessions {
                        session(sess, agent, day);
                    }
                    continue;
                }
            }
            let mtime_day = fs::metadata(file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| unix_day(d.as_secs() as i64))
                .unwrap_or_default();
            let mut file_rows = Rows::new();
            let mut file_sess = Sess::new();
            session(&mut file_sess, "Antigravity", &mtime_day);
            let mut model = String::from("unknown");
            // Transcript tails sometimes repeat the final steps; dedupe by step
            // index so a re-appended tail does not double-count requests.
            let mut seen_steps = std::collections::HashSet::new();
            each_line(file, |line| {
                if !has_key(line, &AGY_KEYS) {
                    return;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
                if let Some(content) = v.get("content").and_then(|x| x.as_str()) {
                    if let Some(idx) = content.rfind("Model Selection` from") {
                        let rest = &content[idx..];
                        if let Some(to) = rest.find(" to ") {
                            let tail = &rest[to + 4..];
                            // The model name ends at the sentence boundary, not the
                            // first dot: "Claude Opus 4.6 (Thinking). ..." must not
                            // truncate to "Claude Opus 4".
                            let end = tail
                                .find(". ")
                                .map(|i| i + 1)
                                .or_else(|| tail.strip_suffix('.').map(|s| s.len()))
                                .unwrap_or_else(|| {
                                    tail.find('\n').unwrap_or(tail.len())
                                });
                            let m = tail[..end].trim().trim_end_matches('.').trim();
                            if !m.is_empty() && m != "None" {
                                model = pricing::normalize_model(m);
                            }
                        }
                    }
                }
                if v.get("type").and_then(|t| t.as_str()) == Some("PLANNER_RESPONSE") {
                    if let Some(si) = v.get("step_index").and_then(|x| x.as_i64()) {
                        if !seen_steps.insert(si) {
                            return;
                        }
                    }
                    let tools = v.get("tool_calls").and_then(|t| t.as_array()).map(|a| a.len() as u64).unwrap_or(0);
                    // Newer (CLI) transcripts report per-response tokens; older IDE
                    // ones carry no token fields anywhere, so these stay zero there.
                    let input = v.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let cache_read = v.get("cache_read_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let output = v.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let ts = v.get("created_at").and_then(|t| t.as_str()).unwrap_or("");
                    let mut r = Row {
                        day: day_of(ts),
                        agent: "Antigravity".into(),
                        billing: Billing::Subscription,
                        model: model.clone(),
                        requests: 1,
                        input,
                        cache_read,
                        output,
                        tools,
                        ..Default::default()
                    };
                    if input + output + cache_read > 0 {
                        if let Some(pr) = pricing.lookup(model_provider(&model), &model) {
                            r.cost = pr.cost(input, cache_read, 0, output);
                            r.cost_known = true;
                            r.savings = pr.savings(cache_read, 0);
                        }
                    }
                    if r.day.is_empty() { r.day = mtime_day.clone(); }
                    add(&mut file_rows, &mut file_sess, "Antigravity", &model, &r.day.clone(), r);
                }
            });
            for r in file_rows.values() {
                add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
            }
            for (day, agent) in file_sess.keys() {
                session(sess, agent, day);
            }
            if let Some((mtime_ns, size)) = file_stamp(file) {
                cache.files.insert(path_str, CachedFile {
                    mtime_ns,
                    size,
                    rows: file_rows.into_values().collect(),
                    sessions: file_sess.into_keys().collect(),
                    ..Default::default()
                });
                cache.dirty = true;
            }
        }
        // conversation dbs without transcripts still count as sessions (dated by file mtime)
        if transcripts.is_empty() {
            for db in list_files(&conv, "db", None) {
                let day = fs::metadata(&db)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| unix_day(d.as_secs() as i64))
                    .unwrap_or_default();
                session(sess, "Antigravity", &day);
            }
        }
    }
    dbs
}

// ---------------- T3 (Antigravity backend) ----------------
// T3 drives Antigravity conversations of its own under
// ~/.t3/userdata/providers/antigravity/*/antigravity-acp/. Same engine, same
// transcript schema, so these rows count as Antigravity. T3 never switches
// models mid-transcript, so the model is the dominant one in the sibling
// conversation db's gen_metadata.
fn t3_conv_db(file: &Path) -> Option<PathBuf> {
    // .../antigravity-acp/brain/<conv>/.system_generated/logs/transcript.jsonl
    let conv = file.parent()?.parent()?.parent()?;
    let brain = conv.parent()?;
    let acp = brain.parent()?;
    if brain.file_name().and_then(|f| f.to_str()) != Some("brain") {
        return None;
    }
    let id = conv.file_name()?.to_str()?;
    Some(acp.join("conversations").join(format!("{}.db", id)))
}

/// Dominant model id across a conversation db's gen_metadata blobs, e.g.
/// "gemini-3.8-flash-high". Requires a vendor hint plus a digit so prompt
/// boilerplate ("disable-teamwork-forced-flash-model") never matches; the
/// most frequent candidate wins so one-off binary artifacts glued to a real
/// id ("highh") can never beat the id itself. One conversation uses one
/// model, so per-response mapping is unnecessary.
fn agy_db_model(db: &Path) -> String {
    let Ok(conn) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return "unknown".into();
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    let Ok(mut stmt) = conn.prepare("SELECT data FROM gen_metadata ORDER BY idx") else {
        return "unknown".into();
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)) else {
        return "unknown".into();
    };
    let mut votes: HashMap<String, u64> = HashMap::new();
    for blob in rows.flatten() {
        let mut cur = Vec::new();
        let mut consider = |cur: &[u8]| {
            if cur.len() >= 8 {
                if let Ok(s) = std::str::from_utf8(cur) {
                    let t = s.trim_matches(|c| c == '.' || c == '-' || c == '_');
                    let l = t.to_lowercase();
                    let vendor = l.contains("gemini") || l.contains("claude") || l.contains("gpt") || l.contains("opus") || l.contains("sonnet");
                    if vendor && l.bytes().any(|b| b.is_ascii_digit()) {
                        *votes.entry(t.to_string()).or_insert(0) += 1;
                    }
                }
            }
        };
        for &b in &blob {
            if b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' {
                cur.push(b);
            } else {
                consider(&cur);
                cur.clear();
            }
        }
        consider(&cur);
    }
    votes.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(a.0.len().cmp(&b.0.len()))).map(|(m, _)| m).unwrap_or_else(|| "unknown".into())
}

fn parse_t3(
    home: &Path,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
    counted_threads: &mut HashSet<String>,
) -> u64 {
    let prov = home.join(".t3").join("userdata").join("providers").join("antigravity");
    let Ok(top) = fs::read_dir(&prov) else { return 0 };
    let mut files = Vec::new();
    for e in top.flatten() {
        let brain = e.path().join("antigravity-acp").join("brain");
        files.extend(list_files(&brain, "jsonl", Some("transcript.jsonl")));
    }
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                if let Some(id) = &entry.thread_id {
                    counted_threads.insert(id.clone());
                }
                continue;
            }
        }
        let model = pricing::normalize_model(&t3_conv_db(file).map(|db| agy_db_model(&db)).unwrap_or_else(|| "unknown".into()));
        let mtime_day = fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| unix_day(d.as_secs() as i64))
            .unwrap_or_default();
        let mut session_day: Option<String> = None;
        let mut seen_steps = HashSet::new();
        let mut file_rows = Rows::new();
        let mut file_sess = Sess::new();
        each_line(file, |line| {
            if !has_key(line, &AGY_KEYS) {
                return;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
            if v.get("type").and_then(|t| t.as_str()) != Some("PLANNER_RESPONSE") {
                return;
            }
            if let Some(si) = v.get("step_index").and_then(|x| x.as_i64()) {
                if !seen_steps.insert(si) {
                    return;
                }
            }
            // T3 transcripts carry no token fields; requests + tools only.
            let tools = v.get("tool_calls").and_then(|t| t.as_array()).map(|a| a.len() as u64).unwrap_or(0);
            let ts = v.get("created_at").and_then(|t| t.as_str()).unwrap_or("");
            let mut r = Row {
                day: day_of(ts),
                agent: "Antigravity".into(),
                billing: Billing::Subscription,
                model: model.clone(),
                requests: 1,
                tools,
                ..Default::default()
            };
            if r.day.is_empty() { r.day = mtime_day.clone(); }
            if session_day.as_ref().is_none_or(|day| r.day < *day) {
                session_day = Some(r.day.clone());
            }
            add(&mut file_rows, &mut file_sess, "Antigravity", &model, &r.day.clone(), r);
        });
        let mut thread_id = None;
        if let Some(day) = session_day {
            session(&mut file_sess, "Antigravity", &day);
            if let Some(db) = t3_conv_db(file) {
                if let Some(id) = db.file_stem().and_then(|s| s.to_str()) {
                    counted_threads.insert(id.to_string());
                    thread_id = Some(id.to_string());
                }
            }
        }
        for r in file_rows.values() {
            add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
        }
        for (day, agent) in file_sess.keys() {
            session(sess, agent, day);
        }
        if let Some((mtime_ns, size)) = file_stamp(file) {
            cache.files.insert(path_str, CachedFile {
                mtime_ns,
                size,
                rows: file_rows.into_values().collect(),
                sessions: file_sess.into_keys().collect(),
                thread_id,
                ..Default::default()
            });
            cache.dirty = true;
        }
    }
    // Missing or empty transcripts fall back to the conversation DB.
    let with_transcript = counted_threads.clone();
    let mut dbs = files.len() as u64;
    // (re-read: the first directory listing was already consumed above)
    let Ok(top2) = fs::read_dir(&prov) else { return dbs };
    for e in top2.flatten() {
        let conv = e.path().join("antigravity-acp").join("conversations");
        for db in list_files(&conv, "db", None) {
            let id = db.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
            if with_transcript.contains(&id) {
                continue;
            }
            dbs += 1;
            let model = pricing::normalize_model(&agy_db_model(&db));
            let mtime_day = fs::metadata(&db)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| unix_day(d.as_secs() as i64))
                .unwrap_or_default();
            let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { continue };
            let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
            // Assistant responses carry the day + model order; tool calls live
            // in their own steps (types 21/25/38/103: one call_* id each).
            let Ok(mut stmt) = conn.prepare("SELECT step_type, metadata FROM steps ORDER BY idx") else { continue };
            let Ok(steps) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))) else { continue };
            let mut day_model: Vec<(String, String)> = Vec::new();
            let mut tools_per_day: HashMap<String, u64> = HashMap::new();
            for step in steps.flatten() {
                let (st, meta) = step;
                let mut d = agy_step_day(&meta);
                if d.is_empty() { d = mtime_day.clone(); }
                if st == 15 {
                    day_model.push((d, model.clone()));
                } else if st == 21 || st == 25 || st == 38 || st == 103 {
                    *tools_per_day.entry(d).or_insert(0) += 1;
                }
            }
            if let Some((day, _)) = day_model.iter().min_by(|a, b| a.0.cmp(&b.0)) {
                session(sess, "Antigravity", day);
                counted_threads.insert(id);
            }
            for (d, model) in &day_model {
                add(rows, sess, "Antigravity", model, d, Row {
                    day: d.clone(),
                    agent: "Antigravity".into(),
                    billing: Billing::Subscription,
                    model: model.clone(),
                    requests: 1,
                    ..Default::default()
                });
            }
            for (d, n) in &tools_per_day {
                let model = day_model.iter().find(|(dd, _)| dd == d).map(|(_, m)| m.clone()).unwrap_or_else(|| "unknown".into());
                add(rows, sess, "Antigravity", &model, d, Row {
                    day: d.clone(),
                    agent: "Antigravity".into(),
                    billing: Billing::Subscription,
                    model: model.clone(),
                    tools: *n,
                    ..Default::default()
                });
            }
        }
    }
    dbs
}

// T3 keeps exact per-turn token usage in its local orchestration database.
// The Antigravity route is recorded as a model id (antigravity/<model>), even
// when the provider adapter is Pi. Public API prices are used only for Cinder's
// API estimate; Antigravity's subscription charge is not exposed here.
fn parse_t3_usage(home: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess, counted_threads: &HashSet<String>) -> bool {
    let db = home.join(".t3").join("userdata").join("statev2.sqlite");
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    // Model selection is per run; a provider session can be reused after a model change.
    let Ok(mut stmt) = conn.prepare(
        "SELECT t.provider_thread_id, t.started_at, t.payload_json, r.payload_json
         FROM orchestration_v2_projection_provider_turns t
         JOIN orchestration_v2_projection_run_attempts a ON a.attempt_id = t.run_attempt_id
         JOIN orchestration_v2_projection_runs r ON r.run_id = a.run_id",
    ) else {
        return false;
    };
    let Ok(turns) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    }) else {
        return false;
    };

    let mut thread_days: HashMap<String, String> = HashMap::new();
    for turn in turns.flatten() {
        let (thread_id, started_at, payload, run_payload) = turn;
        let Ok(run) = serde_json::from_str::<serde_json::Value>(&run_payload) else { continue };
        let Some(raw_model) = run
            .get("modelSelection")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
        else {
            continue;
        };
        let model = pricing::normalize_model(raw_model);
        if !model.to_lowercase().starts_with("antigravity/") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else { continue };
        let Some(usage) = v.get("tokenUsage") else { continue };
        let input = usage.get("inputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let cache_read = usage.get("cachedInputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let output = usage.get("outputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let reasoning = usage.get("reasoningOutputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
        if input + cache_read + output == 0 {
            continue;
        }

        let day = day_of(&started_at);
        let day = if day.is_empty() {
            usage
                .get("updatedAt")
                .and_then(|x| x.as_str())
                .map(day_of)
                .unwrap_or_default()
        } else {
            day
        };
        let counted = counted_threads.contains(&thread_id);
        if !counted && !day.is_empty() {
            thread_days
                .entry(thread_id)
                .and_modify(|first| {
                    if day.as_str() < first.as_str() {
                        *first = day.clone();
                    }
                })
                .or_insert_with(|| day.clone());
        }
        let mut r = Row {
            day: day.clone(),
            agent: "Antigravity".into(),
            billing: Billing::Subscription,
            model: model.clone(),
            requests: if counted { 0 } else { 1 },
            input,
            cache_read,
            output,
            reasoning,
            ..Default::default()
        };
        if let Some(pr) = pricing.lookup(None, &model) {
            r.cost = pr.cost(input, cache_read, 0, output);
            r.cost_known = true;
            r.savings = pr.savings(cache_read, 0);
        }
        add(rows, sess, "Antigravity", &model, &day, r);
    }
    for day in thread_days.values() {
        session(sess, "Antigravity", day);
    }
    true
}

/// Step timestamp: metadata is a protobuf whose field 1 holds an inner message
/// with the unix-seconds timestamp as its field 1 varint.
fn agy_step_day(meta: &[u8]) -> String {
    let varint = |pos: &mut usize| -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        while *pos < meta.len() {
            let b = meta[*pos];
            *pos += 1;
            v |= ((b & 0x7F) as u64) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return Some(v);
            }
            if shift > 70 {
                return None;
            }
        }
        None
    };
    if meta.first() == Some(&0x0A) {
        let mut pos = 1;
        let len = varint(&mut pos).unwrap_or(0) as usize;
        let end = pos + len;
        if end <= meta.len() && meta.get(pos) == Some(&0x08) {
            pos += 1;
            if let Some(ts) = varint(&mut pos) {
                if (1_700_000_000..2_000_000_000).contains(&ts) {
                    return unix_day(ts as i64);
                }
            }
        }
    }
    String::new()
}

// ---------------- DSH ----------------
// ~/.dsh/sessions/**/session.v4.jsonl.zstd: zstd-compressed JSONL, one object
// per line. assistant/message lines are the requests (content items of type
// "tool-call" are the tool calls) and carry data.usage with Pi-style token
// counts: inputTokens excludes cacheReadTokens, total = the parts summed.
fn ms_day(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    unix_day(ms / 1000)
}

fn parse_single_dsh(file: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) {
    let Ok(f) = fs::File::open(file) else { return };
    let Ok(bytes) = zstd::stream::decode_all(f) else { return };
    let text = String::from_utf8_lossy(&bytes);
    let mut model = String::from("unknown");
    let mut day = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                if let Some(ms) = v.get("createdAt").and_then(|x| x.as_i64()) {
                    day = ms_day(ms);
                }
                session(sess, "DSH", &day);
            }
            Some("model/selection") => {
                if let Some(d) = v.get("data") {
                    let p = d.get("provider").and_then(|x| x.as_str()).unwrap_or("");
                    let m = d.get("model").and_then(|x| x.as_str()).unwrap_or("");
                    if !m.is_empty() {
                        let full = if p.is_empty() { m.to_string() } else { format!("{}/{}", p, m) };
                        model = pricing::normalize_model(&full);
                    }
                }
            }
            Some("assistant/message") => {
                let ts = v.get("time").and_then(|x| x.as_i64()).unwrap_or(0);
                let mut tools = 0u64;
                let mut m = model.clone();
                let mut input = 0u64;
                let mut output = 0u64;
                let mut cache_read = 0u64;
                let mut cache_write = 0u64;
                let mut reasoning = 0u64;
                if let Some(d) = v.get("data") {
                    if let Some(content) = d.get("message").and_then(|x| x.get("content")).and_then(|x| x.as_array()) {
                        for item in content {
                            if item.get("type").and_then(|t| t.as_str()) == Some("tool-call") {
                                tools += 1;
                            }
                        }
                    }
                    if let Some(src) = d.get("message").and_then(|x| x.get("source")) {
                        let p = src.get("provider").and_then(|x| x.as_str()).unwrap_or("");
                        let mm = src.get("model").and_then(|x| x.as_str()).unwrap_or("");
                        if !mm.is_empty() {
                            let full = if p.is_empty() { mm.to_string() } else { format!("{}/{}", p, mm) };
                            m = pricing::normalize_model(&full);
                        }
                    }
                    if let Some(u) = d.get("usage") {
                        input = u.get("inputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        output = u.get("outputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        cache_read = u.get("cacheReadTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        cache_write = u.get("cacheWriteTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        reasoning = u.get("reasoningTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    }
                }
                let d = ms_day(ts);
                let d = if d.is_empty() { day.clone() } else { d };
                let mut r = Row {
                    day: d.clone(),
                    agent: "DSH".into(),
                    model: m.clone(),
                    requests: 1,
                    input,
                    cache_read,
                    cache_write,
                    output,
                    reasoning,
                    tools,
                    billing: Billing::Api,
                    ..Default::default()
                };
                if input + output + cache_read + cache_write > 0 {
                    if let Some(pr) = pricing.lookup(model_provider(&m), &m) {
                        r.cost = pr.cost(input, cache_read, cache_write, output);
                        r.cost_known = true;
                        r.savings = pr.savings(cache_read, cache_write);
                    }
                }
                add(rows, sess, "DSH", &m, &d, r);
            }
            _ => {}
        }
    }
}

fn parse_dsh(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let root = home.join(".dsh").join("sessions");
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().and_then(|f| f.to_str()).map(|f| f.ends_with(".jsonl.zstd")).unwrap_or(false) {
                    out.push(p);
                }
            }
        }
    }
    walk(&root, &mut files);
    if files.is_empty() {
        return 0;
    }

    let mut uncached = Vec::new();
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                continue;
            }
            uncached.push((file.clone(), path_str, mtime_ns, size));
        }
    }

    if !uncached.is_empty() {
        cache.dirty = true;
        let pricing_ref = pricing;
        for (file, path_str, mtime_ns, size) in uncached {
            let mut file_rows = Rows::new();
            let mut file_sess = Sess::new();
            parse_single_dsh(&file, pricing_ref, &mut file_rows, &mut file_sess);
            let cached = CachedFile {
                mtime_ns,
                size,
                rows: file_rows.into_values().collect(),
                sessions: file_sess.into_keys().collect(),
                ..Default::default()
            };
            for r in &cached.rows {
                add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
            }
            for (day, agent) in &cached.sessions {
                session(sess, agent, day);
            }
            cache.files.insert(path_str, cached);
        }
    }

    files.len() as u64
}

// ---------------- LM Studio ----------------
// ~/.lmstudio/conversations/*.conversation.json gives requests + sessions +
// the model (gguf stem from indexedModelIdentifier). tokenCount there is the
// live context size at save time, NOT cumulative usage, so it is ignored.
// Real per-prediction tokens come from ~/.lmstudio/server-logs/**/*.log:
//   prompt eval time = ... / N tokens   -> input
//         eval time = ... / N tokens   -> output
// Only the first line of each burst carries a [date] prefix; the rest
// inherit the day, and predictions inherit the last loaded model.
// Local inference has no price: tokens are recorded, cost stays unknown.
fn lms_stem(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let base = base.strip_suffix(".gguf").or_else(|| base.strip_suffix(".GGUF")).unwrap_or(base);
    pricing::normalize_model(base)
}

/// (is_prompt, n) for a slot print_timing line, else None.
fn lms_tokens(line: &str) -> Option<(bool, u64)> {
    let prompt = line.contains("prompt eval time");
    if !prompt && !line.contains("eval time") {
        return None;
    }
    let mi = line.find("eval time")? + "eval time".len();
    let rest = &line[mi..];
    let slash = rest.find('/')?;
    let num: String = rest[slash + 1..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((prompt, num.parse().ok()?))
}

fn parse_lmstudio(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let dir = home.join(".lmstudio").join("conversations");
    let files: Vec<PathBuf> = list_files(&dir, "json", None)
        .into_iter()
        .filter(|p| p.file_name().and_then(|f| f.to_str()).map(|f| f.ends_with(".conversation.json")).unwrap_or(false))
        .collect();
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                continue;
            }
        }
        let Ok(text) = fs::read_to_string(file) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let day = v.get("createdAt").and_then(|x| x.as_i64()).map(ms_day).unwrap_or_default();
        let model = v.get("lastUsedModel").and_then(|m| m.get("indexedModelIdentifier")).and_then(|x| x.as_str()).map(lms_stem).unwrap_or_else(|| {
            v.get("lastUsedModel").and_then(|m| m.get("identifier")).and_then(|x| x.as_str()).map(pricing::normalize_model).unwrap_or_else(|| "unknown".into())
        });
        let mut requests = 0u64;
        if let Some(msgs) = v.get("messages").and_then(|x| x.as_array()) {
            for m in msgs {
                let sel = m.get("currentlySelected").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                if let Some(ver) = m.get("versions").and_then(|x| x.as_array()).and_then(|a| a.get(sel)) {
                    if ver.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        requests += 1;
                    }
                }
            }
        }
        let mut file_rows = Rows::new();
        let mut file_sess = Sess::new();
        session(&mut file_sess, "LM Studio", &day);
        if requests > 0 {
            add(&mut file_rows, &mut file_sess, "LM Studio", &model, &day, Row {
                day: day.clone(),
                agent: "LM Studio".into(),
                model: model.clone(),
                requests,
                billing: Billing::Local,
                ..Default::default()
            });
        }
        for r in file_rows.values() {
            add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
        }
        for (day, agent) in file_sess.keys() {
            session(sess, agent, day);
        }
        if let Some((mtime_ns, size)) = file_stamp(file) {
            cache.files.insert(path_str, CachedFile {
                mtime_ns,
                size,
                rows: file_rows.into_values().collect(),
                sessions: file_sess.into_keys().collect(),
                ..Default::default()
            });
            cache.dirty = true;
        }
    }
    let mut logs = list_files(&home.join(".lmstudio").join("server-logs"), "log", None);
    // Chronological order, one shared state: a rotated log continues the
    // previous file's tasks without repeating the load_model line.
    logs.sort();
    let mut day = String::new();
    let mut model = String::from("unknown");
    // What the same tokens would have cost on a cloud provider: running the
    // model locally is what saved the money.
    let cloud = pricing.reference_cloud();
    for file in &logs {
        each_line(file, |line| {
            if line.starts_with('[') && line.len() > 11 && line.as_bytes().get(11) == Some(&b' ') {
                let d = day_of(&line[1..]);
                if d.len() == 10 {
                    day = d;
                }
            }
            if line.contains("load_model") && line.contains(".gguf") {
                if let Some(i) = line.find("loading model") {
                    let rest = &line[i + "loading model".len()..];
                    let path = rest.trim().trim_matches('\'').trim_matches('"');
                    let stem = lms_stem(path.split_whitespace().next().unwrap_or(path));
                    if stem != "unknown" {
                        model = stem;
                    }
                }
            }
            if let Some((is_prompt, n)) = lms_tokens(line) {
                if day.is_empty() || model == "unknown" || n == 0 {
                    return;
                }
                let mut r = Row {
                    day: day.clone(),
                    agent: "LM Studio".into(),
                    model: model.clone(),
                    billing: Billing::Local,
                    ..Default::default()
                };
                if is_prompt {
                    r.input = n;
                } else {
                    r.output = n;
                }
                if let Some(p) = cloud {
                    r.cloud_savings = p.cost(r.input, r.cache_read, r.cache_write, r.output);
                }
                add(rows, sess, "LM Studio", &model, &day, r);
            }
        });
    }
    (files.len() + logs.len()) as u64
}

pub(crate) fn unix_day(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs / 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Collect the stats. Runs off the UI thread and reports the stage it is on, so a
/// cold start stays responsive (and legible) while it walks the local logs.
#[tauri::command(async)]
fn get_stats(app: AppHandle) -> Stats {
    scan(&|stage| {
        let _ = app.emit("stats-stage", stage);
    })
}

#[tauri::command(async)]
fn rescan_stats(app: AppHandle) -> Stats {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    cache::ScanCache::clear(&home);
    scan(&|stage| {
        let _ = app.emit("stats-stage", stage);
    })
}

fn scan(progress: &dyn Fn(&str)) -> Stats {
    // Serialise concurrent scans (manual refresh + background watcher): each one
    // walks ~5 GB, so overlapping runs just thrash the disk for the same result.
    static SCAN_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
    let _guard = SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    scan_home(&home, progress)
}

fn scan_home(home: &Path, progress: &dyn Fn(&str)) -> Stats {
    // The live quota snapshot is read from the background refresher, so a scan
    // never waits on the network.
    let (reports, quota_checked_at, _, _) = quota::snapshot();
    scan_home_with(home, progress, reports, quota_checked_at)
}

fn scan_home_with(
    home: &Path,
    progress: &dyn Fn(&str),
    reports: Vec<quota::QuotaReport>,
    quota_checked_at: String,
) -> Stats {
    progress("model prices");
    let pricing = Pricing::load(home);
    let pricing_fp = pricing.fingerprint;
    let mut cache = ScanCache::load_for(home, pricing_fp);

    let mut rows = Rows::new();
    let mut sess = Sess::new();
    let mut limits = Vec::new();
    let mut counted_threads = HashSet::new();
    let mut plan_routes: HashMap<String, u64> = HashMap::new();
    let mut artifacts: Vec<Artifact> = Vec::new();
    // (agent, path, files, milliseconds) for the sources panel.
    let mut sources: Vec<Source> = Vec::new();

    progress("Pi logs");
    scan_source(&mut sources, "Pi", "~/.pi/agent/sessions", || {
        parse_pi(home, &pricing, &mut cache, &mut rows, &mut sess, &mut plan_routes)
    });

    progress("Claude Code logs");
    scan_source(&mut sources, "Claude Code", "~/.claude/projects", || {
        parse_claude(home, &pricing, &mut cache, &mut rows, &mut sess, &mut artifacts)
    });

    progress("OpenCode database");
    scan_source(&mut sources, "OpenCode", "~/.local/share/opencode/opencode.db", || {
        parse_opencode(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    progress("Antigravity logs");
    scan_source(
        &mut sources,
        "Antigravity",
        "~/.gemini/antigravity* + ~/.t3/userdata/statev2.sqlite + providers/antigravity/*/antigravity-acp",
        || parse_antigravity(home, &pricing, &mut cache, &mut rows, &mut sess),
    );

    progress("T3 logs");
    scan_source(&mut sources, "T3", "~/.t3/userdata/providers/antigravity", || {
        parse_t3(home, &mut cache, &mut rows, &mut sess, &mut counted_threads)
    });

    progress("T3 usage database");
    scan_source(&mut sources, "T3 usage", "~/.t3/userdata/statev2.sqlite", || {
        if parse_t3_usage(home, &pricing, &mut rows, &mut sess, &counted_threads) { 1 } else { 0 }
    });

    progress("DSH logs");
    scan_source(&mut sources, "DSH", "~/.dsh/sessions", || {
        parse_dsh(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    progress("LM Studio logs");
    scan_source(&mut sources, "LM Studio", "~/.lmstudio/conversations + server-logs", || {
        parse_lmstudio(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    progress("Codex logs");
    scan_source(&mut sources, "Codex", "~/.codex/sessions + archived_sessions", || {
        parse_codex(home, &pricing, &mut cache, &mut rows, &mut sess, &mut limits)
    });

    // The agents Cinder learned to read later.
    progress("Cursor logs");
    scan_source(&mut sources, "Cursor", "~/.cursor/chats + cli-chats", || {
        agents::parse_cursor(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    progress("GitHub Copilot logs");
    scan_source(&mut sources, "GitHub Copilot", "~/.copilot/history-session-state + logs", || {
        agents::parse_copilot(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    progress("Gemini CLI logs");
    scan_source(&mut sources, "Gemini CLI", "~/.gemini/tmp + sessions", || {
        agents::parse_gemini(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    progress("Cline logs");
    scan_source(&mut sources, "Cline", "VS Code globalStorage/saoudrizwan.claude-dev", || {
        agents::parse_cline(home, &pricing, &mut cache, &mut rows, &mut sess)
    });

    cache.save();
    progress("done");

    // The live quota snapshot was handed in, so a scan never waits on the
    // network.
    let mut quota_error = String::new();
    let mut live_windows: Vec<quota::LiveQuota> = Vec::new();
    for report in reports {
        if report.skipped {
            continue;
        }
        if !report.error.is_empty() {
            if !quota_error.is_empty() {
                quota_error.push_str("; ");
            }
            quota_error.push_str(&format!("{}: {}", report.agent, report.error));
            continue;
        }
        for w in report.windows {
            limits.push(LimitRow {
                agent: w.agent.clone(),
                window: w.window.clone(),
                used_percent: w.used_percent,
                window_minutes: w.window_minutes,
                resets_at: w.resets_at,
                plan: w.plan.clone(),
                // A window that is not fresh is marked as such: it is the last
                // number that came back, not the current state.
                source: if report.stale { "stale".into() } else { "live".into() },
                detail: w.detail.clone(),
                used_amount: w.used_amount,
                limit_amount: w.limit_amount,
            });
            live_windows.push(w);
        }
    }

    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|a, b| a.day.cmp(&b.day).then(a.agent.cmp(&b.agent)).then(a.model.cmp(&b.model)).then(a.project.cmp(&b.project)));
    let mut sessions: Vec<SessionRow> = sess
        .into_iter()
        .map(|((day, agent), n)| SessionRow { day, agent, sessions: n })
        .collect();
    sessions.sort_by(|a, b| a.day.cmp(&b.day));

    let mut unpriced: HashMap<String, u64> = HashMap::new();
    for r in rows.iter().filter(|r| r.cost_partial && !r.model.is_empty()) {
        *unpriced.entry(r.model.clone()).or_default() += r.tokens();
    }
    let mut unpriced: Vec<_> = unpriced.into_iter().collect();
    unpriced.sort_by_key(|a| std::cmp::Reverse(a.1));

    // The same artifact can be published by several turns; keep one of each.
    artifacts.sort_by(|a, b| a.url.cmp(&b.url));
    artifacts.dedup_by(|a, b| a.url == b.url);

    // The plans the user declared, and how this month's usage looks against
    // them. A plan says what the work was worth; a budget says how much of
    // the limit is gone.
    let now = now_secs();
    let month = budget::month_of(&unix_day(now));
    let plans = budget::statuses(&rows, &budget::load(home), &month);
    let alerts = alerts::evaluate(&live_windows, &plans, now);

    // The tray popover answers from what the last scan found, so opening it
    // never has to walk the logs again.
    let today = unix_day(now);
    let today_rows: Vec<&Row> = rows.iter().filter(|r| r.day == today).collect();
    // A run that used no tokens costs nothing, so it does not make the
    // day's cost unknown.
    let priced_today: Vec<&&Row> = today_rows.iter().filter(|r| r.tokens() > 0).collect();
    tray::remember(tray::ScanSummary {
        today_cost: today_rows.iter().map(|r| r.cost).sum(),
        today_cost_known: priced_today.iter().all(|r| r.cost_known),
        today_cost_partial: today_rows.iter().any(|r| r.cost_partial),
        today_tokens: today_rows.iter().map(|r| r.tokens()).sum(),
        today_requests: today_rows.iter().map(|r| r.requests).sum(),
        alerts: alerts.clone(),
        plans: plans.clone(),
        scanned_at: quota::now_iso(),
    });

    Stats {
        rows,
        sessions,
        limits,
        sources,
        unpriced: unpriced.into_iter().take(12).map(|(m, _)| m).collect(),
        plan_routes: {
            let mut v: Vec<(String, u64)> = plan_routes.into_iter().collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v.into_iter().map(|(p, _)| p).collect()
        },
        quota_error,
        quota_checked_at,
        artifacts,
        alerts,
        plans,
        pricing: PricingInfo {
            source: pricing.source.clone(),
            embedded: pricing.embedded_count,
            local: pricing.local_count,
        },
    }
}

/// The current time in unix seconds.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tauri::command]
async fn check_update() -> Result<updater::UpdateInfo, String> {
    let current_version = env!("CARGO_PKG_VERSION");
    let repo = std::env::var("CINDER_UPDATE_REPO")
        .unwrap_or_else(|_| updater::DEFAULT_REPO.to_string());
    updater::check_for_updates_impl(current_version, &repo).await
}

#[tauri::command]
async fn download_and_install_update(
    app: AppHandle,
    download_url: String,
    asset_name: Option<String>,
) -> Result<(), String> {
    updater::download_and_install_impl(app, download_url, asset_name).await
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The plans the user declared, if any.
#[tauri::command]
fn get_plans() -> Vec<Plan> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    budget::load(&home)
}

/// Remember what the user declared.
#[tauri::command(async)]
fn set_plans(app: AppHandle, plans: Vec<Plan>) -> Result<Vec<Plan>, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    budget::save(&home, &plans)?;
    // A budget that just crossed its limit is worth saying out loud, so the
    // announcements start over.
    alerts::reset_announcements();
    let stats = scan(&|_| {});
    announce(&app, &stats.alerts);
    let _ = app.emit("stats-updated", &stats);
    Ok(budget::load(&home))
}

/// What the tray popover shows.
#[tauri::command]
fn get_tray_summary() -> tray::TraySummary {
    tray::summary()
}

/// Do something the popover asked for.
#[tauri::command]
fn tray_action(app: AppHandle, action: String) {
    match action.as_str() {
        "open" => tray::show_window(&app),
        "rescan" => tray::rescan(&app),
        "quit" => app.exit(0),
        "hide" => tray::hide_popover(&app),
        // A refresh of the live quotas, without waiting for the timer.
        "refresh-quota" => request_quota_refresh(),
        _ => {}
    }
}

// ---------------- System tray ----------------
// The tray shows how close the tightest live quota window is to its ceiling,
// so the number that matters is visible without opening the window, and a
// click opens a popover with the whole picture. The tray itself lives in
// `tray.rs`.

/// Wakes the quota refresher, so a refresh asked for by hand does not wait
/// for the timer.
static QUOTA_WAKE: (std::sync::Mutex<bool>, std::sync::Condvar) =
    (std::sync::Mutex::new(false), std::sync::Condvar::new());

/// Ask the background refresher to run now.
fn request_quota_refresh() {
    let (lock, cvar) = &QUOTA_WAKE;
    let mut flag = lock.lock().unwrap_or_else(|e| e.into_inner());
    *flag = true;
    cvar.notify_all();
}

/// Wait for the next tick, or until somebody asks for a refresh.
fn wait_for_quota_tick() {
    let (lock, cvar) = &QUOTA_WAKE;
    let mut flag = lock.lock().unwrap_or_else(|e| e.into_inner());
    // A little jitter, so machines that were started at the same time do not
    // all ask the providers at the same moment.
    let jitter = std::time::Duration::from_secs((now_secs() % 30) as u64);
    while !*flag {
        let (guard, timeout) = cvar
            .wait_timeout(flag, quota::REFRESH_INTERVAL + jitter)
            .unwrap_or_else(|e| e.into_inner());
        flag = guard;
        if timeout.timed_out() {
            break;
        }
    }
    *flag = false;
}

/// Keep the price list fresh.
///
/// The bundled catalogue is a snapshot taken when the app was built. A fresh
/// one is fetched once a day, so a model that appeared last week is priced at
/// this week's price instead of showing up as unpriced.
fn spawn_price_refresher(app: AppHandle) {
    std::thread::spawn(move || {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        loop {
            if pricing::needs_refresh(&home) {
                let fetched = tokio::runtime::Runtime::new()
                    .map(|rt| rt.block_on(pricing::refresh(&home)))
                    .unwrap_or_else(|_| Err("no runtime".into()));
                if let Ok(count) = fetched {
                    // The prices changed, so the dashboard is rescanned with
                    // them and the user sees the difference.
                    eprintln!("price list refreshed: {count} models");
                    let stats = scan(&|_| {});
                    let _ = app.emit("stats-updated", &stats);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    });
}

/// Ask the providers for their quota windows, over and over.
///
/// The first run happens immediately so the tray and the limits view have real
/// numbers early; later runs wait out the interval. A failed refresh leaves
/// the previous snapshot in place rather than blanking the panel.
fn spawn_quota_refresher(app: AppHandle) {
    std::thread::spawn(move || {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        // The numbers from the last run are on disk, so a launch shows them
        // while the first refresh is still in flight.
        quota::restore_cache(&home);
        tray::refresh();
        loop {
            let reports = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(quota::fetch_all(&home)),
                Err(_) => Vec::new(),
            };
            if !reports.is_empty() {
                quota::publish(reports);
                quota::save_cache(&home, &quota::snapshot().0);
                tray::refresh();
                // The window shows the limits too, so it gets the new numbers.
                let stats = scan(&|_| {});
                announce(&app, &stats.alerts);
                let _ = app.emit("stats-updated", &stats);
            }
            wait_for_quota_tick();
        }
    });
}

/// Tell the user about an alert the first time it shows up.
///
/// The same alert is not announced twice, and one that clears is forgotten so
/// it can fire again later.
fn announce(app: &AppHandle, alerts: &[Alert]) {
    let id = app.config().identifier.clone();
    for alert in alerts::take_new(alerts) {
        let mut n = notify_rust::Notification::new();
        n.summary(&alert.title);
        n.body(&alert.detail);
        // Without this the notification is filed under the PowerShell app id,
        // and the toast says "Windows PowerShell" instead of "Cinder".
        n.app_id(&id);
        let _ = n.show();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The window and the notifications are Cinder's, not the name of whatever
    // started them.
    claim_app_id("com.cinder.usage");
    tauri::Builder::default()
        // Remember where the user put the window instead of resetting to the
        // config default on every launch.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            spawn_log_watcher(app.handle().clone());
            spawn_quota_refresher(app.handle().clone());
            spawn_price_refresher(app.handle().clone());
            if let Err(e) = tray::build(app.handle()) {
                // A missing tray is not a reason to refuse to start.
                eprintln!("system tray unavailable: {e}");
            }
            tray::open_on_start(app.handle());
            // The popover hides itself when it loses the focus, the way a
            // menu does.
            if let Some(window) = app.get_webview_window(tray::POPOVER) {
                tray::watch_popover(&window);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_stats,
            rescan_stats,
            check_update,
            download_and_install_update,
            get_app_version,
            get_plans,
            set_plans,
            get_tray_summary,
            tray_action
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Register new source directories without re-reading any log contents.
fn register_log_watches(
    home: &Path,
    watched: &mut HashSet<PathBuf>,
    mut watch: impl FnMut(&Path, notify::RecursiveMode) -> bool,
) -> bool {
    use notify::RecursiveMode;
    let mut roots: Vec<(PathBuf, RecursiveMode)> = vec![
        (home.join(".pi").join("agent").join("sessions"), RecursiveMode::Recursive),
        (home.join(".codex").join("sessions"), RecursiveMode::Recursive),
        (home.join(".codex").join("archived_sessions"), RecursiveMode::Recursive),
        (home.join(".claude").join("projects"), RecursiveMode::Recursive),
        (home.join(".gemini").join("antigravity"), RecursiveMode::Recursive),
        (home.join(".gemini").join("antigravity-cli"), RecursiveMode::Recursive),
        (home.join(".dsh").join("sessions"), RecursiveMode::Recursive),
        (home.join(".lmstudio").join("conversations"), RecursiveMode::NonRecursive),
        (home.join(".lmstudio").join("server-logs"), RecursiveMode::Recursive),
        // Non-recursive so statev2.sqlite and its WAL trigger live refreshes
        // without watching T3's unrelated logs and conversation trees.
        (home.join(".t3").join("userdata"), RecursiveMode::NonRecursive),
    ];
    // T3's own Antigravity workspaces; enumerated so the watcher does not
    // descend into the unrelated codex shadow copies next to them.
    let t3prov = home.join(".t3").join("userdata").join("providers").join("antigravity");
    if let Ok(top) = fs::read_dir(&t3prov) {
        for e in top.flatten() {
            let acp = e.path().join("antigravity-acp");
            if acp.exists() {
                roots.push((acp, RecursiveMode::Recursive));
            }
        }
    }
    // Non-recursive: the db/wal/shm files live here, and this skips the
    // heavy log/repos/snapshot subtrees (WAL writes hit the -wal file,
    // so the directory itself — not just opencode.db — must be watched).
    roots.push((home.join(".local").join("share").join("opencode"), RecursiveMode::NonRecursive));

    watched.retain(|path| path.exists());
    let mut added = false;
    for (path, mode) in roots {
        if !watched.contains(&path) && path.exists() && watch(&path, mode) {
            watched.insert(path);
            added = true;
        }
    }
    added
}

/// Discover new source directories every 5 s. Rescan changed logs after 10 s
/// quiet (or 60 s of continuous writes), at most once per minute.
fn spawn_log_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        use notify::Watcher;
        use std::time::{Duration, Instant};
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(w) => w,
            Err(_) => return,
        };
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let mut watched = HashSet::new();
        register_log_watches(&home, &mut watched, |p, mode| watcher.watch(p, mode).is_ok());
        let mut last_discovery = Instant::now();
        // Old enough that the first real change rescans after the quiet period;
        // startup itself already ran a full scan via get_stats.
        let mut last_scan = Instant::now() - Duration::from_secs(3600);
        let mut pending = false;
        let mut last_event = Instant::now();
        let mut pending_since = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(_)) => {
                    if !pending {
                        pending_since = Instant::now();
                    }
                    pending = true;
                    last_event = Instant::now();
                }
                Ok(Err(_)) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Only discover directories here; log contents are still notification-driven.
            if last_discovery.elapsed() >= Duration::from_secs(5) {
                if register_log_watches(&home, &mut watched, |p, mode| watcher.watch(p, mode).is_ok()) {
                    if !pending { pending_since = Instant::now(); }
                    pending = true;
                    last_event = Instant::now();
                }
                last_discovery = Instant::now();
            }
            // Rescan 10 s after writes go quiet — or after 60 s of continuous
            // writes, so a long-running agent session can't starve updates.
            let quiet = last_event.elapsed() >= Duration::from_secs(10);
            let overdue = pending_since.elapsed() >= Duration::from_secs(60);
            if pending && (quiet || overdue) {
                if last_scan.elapsed() >= Duration::from_secs(60) {
                    pending = false;
                    let stats = scan(&|_| {});
                    last_scan = Instant::now();
                    let _ = app.emit("stats-updated", &stats);
                } else {
                    // Rate limit: re-check once the minute is up. Events arriving
                    // meanwhile just refresh the quiet timer above.
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        }
    });
}

fn main() {
    run();
}

#[cfg(test)]
mod tests;
