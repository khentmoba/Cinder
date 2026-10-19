// Cinder — coding agent usage statistics collector
//
// Parses the local session stores of the coding agents installed on this machine
// and emits per-day, per-agent, per-model rows. The UI aggregates by time range.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cache;
mod pricing;
mod updater;

use cache::{file_stamp, CachedDb, CachedFile, CachedLimit, ScanCache};
use pricing::Pricing;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Row {
    pub day: String,
    pub agent: String,
    pub model: String,
    pub requests: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
    pub tools: u64,
    pub cost: f64,
    pub cost_known: bool,
    pub cost_partial: bool,
    pub savings: f64,
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
}

#[derive(Serialize)]
struct Source {
    agent: String,
    path: String,
    found: bool,
    files: u64,
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
}

#[derive(Serialize)]
struct PricingInfo {
    source: String,
    embedded: usize,
    local: usize,
}

type Rows = HashMap<(String, String, String), Row>;
type Sess = HashMap<(String, String), u64>;

/// Provider prefix of a model string ("opencode-go/deepseek-v4-flash" -> "opencode-go").
fn model_provider(model: &str) -> Option<&str> {
    model.split_once('/').map(|(p, _)| p).filter(|p| !p.is_empty() && !p.contains(' '))
}

fn add(rows: &mut Rows, sess: &mut Sess, agent: &str, model: &str, day: &str, r: Row) {
    let key = (day.to_string(), agent.to_string(), model.to_string());
    let e = rows.entry(key).or_insert_with(|| Row {
        day: day.to_string(),
        agent: agent.to_string(),
        model: model.to_string(),
        ..Default::default()
    });
    e.requests += r.requests;
    e.input += r.input;
    e.cache_read += r.cache_read;
    e.cache_write += r.cache_write;
    e.output += r.output;
    e.reasoning += r.reasoning;
    e.tools += r.tools;
    e.cost += r.cost;
    e.cost_known = e.cost_known || r.cost_known;
    e.cost_partial |= r.cost_partial || (!r.cost_known && r.tokens() > 0);
    e.savings += r.savings;
    let _ = sess;
}

fn session(sess: &mut Sess, agent: &str, day: &str) {
    *sess.entry((day.to_string(), agent.to_string())).or_insert(0) += 1;
}

fn day_of(ts: &str) -> String {
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

/// True when a line carries at least one of `keys`. A JSONL line that carries none
/// of them cannot change the collected stats, so it is never handed to serde_json.
/// The logs are ~1.6 GB across ~1000 files; skipping half of the lines halves the
/// parse work, which is what a cold start (empty page cache) spends its time on.
fn has_key(line: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| line.contains(k))
}

/// Every line that can contribute a Pi row: the session header (day marker) or an
/// assistant message carrying a usage block.
const PI_KEYS: &[&str] = &["usage", "session"];
/// Every line that can contribute a Codex row: the header/context lines, tool calls
/// and token-count events. Response items and turn events are ignored.
const CODEX_KEYS: &[&str] = &[
    "session_meta",
    "turn_context",
    "response_item",
    "event_msg",
    "function_call",
    "custom_tool_call",
    "token_count",
];
/// Claude assistant messages carrying usage blocks. Without this prefilter every
/// line of every project jsonl goes through serde_json for nothing.
const CLAUDE_KEYS: &[&str] = &["assistant", "usage"];
/// Planner responses, plus the lines that switch the model.
const AGY_KEYS: &[&str] = &["PLANNER_RESPONSE", "Model Selection"];

