// Model pricing lookup.
// Priority: local agent catalogs (exact prices for the user's own providers)
//   -> embedded LiteLLM catalog (exact) -> normalized/suffix match -> family fallback.
// Prices are USD per 1M tokens.
//
// The embedded catalog is a snapshot. A fresh one is fetched in the background
// and kept on disk, so a model that appeared last week is priced this week.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const EMBEDDED: &str = include_str!("../prices.json");

/// Where the price list comes from.
const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
/// How long a fetched catalogue is used before a new one is fetched.
const CATALOG_TTL_HOURS: u64 = 24;

/// The premium a provider charges past a long context.
///
/// Some models cost more once the request is longer than a threshold: the
/// catalogue carries the rates for that case, and the threshold is the
/// prompt length (the input, the cached input and the cache writes) past
/// which they apply.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Tier {
    /// The prompt length past which the premium applies.
    pub above: u64,
    pub i: f64,
    pub o: f64,
    pub cr: f64,
    pub cw: f64,
    /// The 1-hour cache write rate past the threshold, when it is known.
    #[serde(default)]
    pub cw1h: f64,
}

impl Tier {
    /// The 1-hour ephemeral cache write rate past the threshold: twice the
    /// input price, when the catalogue does not say.
    pub fn cache_write_1h_rate(&self) -> f64 {
        if self.cw1h > 0.0 {
            self.cw1h
        } else {
            self.i * 2.0
        }
    }
    /// The cost of a request priced past the threshold.
    pub fn cost_split(
        &self,
        input: u64,
        cache_read: u64,
        cache_write: u64,
        cache_write_1h: u64,
        output: u64,
    ) -> f64 {
        let five_min = cache_write.saturating_sub(cache_write_1h);
        (input as f64 * self.i
            + cache_read as f64 * self.cr
            + five_min as f64 * self.cw
            + cache_write_1h as f64 * self.cache_write_1h_rate()
            + output as f64 * self.o)
            / 1e6
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Price {
    pub i: f64,
    pub o: f64,
    pub cr: f64,
    pub cw: f64,
    /// The premium past a long context, when the provider charges one.
    #[serde(default)]
    pub tier: Option<Tier>,
}

impl Price {
    /// The rate for a request of this size.
    ///
    /// A request whose prompt is past the provider's long-context threshold is
    /// priced at the premium rates, if the catalogue has them.
    fn rates_for(&self, prompt: u64) -> Option<Tier> {
        match self.tier {
            Some(t) if prompt > t.above => Some(t),
            _ => None,
        }
    }
    pub fn cost(&self, input: u64, cache_read: u64, cache_write: u64, output: u64) -> f64 {
        let prompt = input + cache_read + cache_write;
        if let Some(t) = self.rates_for(prompt) {
            return t.cost_split(input, cache_read, cache_write, 0, output);
        }
        (input as f64 * self.i + cache_read as f64 * self.cr + cache_write as f64 * self.cw + output as f64 * self.o) / 1e6
    }
    /// The 1-hour ephemeral cache write rate: twice the base input price.
    ///
    /// Anthropic bills a 1-hour cache write at twice the input price, and a
    /// 5-minute write at 1.25x it - so the 1-hour write is 1.6x the 5-minute
    /// one, not twice it. The rate the CLI itself charges agrees: for
    /// claude-opus-5-5, where the input price is $4 per million, a session
    /// matches to the cent at $8 per million for the 1-hour writes and not at
    /// $10.
    pub fn cache_write_1h_rate(&self) -> f64 {
        self.i * 2.0
    }
    /// The cost of a request where part of the cache writes are billed at the
    /// 1-hour ephemeral rate. `cache_write` is the total written, of which
    /// `cache_write_1h` uses the longer lifetime.
    pub fn cost_split(
        &self,
        input: u64,
        cache_read: u64,
        cache_write: u64,
        cache_write_1h: u64,
        output: u64,
    ) -> f64 {
        let prompt = input + cache_read + cache_write;
        if let Some(t) = self.rates_for(prompt) {
            return t.cost_split(input, cache_read, cache_write, cache_write_1h, output);
        }
        let five_min = cache_write.saturating_sub(cache_write_1h);
        (input as f64 * self.i
            + cache_read as f64 * self.cr
            + five_min as f64 * self.cw
            + cache_write_1h as f64 * self.cache_write_1h_rate()
            + output as f64 * self.o)
            / 1e6
    }
    /// What caching saved versus sending the same tokens as plain input.
    pub fn savings(&self, cache_read: u64, cache_write: u64) -> f64 {
        let read = if self.cr > 0.0 { self.cr } else { self.i };
        let write = if self.cw > 0.0 { self.cw } else { self.i };
        (cache_read as f64 * (self.i - read) + cache_write as f64 * (self.i - write)).max(0.0) / 1e6
    }
    /// What caching saved, where part of the writes use the 1-hour rate and
    /// therefore cost more than plain input rather than less.
    ///
    /// `input` is the uncached input of the request: the prompt length is
    /// what decides whether the premium rates apply, and the saving is
    /// measured against the rates the request is billed at.
    pub fn savings_split(&self, input: u64, cache_read: u64, cache_write: u64, cache_write_1h: u64) -> f64 {
        let five_min = cache_write.saturating_sub(cache_write_1h);
        let (i, read, write, one_hour) = match self.rates_for(input + cache_read + cache_write) {
            Some(t) => (t.i, t.cr, t.cw, t.cache_write_1h_rate()),
            None => (self.i, self.cr, self.cw, self.cache_write_1h_rate()),
        };
        let read = if read > 0.0 { read } else { i };
        let write = if write > 0.0 { write } else { i };
        let saved = cache_read as f64 * (i - read) + five_min as f64 * (i - write);
        let spent = cache_write_1h as f64 * (one_hour - i);
        (saved - spent).max(0.0) / 1e6
    }
}

pub struct Pricing {
    embedded: HashMap<String, Price>,
    /// index from model tail (after last '/') to candidate keys
    embedded_tail: HashMap<String, Vec<String>>,
    /// "provider/id" -> price, from the local agent catalogs
    local_by_provider: HashMap<String, Price>,
    /// bare "id" -> price, only when every provider agrees on the price
    local_bare: HashMap<String, Price>,
    pub source: String,
    pub embedded_count: usize,
    pub local_count: usize,
    /// A number that changes when the prices change, so a scan cache built
    /// against the old prices is rebuilt instead of replayed.
    pub fingerprint: u64,
}

fn norm(s: &str) -> String {
    let l = s.to_lowercase();
    // strip provider prefixes and known decorations
    let out = {
        // strip provider prefixes and known decorations
        let mut last = String::new();
        for part in l.split('/') {
            if !part.is_empty() {
                last = part.to_string();
            }
        }
        last
    };
    // strip trailing date / build tags: -20250929, -v1:0, @free, :free, -latest
    let mut base = out.clone();
    if let Some(i) = base.find('(') {
        base.truncate(i);
    }
    base = base.trim().replace(' ', "-");
    // DSH names the model with its route baked in (antigravity-gemini-3.8-flash
    // served by the antigravity route); the catalog prices the bare id.
    if let Some(stripped) = base.strip_prefix("antigravity-") {
        base = stripped.to_string();
    }
    for pat in [":free", "@free", "-latest", "-preview", "-tiered", "-thinking", "-high", "-medium", "-low", "-exp"] {
        if let Some(i) = base.rfind(pat) {
            base.truncate(i);
        }
    }
    // strip a trailing context-size tag like -1m (gpt-6-luna-1m is the 1M-context
    // variant of gpt-6-luna at the same base price; tiers above 272k input
    // tokens cost more, which this estimate does not capture)
    let mut context_variant = false;
    if let Some(dash) = base.rfind('-') {
        let suf = &base[dash + 1..];
        if suf.strip_suffix('m').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
            base.truncate(dash);
            context_variant = true;
        }
    }
    // strip trailing date digits group like -20250929 or -v2
    let bytes: Vec<char> = base.chars().collect();
    let mut cut = bytes.len();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    let digits = bytes.len() - i;
    // A context variant keeps its own version: "gpt-5-1m" is priced as gpt-5,
    // not as gpt. A date tag is still stripped, so "gpt-4-0613-1m" finds gpt-4.
    if i < bytes.len() && i > 0 && bytes[i - 1] == '-' && (digits >= 4 || !context_variant) {
        cut = i - 1;
    }
    // strip a bare trailing digit group (gpt-5-4 -> keep) only when preceded by '-'
    let base: String = bytes[..cut].iter().collect();
    let base = base.trim_end_matches('-').to_string();
    if base.is_empty() { out } else { base }
}

fn tail(s: &str) -> String {
    s.rsplit('/').next().unwrap_or(s).to_lowercase()
}

/// Read a price catalogue into the tables the lookup uses.
///
/// The shape is the one the bundled catalogue has: a `models` map of
/// `{"i":..,"o":..,"cr":..,"cw":..}` per model.
fn parse_catalog(text: &str) -> (HashMap<String, Price>, HashMap<String, Vec<String>>, String) {
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    let mut models = HashMap::new();
    let mut tail_index: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(entries) = parsed.get("models").and_then(|m| m.as_object()) {
        for (k, v) in entries {
            let p = Price {
                i: v.get("i").and_then(|x| x.as_f64()).unwrap_or(0.0),
                o: v.get("o").and_then(|x| x.as_f64()).unwrap_or(0.0),
                cr: v.get("cr").and_then(|x| x.as_f64()).unwrap_or(0.0),
                cw: v.get("cw").and_then(|x| x.as_f64()).unwrap_or(0.0),
                tier: v.get("tier").and_then(|t| serde_json::from_value(t.clone()).ok()),
            };
            let key = k.to_lowercase();
            tail_index.entry(tail(k)).or_default().push(key.clone());
            models.insert(key, p);
        }
    }
    let meta = parsed.get("meta");
    let source = meta
        .and_then(|m| m.get("source"))
        .and_then(|x| x.as_str())
        .unwrap_or("LiteLLM")
        .to_string();
    let fetched = meta
        .and_then(|m| m.get("fetched"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let source = if fetched.is_empty() { source } else { format!("{source} ({fetched})") };
    (models, tail_index, source)
}

/// A number that changes when the prices change.
fn fingerprint(source: &str, embedded: usize, local: usize) -> u64 {
    // A plain FNV-1a over the source and the counts: enough to notice that
    // the catalogue changed, which is all the scan cache needs.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.bytes().chain(embedded.to_le_bytes()).chain(local.to_le_bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Where the fetched catalogue is kept.
pub fn catalog_path(home: &Path) -> PathBuf {
    home.join(".cinder").join("prices.json")
}

/// The fetched catalogue, kept so a scan does not read and parse the file
/// every time.
static REFRESHED: LazyLock<std::sync::Mutex<Option<Refreshed>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

/// A parsed fetched catalogue.
type Refreshed = (u64, HashMap<String, Price>, HashMap<String, Vec<String>>, String);

/// The parsed tables of a catalogue.
type Catalog = (HashMap<String, Price>, HashMap<String, Vec<String>>, String);

/// The fetched catalogue, when there is one and it is still fresh.
fn refreshed_catalog(home: &Path) -> Option<Catalog> {
    let path = catalog_path(home);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(modified).ok()?;
    // A catalogue that is older than a day is not used: the refresh thread
    // will fetch a new one.
    if age.as_secs() > CATALOG_TTL_HOURS * 3600 {
        return None;
    }
    let stamp = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as u64;
    {
        let guard = REFRESHED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, models, tail, source)) = guard.as_ref() {
            if *s == stamp {
                return Some((models.clone(), tail.clone(), source.clone()));
            }
        }
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let (models, tail, source) = parse_catalog(&text);
    if models.is_empty() {
        return None;
    }
    let mut guard = REFRESHED.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((stamp, models.clone(), tail.clone(), source.clone()));
    Some((models, tail, source))
}

/// Whether the catalogue on disk is old enough to be worth fetching again.
pub fn needs_refresh(home: &Path) -> bool {
    let path = catalog_path(home);
    let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
        return true;
    };
    match std::time::SystemTime::now().duration_since(modified) {
        Ok(age) => age.as_secs() > CATALOG_TTL_HOURS * 3600,
        Err(_) => true,
    }
}

/// Fetch a fresh price list and keep it on disk.
///
/// The list is converted into the shape the bundled catalogue has, so the
/// lookup does not have to know where the prices came from. A fetch that
/// fails, or that answers with something unexpected, leaves the catalogue
/// that is already there alone.
pub async fn refresh(home: &Path) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("Cinder")
        .build()
        .map_err(|e| e.to_string())?;
    let text = client
        .get(LITELLM_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let catalog = convert_litellm(&text)?;
    let path = catalog_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(&catalog).map_err(|e| e.to_string())?;
    // The file is written in one go through a temporary, so a crash halfway
    // through cannot leave a half-written catalogue behind.
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    Ok(catalog.get("models").and_then(|m| m.as_object()).map(|m| m.len()).unwrap_or(0))
}

/// The premium a model is charged past a long context.
///
/// LiteLLM names those rates after the threshold they apply at - "the input
/// cost above 100k tokens" - so the threshold comes out of the field name.
/// The first (lowest) threshold is the one that applies first.
fn litellm_tier(entry: &serde_json::Map<String, serde_json::Value>) -> Option<serde_json::Value> {
    let per_million = |v: Option<f64>| v.unwrap_or(0.0) * 1_000_000.0;
    let above = |name: &str| -> Option<u64> {
        let rest = name.strip_suffix("_tokens")?;
        let rest = rest.rsplit_once("above_")?.1;
        let k = rest.strip_suffix('k')?;
        k.parse::<u64>().ok().map(|n| n * 1000)
    };
    let find = |prefix: &str| -> Option<(u64, f64)> {
        for (name, value) in entry {
            if !name.starts_with(prefix) || !name.ends_with("_tokens") {
                continue;
            }
            let n = above(name)?;
            let v = value.as_f64()?;
            return Some((n, per_million(Some(v))));
        }
        None
    };
    // The threshold every one of the rates agrees on.
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for prefix in [
        "input_cost_per_token_above_",
        "output_cost_per_token_above_",
        "cache_read_input_token_cost_above_",
        "cache_creation_input_token_cost_above_",
    ] {
        if let Some((n, _)) = find(prefix) {
            *counts.entry(n).or_default() += 1;
        }
    }
    let threshold = counts
        .into_iter()
        .filter(|(_, c)| *c >= 3)
        .map(|(n, _)| n)
        .min()?;
    let i = find("input_cost_per_token_above_").filter(|(n, _)| *n == threshold).map(|(_, v)| v)?;
    let o = find("output_cost_per_token_above_").filter(|(n, _)| *n == threshold).map(|(_, v)| v)?;
    let cr = find("cache_read_input_token_cost_above_").filter(|(n, _)| *n == threshold).map(|(_, v)| v).unwrap_or(0.0);
    let cw = find("cache_creation_input_token_cost_above_").filter(|(n, _)| *n == threshold).map(|(_, v)| v).unwrap_or(0.0);
    // The 1-hour cache write rate past the threshold is named differently, and
    // only some models carry it.
    let mut cw1h = 0.0;
    for (name, value) in entry {
        if !name.contains("above_1hr_above_") {
            continue;
        }
        if above(name) == Some(threshold) {
            cw1h = per_million(value.as_f64());
        }
    }
    Some(serde_json::json!({
        "above": threshold,
        "i": i,
        "o": o,
        "cr": cr,
        "cw": cw,
        "cw1h": cw1h,
    }))
}

/// The LiteLLM price list, in the shape the bundled catalogue has.
///
/// LiteLLM quotes prices per token; the catalogue quotes them per million,
/// which is how the dashboard reads them.
fn convert_litellm(text: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(entries) = parsed.as_object() else {
        return Err("the price list was not an object".into());
    };
    let mut models = serde_json::Map::new();
    for (k, v) in entries {
        // The file carries an example entry that is not a model.
        if k == "sample_spec" {
            continue;
        }
        let Some(entry) = v.as_object() else { continue };
        let input = entry.get("input_cost_per_token").and_then(|x| x.as_f64());
        let output = entry.get("output_cost_per_token").and_then(|x| x.as_f64());
        // An entry with no price at all is not a model Cinder can price.
        if input.is_none() && output.is_none() {
            continue;
        }
        let per_million = |v: Option<f64>| v.unwrap_or(0.0) * 1_000_000.0;
        let price = serde_json::json!({
            "i": per_million(input),
            "o": per_million(output),
            "cr": per_million(entry.get("cache_read_input_token_cost").and_then(|x| x.as_f64())),
            "cw": per_million(entry.get("cache_creation_input_token_cost").and_then(|x| x.as_f64())),
            "p": entry.get("litellm_provider").and_then(|x| x.as_str()).unwrap_or(""),
        });
        let mut price = price;
        if let Some(tier) = litellm_tier(entry) {
            price["tier"] = tier;
        }
        models.insert(k.to_lowercase(), price);
    }
    if models.is_empty() {
        return Err("the price list held no prices".into());
    }
    let count = models.len();
    Ok(serde_json::json!({
        "meta": {
            "fetched": crate::unix_day(crate::now_secs()),
            "models": count,
            "repo": "https://github.com/BerriAI/litellm",
            "source": "LiteLLM model_prices_and_context_window.json",
            "unit": "USD per 1M tokens",
        },
        "models": models,
    }))
}

impl Pricing {
    pub fn load(home: &std::path::Path) -> Pricing {
        let (mut embedded, mut embedded_tail, mut source) = parse_catalog(EMBEDDED);
        let bundled_count = embedded.len();
        // A catalogue fetched earlier wins over the bundled one, so a model
        // that appeared last week is priced at this week's price.
        if let Some((fresh, fresh_tail, fresh_source)) = refreshed_catalog(home) {
            if !fresh.is_empty() {
                let mut merged = embedded;
                merged.extend(fresh);
                let mut tail_index = embedded_tail;
                for (k, v) in fresh_tail {
                    tail_index.entry(k).or_default().extend(v);
                }
                embedded = merged;
                embedded_tail = tail_index;
                source = format!("{fresh_source} · {bundled_count} bundled");
            }
        }

        let mut local = HashMap::new();
        load_clinepass(&home.join(".pi").join("agent").join("clinepass-prices.json"), &mut local);
        load_pi_catalog(&home.join(".pi").join("agent").join("models-store.json"), &mut local);
        load_pi_catalog(&home.join(".pi").join("agent").join("models.json"), &mut local);
        // A bare id is only usable when all providers agree, including cache prices.
        let mut local_bare: HashMap<String, Price> = HashMap::new();
        let mut local_conflicts = std::collections::HashSet::new();
        for (key, price) in &local {
            let id = tail(key);
            if local_bare.get(&id).is_some_and(|previous| previous != price) {
                local_conflicts.insert(id.clone());
            }
            local_bare.insert(id, *price);
        }
        local_bare.retain(|id, _| !local_conflicts.contains(id));

        let embedded_count = embedded.len();
        let local_count = local.len();
        let fingerprint = fingerprint(&source, embedded_count, local_count);

        Pricing {
            embedded_count,
            local_count,
            embedded,
            embedded_tail,
            local_by_provider: local,
            local_bare,
            source,
            fingerprint,
        }
    }

    fn lookup_local(&self, provider: Option<&str>, model: &str) -> Option<Price> {
        let m = model.to_lowercase();
        let t = tail(&m);
        let mut tails = vec![t.clone()];
        let nt = norm(&t);
        if nt != t {
            tails.push(nt.clone());
        }
        // Also try converting version dots <-> dashes: e.g. 3-8 <-> 3.8
        if t.contains('.') {
            tails.push(t.replace('.', "-"));
        } else if t.contains('-') {
            let mut dotted = String::new();
            let chars: Vec<char> = t.chars().collect();
            for i in 0..chars.len() {
                if chars[i] == '-' && i > 0 && i + 1 < chars.len() && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit() {
                    dotted.push('.');
                } else {
                    dotted.push(chars[i]);
                }
            }
            if dotted != t {
                tails.push(dotted.clone());
                tails.push(norm(&dotted));
            }
        }
        tails.dedup();

        match provider {
            // a known provider is priced strictly by its own catalog entry
            Some(p) => {
                for cand in provider_candidates(p) {
                    let id = m.strip_prefix(&format!("{}/", cand)).unwrap_or(&m);
                    if let Some(price) = self.local_by_provider.get(&format!("{}/{}", cand, id)) {
                        return Some(*price);
                    }
                    for tail in &tails {
                        if let Some(pr) = self
                            .local_by_provider
                            .get(&format!("{}/{}", cand, tail))
                        {
                            return Some(*pr);
                        }
                    }
                }
                None
            }
            // unknown provider: a bare id is fine, unless providers disagree about it
            None => tails.iter().find_map(|id| self.local_bare.get(id).copied()),
        }
    }

    fn lookup_embedded(&self, model: &str) -> Option<Price> {
        let m = model.to_lowercase();
        if let Some(p) = self.embedded.get(&m) {
            return Some(*p);
        }
        let n = norm(&m);
        if let Some(p) = self.embedded.get(&n) {
            return Some(*p);
        }
        // suffix match: any catalog entry with the same tail
        let t = tail(&m);
        for cand in [t, n.clone()] {
            if let Some(list) = self.embedded_tail.get(&cand) {
                // prefer the shortest provider prefix that still matches, i.e. canonical names
                let mut best: Option<(usize, Price)> = None;
                for key in list {
                    let p = self.embedded[key];
                    let score = key.len(); // canonical bare names are shortest
                    if best.map(|(s, _)| score < s).unwrap_or(true) {
                        best = Some((score, p));
                    }
                }
                if let Some((_, p)) = best {
                    return Some(p);
                }
            }
        }
        None
    }

    fn family(&self, model: &str) -> Option<Price> {
        // last resort: family heuristics on the normalized name
        let n = norm(model);
        let family: &[(&str, &str)] = &[
            ("opus", "claude-opus-4-5"),
            ("sonnet", "claude-sonnet-4-5"),
            ("haiku", "claude-haiku-4-5"),
            ("gpt-oss", "groq/openai/gpt-oss-120b"),
            ("gpt-5-nano", "gpt-5-nano"),
            ("gpt-5-mini", "gpt-5-mini"),
            ("gpt-5", "gpt-5"),
            ("gpt-4.1-mini", "gpt-4.1-mini"),
            ("gpt-4.1", "gpt-4.1"),
            ("gemini-3", "aihubmix/gemini-3-flash-preview"),
            ("gemini-2.5-flash", "gemini-2.5-flash"),
            ("gemini-2.5-pro", "gemini-2.5-pro"),
            ("agnes", "aihubmix/agnes-2.5-flash"),
            ("bonsai", "openrouter/prism-ml/ternary-bonsai-2-27b"),
            ("deepseek", "deepseek-chat"),
            ("qwen", "qwen-plus"),
            ("glm", "glm-4.6"),
            ("mistral", "mistral-large-latest"),
            ("llama", "llama-3.3-70b-versatile"),
            ("grok", "grok-4"),
        ];
        for (key, canon) in family {
            if n.contains(key) {
                if let Some(p) = self.embedded.get(*canon) {
                    return Some(*p);
                }
            }
        }
        None
    }

    /// The price of a stand-in cloud model, used to put a number on what a
    /// local run saved.
    ///
    /// `gpt-4o` is the baseline because its list price is one most readers
    /// can check; the others are fallbacks for a catalog that does not carry
    /// it.
    pub fn reference_cloud(&self) -> Option<Price> {
        for id in ["gpt-4o", "claude-sonnet-4-5", "gpt-4.1", "gemini-2.5-flash"] {
            if let Some(p) = self.embedded.get(id) {
                return Some(*p);
            }
        }
        None
    }

    /// Price a request. `provider` is the route that actually served it; when it is
    /// known we never fall back to another provider's price for the same model name,
    /// because subscription routes (Zed AI, Antigravity plans, contributor tiers)
    /// are not billed per token.
    pub fn lookup(&self, provider: Option<&str>, model: &str) -> Option<Price> {
        if model.is_empty() || model == "unknown" {
            return None;
        }
        let (inferred_provider, _model_tail) = match model.split_once('/') {
            Some((p, m)) if !p.is_empty() && !m.is_empty() => (Some(p), m),
            _ => (None, model),
        };
        let effective_provider = provider.or(inferred_provider);

        // The local catalogues are a refinement: they know what each route
        // charges. When they disagree about a bare model name, the route that
        // served the request is not known, so their price cannot be used â€”
        // but that does not mean the work was free. The public list price is
        // the better answer than nothing, and it is the one the dashboard
        // quotes everywhere else.
        if let Some(p) = self.lookup_local(effective_provider, model) {
            return Some(p);
        }
        if effective_provider != provider {
            if let Some(p) = self.lookup_local(provider, model) {
                return Some(p);
            }
        }
        // free / contributor routes genuinely cost nothing, whatever the provider
        let m = model.to_lowercase();
        if is_free_route(&m) {
            return Some(Price { i: 0.0, o: 0.0, cr: 0.0, cw: 0.0, ..Default::default() });
        }
        // Public list prices are only a fair stand-in for providers that actually bill
        // per token. A subscription route (zed-ai, antigravity, ...) is covered by its
        // plan, so pricing it from a same-named OpenAI/Anthropic model would be fiction.
        let mirror = match effective_provider {
            None => true,
            Some(p) => provider_candidates(p)
                .iter()
                .any(|c| MIRROR_PROVIDERS.contains(&c.as_str())),
        };
        if !mirror {
            return None;
        }
        self.lookup_embedded(model).or_else(|| self.family(model))
    }
}

/// Providers that bill per token through the public price lists.
const MIRROR_PROVIDERS: &[&str] = &[
    "openai", "anthropic", "google", "gemini", "vertex_ai-language-models", "azure",
    "openrouter", "deepseek", "mistral", "xai", "groq", "together_ai", "fireworks_ai",
    "bedrock", "ollama", "lm_studio", "tokenharbor", "clinepass", "cline-pass", "cline",
    "explabs",
];

/// "opencode-go-responses" and "opencode-go" are the same Zen route with a different
/// API shape, and "mimo" is xiaomi's vendor prefix. Try these aliases in order.
fn provider_candidates(provider: &str) -> Vec<String> {
    let p = provider.to_lowercase();
    let mut out = vec![p.clone()];
    for suffix in ["-responses", "-free-responses", "-chat", "-go", "-zen", "-free", "-pass", "-official", "-account"] {
        if let Some(base) = p.strip_suffix(suffix) {
            out.push(base.to_string());
            if base.starts_with("opencode") {
                out.push("opencode".to_string());
            }
        }
    }
    match p.as_str() {
        // pi's catalog prices these vendor models under the Zen route that serves them
        "mimo" | "xiaomi" => {
            out.push("opencode-go".to_string());
            out.push("opencode".to_string());
        }
        "openai-codex" => out.push("openai".to_string()),
        "cline" | "clinepass" | "cline-pass" => {
            out.push("cline-pass".to_string());
            out.push("clinepass".to_string());
            out.push("cline".to_string());
        }
        "deepseek-official" | "deepseek-account" => {
            out.push("deepseek".to_string());
        }
        // user-defined routes that re-serve known catalog models under the
        // same ids (verified identical prices across the aliased providers)
        "custom" => {
            out.push("opencode-go".to_string());
            out.push("opencode".to_string());
            out.push("meta".to_string());
        }
        "codex" => {
            out.push("openai-codex".to_string());
            out.push("openai".to_string());
        }
        "opencode2dsh" => out.push("opencode".to_string()),
        "google-antigravity" => out.push("antigravity".to_string()),
        "codex-router" => {
            out.push("custom".to_string());
            out.push("opencode-go".to_string());
            out.push("opencode".to_string());
            out.push("meta".to_string());
        }
        _ => {}
    }
    out.dedup();
    out
}

/// Some logs record the model as a serialized object: {"id":"muse-spark-1.2","providerID":"opencode"}
pub fn normalize_model(model: &str) -> String {
    let t = model.trim();
    if !t.starts_with('{') {
        return t.to_string();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
        return t.to_string();
    };
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let prov = v
        .get("providerID")
        .or_else(|| v.get("providerId"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if id.is_empty() {
        t.to_string()
    } else if prov.is_empty() {
        id.to_string()
    } else {
        format!("{}/{}", prov, id)
    }
}

fn is_free_route(model: &str) -> bool {
    let tail = tail(model);
    tail.ends_with("-free")
        || tail == "free"
        || tail.contains(":free")
        || model.contains("/free")
        || tail.ends_with("-free-responses")
}

fn price_from_json(v: &serde_json::Value) -> Option<Price> {
    let i = v.get("input").and_then(|x| x.as_f64());
    let o = v.get("output").and_then(|x| x.as_f64());
    // a catalog entry that declares both prices is authoritative even at zero (free routes)
    let (i, o) = (i?, o?);
    let cr = v.get("cacheRead").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let cw = v.get("cacheWrite").and_then(|x| x.as_f64()).unwrap_or(0.0);
    // The premium past a long context, when the catalog lists one.
    let tier = v.get("tiers").and_then(|t| t.as_array()).and_then(|list| {
        list.iter().find_map(|t| {
            let above = t.get("inputTokensAbove").and_then(|x| x.as_u64())?;
            let ti = t.get("input").and_then(|x| x.as_f64())?;
            let to = t.get("output").and_then(|x| x.as_f64())?;
            Some(Tier {
                above,
                i: ti,
                o: to,
                cr: t.get("cacheRead").and_then(|x| x.as_f64()).unwrap_or(0.0),
                cw: t.get("cacheWrite").and_then(|x| x.as_f64()).unwrap_or(0.0),
                cw1h: 0.0,
            })
        })
    });
    Some(Price { i, o, cr, cw, tier })
}

/// ~/.pi/agent/models-store.json : { provider: { models: [ { id, cost: {...} } ] } }
/// (older builds used an object keyed by model id; both shapes are accepted)
fn load_pi_catalog(path: &std::path::Path, out: &mut HashMap<String, Price>) {
    let Some(v) = crate::read_json_file(path) else { return };
    let providers = if let Some(pv) = v.get("providers").and_then(|p| p.as_object()) {
        pv
    } else if let Some(pv) = v.as_object() {
        pv
    } else {
        return;
    };
    for (provider, pv) in providers {
        let Some(models) = pv.get("models") else { continue };
        let mut insert = |id: &str, mv: &serde_json::Value| {
            let Some(cost) = mv.get("cost") else { return };
            if let Some(p) = price_from_json(cost) {
                let idl = id.to_lowercase();
                let key = format!("{}/{}", provider.to_lowercase(), idl);
                out.insert(key, p);
            }
        };
        match models {
            serde_json::Value::Array(list) => {
                for mv in list.iter() {
                    if let Some(id) = mv.get("id").and_then(|x| x.as_str()) {
                        insert(id, mv);
                    }
                }
            }
            serde_json::Value::Object(map) => {
                for (id, mv) in map {
                    insert(id, mv);
                }
            }
            _ => {}
        }
    }
}

/// ~/.pi/agent/clinepass-prices.json : { models: { key: { input, output, cacheRead } } }
fn load_clinepass(path: &std::path::Path, out: &mut HashMap<String, Price>) {
    let Some(v) = crate::read_json_file(path) else { return };
    let Some(models) = v.get("models").and_then(|m| m.as_object()) else { return };
    for (id, mv) in models {
        if let Some(p) = price_from_json(mv) {
            out.insert(id.to_lowercase(), p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_price_list_is_converted_into_the_shape_the_catalogue_uses() {
        let litellm = r#"{
            "sample_spec": {"input_cost_per_token": 0.0},
            "gpt-4o": {
                "input_cost_per_token": 2.5e-06,
                "output_cost_per_token": 1e-05,
                "cache_read_input_token_cost": 1.25e-06,
                "litellm_provider": "openai"
            },
            "free-model": {"input_cost_per_token": 0.0, "output_cost_per_token": 0.0},
            "no-price-at-all": {"max_tokens": 100}
        }"#;
        let catalog = convert_litellm(litellm).expect("the list should convert");
        let models = catalog.get("models").and_then(|m| m.as_object()).unwrap();
        // The sample entry and the entry without a price are left out.
        assert_eq!(models.len(), 2);
        let gpt = models.get("gpt-4o").unwrap();
        // LiteLLM quotes per token; the catalogue quotes per million.
        assert_eq!(gpt.get("i").and_then(|x| x.as_f64()), Some(2.5));
        assert_eq!(gpt.get("o").and_then(|x| x.as_f64()), Some(10.0));
        assert_eq!(gpt.get("cr").and_then(|x| x.as_f64()), Some(1.25));
        assert_eq!(gpt.get("cw").and_then(|x| x.as_f64()), Some(0.0));
        assert_eq!(gpt.get("p").and_then(|x| x.as_str()), Some("openai"));
        // A model the list says is free stays free.
        let free = models.get("free-model").unwrap();
        assert_eq!(free.get("i").and_then(|x| x.as_f64()), Some(0.0));
    }

    #[test]
    fn a_price_list_that_is_not_a_price_list_is_refused() {
        assert!(convert_litellm("not json").is_err());
        assert!(convert_litellm("{}").is_err());
        assert!(convert_litellm(r#"{"sample_spec": {}}"#).is_err());
    }

    #[test]
    fn a_fetched_catalogue_wins_over_the_bundled_one() {
        let dir = std::env::temp_dir().join(format!("cinder-prices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A catalogue with one model, at a price the bundled one does not
        // have, plus one model with a different price than the bundled one.
        let catalog = serde_json::json!({
            "meta": { "source": "LiteLLM", "fetched": "2026-10-22" },
            "models": {
                "gpt-4o": { "i": 1.0, "o": 2.0, "cr": 0.0, "cw": 0.0 },
                "brand-new-model": { "i": 3.0, "o": 4.0, "cr": 0.0, "cw": 0.0 }
            }
        });
        std::fs::create_dir_all(catalog_path(&dir).parent().unwrap()).unwrap();
        std::fs::write(catalog_path(&dir), serde_json::to_vec(&catalog).unwrap()).unwrap();

        let pricing = Pricing::load(&dir);
        // The fetched price wins.
        let gpt = pricing.lookup(None, "gpt-4o").expect("gpt-4o should be priced");
        assert_eq!(gpt.i, 1.0);
        // A model the bundled catalogue never heard of is priced now.
        let fresh = pricing.lookup(None, "brand-new-model").expect("the new model should be priced");
        assert_eq!(fresh.i, 3.0);
        // A model the fetched catalogue does not mention still has the bundled
        // price.
        let other = pricing.lookup(None, "claude-sonnet-4-5").expect("the bundled price should still be there");
        assert!(other.i > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_fingerprint_changes_when_the_prices_change() {
        let a = fingerprint("LiteLLM (2026-10-01)", 100, 2);
        let b = fingerprint("LiteLLM (2026-10-22)", 100, 2);
        assert_ne!(a, b);
        assert_eq!(a, fingerprint("LiteLLM (2026-10-01)", 100, 2));
    }

    #[test]
    fn a_catalogue_that_is_missing_is_not_a_reason_to_fail() {
        let dir = std::env::temp_dir().join(format!("cinder-prices-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Pricing::load(&dir).lookup(None, "gpt-4o").is_some());
        assert!(needs_refresh(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fetches the real price list, which reaches the network.
    #[test]
    #[ignore = "reaches the network"]
fn live_price_refresh() {
        let dir = std::env::temp_dir().join(format!("cinder-prices-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let count = rt.block_on(refresh(&dir)).expect("the price list should be fetched");
        println!("fetched {count} models");
        assert!(count > 1000, "the price list should hold thousands of models");
        let pricing = Pricing::load(&dir);
        let gpt = pricing.lookup(None, "gpt-4o").expect("gpt-4o should be priced");
        println!("gpt-4o: in {} out {}", gpt.i, gpt.o);
        assert!(gpt.i > 0.0 && gpt.o > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