// ---------------- Pi ----------------
fn parse_single_pi(
    file: &Path,
    pricing: &Pricing,
    rows: &mut Rows,
    sess: &mut Sess,
    plan_routes: &mut HashMap<String, u64>,
) {
    let mut day = String::new();
    each_line(file, |line| {
        if !has_key(line, PI_KEYS) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        if v.get("type").and_then(|t| t.as_str()) == Some("session") && day.is_empty() {
            day = day_of(ts);
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
            ..Default::default()
        };
        // pi accounts for its own routes. A reported zero is meaningful: it means the
        // route is a plan/subscription or has no per-token billing (e.g. zed-ai),
        // so it must not be re-priced with a same-named model from another provider.
        let reported = u.get("cost").and_then(|c| c.get("total")).and_then(|x| x.as_f64());
        let priced = pricing.lookup(provider.as_deref(), &model);
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
    rows: &mut Rows,
    sess: &mut Sess,
    limits_out: &mut Vec<CachedLimit>,
) {
    let mut model = String::from("unknown");
    let mut day = String::new();
    let mut plan = String::new();
    let mut file_latest: HashMap<String, (String, LimitRow)> = HashMap::new();
    each_line(file, |line| {
        if !has_key(line, CODEX_KEYS) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let p = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        if day.is_empty() && ty == "session_meta" {
            day = day_of(ts);
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
                        parse_single_codex(file, pricing_ref, &mut file_rows, &mut file_sess, &mut file_limits);
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
fn parse_single_claude(
    file: &Path,
    pricing: &Pricing,
    rows: &mut Rows,
    sess: &mut Sess,
) {
    let mut day = String::new();
    each_line(file, |line| {
        if !has_key(line, CLAUDE_KEYS) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
        if day.is_empty() && !day_of(ts).is_empty() {
            day = day_of(ts);
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
        if input + output + cache_read + cache_write == 0 {
            return;
        }
        let mut tools = 0u64;
        if let Some(content) = msg.get("content").and_then(|x| x.as_array()) {
            for item in content {
                if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    tools += 1;
                }
            }
        }
        let model = pricing::normalize_model(msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown"));
        let mut r = Row {
            day: day_of(ts),
            agent: "Claude Code".into(),
            model: model.clone(),
            requests: 1,
            input,
            cache_read,
            cache_write,
            output,
            reasoning: 0,
            tools,
            ..Default::default()
        };
        if let Some(pr) = pricing.lookup(model_provider(&model), &model) {
            r.cost = pr.cost(input, cache_read, cache_write, output);
            r.cost_known = true;
            r.savings = pr.savings(cache_read, cache_write);
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
            parse_single_claude(&file, pricing_ref, &mut file_rows, &mut file_sess);
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
    if let Some(cached) = &cache.opencode_db {
        if cached.mtime_ns == mtime_ns && cached.size == size {
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
    if let Ok(mut stmt) = conn.prepare("SELECT data, time_created FROM message") {
        if let Ok(it) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            for (data, ts) in it.flatten() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
                if v.get("role").and_then(|x| x.as_str()) != Some("assistant") {
                    continue;
                }
                let model = opencode_model(&v);
                let day = unix_day(if ts > 1_000_000_000_000 { ts / 1000 } else { ts });
                let u = &v["tokens"];
                let mut r = Row {
                    requests: 1,
                    input: u["input"].as_u64().unwrap_or(0),
                    output: u["output"].as_u64().unwrap_or(0),
                    cache_read: u["cache"]["read"].as_u64().unwrap_or(0),
                    cache_write: u["cache"]["write"].as_u64().unwrap_or(0),
                    reasoning: u["reasoning"].as_u64().unwrap_or(0),
                    ..Default::default()
                };
                let price = pricing.lookup(model_provider(&model), &model);
                if let Some(cost) = v.get("cost").and_then(|x| x.as_f64()).filter(|c| *c >= 0.0) {
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
    // Group tools in SQLite instead of transferring each call's JSON.
    let sql = "SELECT json_extract(m.data, '$.providerID'), json_extract(m.data, '$.modelID'),
                      date(p.time_created/1000,'unixepoch'), COUNT(*)
               FROM part p JOIN message m ON p.message_id = m.id
               WHERE json_valid(p.data) AND json_valid(m.data) AND json_extract(p.data, '$.type') = 'tool'
               GROUP BY 1, 2, 3";
    if let Ok(mut stmt) = conn.prepare(sql) {
        if let Ok(it) = stmt.query_map([], |r| Ok((
            r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?, r.get::<_, u64>(3)?,
        ))) {
            for (provider, model, day, tools) in it.flatten() {
                let Some(day) = day else { continue };
                let model = opencode_model(&serde_json::json!({"providerID": provider, "modelID": model}));
                add(&mut db_rows, &mut db_sess, "OpenCode", &model, &day, Row { tools, ..Default::default() });
            }
        }
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
        count,
        rows: db_rows.into_values().collect(),
        sessions: db_sess.into_keys().collect(),
    });
    cache.dirty = true;
    count
}

fn opencode_model(message: &serde_json::Value) -> String {
    let id = message["modelID"].as_str().unwrap_or("unknown");
    match message["providerID"].as_str().filter(|p| !p.is_empty()) {
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
                if !has_key(line, AGY_KEYS) {
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
            if !has_key(line, AGY_KEYS) {
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
                    ..Default::default()
                };
                if is_prompt {
                    r.input = n;
                } else {
                    r.output = n;
                }
                add(rows, sess, "LM Studio", &model, &day, r);
            }
        });
    }
    (files.len() + logs.len()) as u64
}

fn unix_day(secs: i64) -> String {
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
    progress("model prices");
    let pricing = Pricing::load(home);
    let pricing_fp = (pricing.embedded_count as u64) ^ ((pricing.local_count as u64) << 32);
    let mut cache = ScanCache::load_for(home, pricing_fp);

    let mut rows = Rows::new();
    let mut sess = Sess::new();
    let mut limits = Vec::new();
    let mut counted_threads = HashSet::new();
    let mut plan_routes: HashMap<String, u64> = HashMap::new();

    progress("Pi logs");
    let pi_files = parse_pi(home, &pricing, &mut cache, &mut rows, &mut sess, &mut plan_routes);
    progress("Claude Code logs");
    let claude_files = parse_claude(home, &pricing, &mut cache, &mut rows, &mut sess);
    progress("OpenCode database");
    let opencode_n = parse_opencode(home, &pricing, &mut cache, &mut rows, &mut sess);
    progress("Antigravity logs");
    let agy_dbs = parse_antigravity(home, &pricing, &mut cache, &mut rows, &mut sess);
    progress("T3 logs");
    let t3_files = parse_t3(home, &mut cache, &mut rows, &mut sess, &mut counted_threads);
    progress("T3 usage database");
    let t3_usage_db = parse_t3_usage(home, &pricing, &mut rows, &mut sess, &counted_threads);
    progress("DSH logs");
    let dsh_files = parse_dsh(home, &pricing, &mut cache, &mut rows, &mut sess);
    progress("LM Studio logs");
    let lms_files = parse_lmstudio(home, &mut cache, &mut rows, &mut sess);

    progress("Codex logs");
    let codex_files = parse_codex(home, &pricing, &mut cache, &mut rows, &mut sess, &mut limits);
    cache.save();
    progress("done");

    let sources = vec![
        Source { agent: "Pi".into(), path: "~/.pi/agent/sessions".into(), found: pi_files > 0, files: pi_files },
        Source { agent: "Codex".into(), path: "~/.codex/sessions + archived_sessions".into(), found: codex_files > 0, files: codex_files },
        Source { agent: "Claude Code".into(), path: "~/.claude/projects".into(), found: claude_files > 0, files: claude_files },
        Source { agent: "OpenCode".into(), path: "~/.local/share/opencode/opencode.db".into(), found: opencode_n > 0, files: opencode_n },
        Source {
            agent: "Antigravity".into(),
            path: "~/.gemini/antigravity* + ~/.t3/userdata/statev2.sqlite + providers/antigravity/*/antigravity-acp".into(),
            found: agy_dbs + t3_files > 0 || t3_usage_db,
            files: agy_dbs + t3_files + (t3_usage_db as u64),
        },
        Source { agent: "DSH".into(), path: "~/.dsh/sessions".into(), found: dsh_files > 0, files: dsh_files },
        Source { agent: "LM Studio".into(), path: "~/.lmstudio/conversations".into(), found: lms_files > 0, files: lms_files },
    ];

    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|a, b| a.day.cmp(&b.day).then(a.agent.cmp(&b.agent)).then(a.model.cmp(&b.model)));
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
        pricing: PricingInfo {
            source: pricing.source.clone(),
            embedded: pricing.embedded_count,
            local: pricing.local_count,
        },
    }
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            spawn_log_watcher(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_stats,
            rescan_stats,
            check_update,
            download_and_install_update,
            get_app_version
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
