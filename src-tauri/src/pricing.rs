// Model pricing lookup.
// Priority: local agent catalogs (exact prices for the user's own providers)
//   -> embedded LiteLLM catalog (exact) -> normalized/suffix match -> family fallback.
// Prices are USD per 1M tokens.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const EMBEDDED: &str = include_str!("../prices.json");

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub i: f64,
    pub o: f64,
    pub cr: f64,
    pub cw: f64,
}

impl Price {
    pub fn cost(&self, input: u64, cache_read: u64, cache_write: u64, output: u64) -> f64 {
        (input as f64 * self.i + cache_read as f64 * self.cr + cache_write as f64 * self.cw + output as f64 * self.o) / 1e6
    }
    /// What caching saved versus sending the same tokens as plain input.
    pub fn savings(&self, cache_read: u64, cache_write: u64) -> f64 {
        let read = if self.cr > 0.0 { self.cr } else { self.i };
        let write = if self.cw > 0.0 { self.cw } else { self.i };
        (cache_read as f64 * (self.i - read) + cache_write as f64 * (self.i - write)).max(0.0) / 1e6
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
    local_conflicts: std::collections::HashSet<String>,
    pub source: String,
    pub embedded_count: usize,
    pub local_count: usize,
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
    for pat in [":free", "@free", "-latest", "-preview"] {
        if let Some(i) = base.rfind(pat) {
            base.truncate(i);
        }
    }
    // strip trailing date digits group like -20250929 or -v2
    let bytes: Vec<char> = base.chars().collect();
    let mut cut = bytes.len();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i < bytes.len() && i > 0 && bytes[i - 1] == '-' {
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

impl Pricing {
    pub fn load(home: &std::path::Path) -> Pricing {
        let parsed: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap_or(serde_json::Value::Null);
        let mut embedded = HashMap::new();
        let mut embedded_tail: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(models) = parsed.get("models").and_then(|m| m.as_object()) {
            for (k, v) in models {
                let p = Price {
                    i: v.get("i").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    o: v.get("o").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    cr: v.get("cr").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    cw: v.get("cw").and_then(|x| x.as_f64()).unwrap_or(0.0),
                };
                embedded.insert(k.to_lowercase(), p);
                embedded_tail.entry(tail(k)).or_default().push(k.to_lowercase());
            }
        }
        let source = parsed
            .get("meta")
            .and_then(|m| m.get("source"))
            .and_then(|x| x.as_str())
            .unwrap_or("LiteLLM")
            .to_string();

        let mut local = HashMap::new();
        let mut local_conflicts = load_pi_catalog(&home.join(".pi").join("agent").join("models-store.json"), &mut local);
        let mut cline = HashMap::new();
        let cline_conflicts = load_clinepass(&home.join(".pi").join("agent").join("clinepass-prices.json"), &mut cline);
        local_conflicts.extend(cline_conflicts);
        // a bare id is only usable when all providers quote the same price
        let mut local_bare: HashMap<String, Price> = HashMap::new();
        for (k, v) in local.iter().filter(|(k, _)| !k.contains('/')) {
            local_bare.insert(k.clone(), *v);
        }

        Pricing {
            embedded_count: embedded.len(),
            local_count: local.len(),
            embedded,
            embedded_tail,
            local_by_provider: local,
            local_bare,
            local_conflicts,
            source,
        }
    }

    fn from_local(&self, provider: Option<&str>, model: &str) -> Option<Price> {
        let m = model.to_lowercase();
        let t = tail(&m);
        match provider {
            // a known provider is priced strictly by its own catalog entry
            Some(p) => {
                for cand in provider_candidates(p) {
                    if let Some(pr) = self
                        .local_by_provider
                        .get(&format!("{}/{}", cand, t))
                    {
                        return Some(*pr);
                    }
                }
                None
            }
            // unknown provider: a bare id is fine, unless providers disagree about it
            None => {
                if self.local_conflicts.contains(&t) {
                    None
                } else {
                    self.local_bare.get(&t).copied()
                }
            }
        }
    }

    fn from_embedded(&self, model: &str) -> Option<Price> {
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

    /// Price a request. `provider` is the route that actually served it; when it is
    /// known we never fall back to another provider's price for the same model name,
    /// because subscription routes (Zed AI, Antigravity plans, contributor tiers)
    /// are not billed per token.
    pub fn lookup(&self, provider: Option<&str>, model: &str) -> Option<Price> {
        if model.is_empty() || model == "unknown" {
            return None;
        }
        if let Some(p) = self.from_local(provider, model) {
            return Some(p);
        }
        // free / contributor routes genuinely cost nothing, whatever the provider
        let m = model.to_lowercase();
        if is_free_route(&m) {
            return Some(Price { i: 0.0, o: 0.0, cr: 0.0, cw: 0.0 });
        }
        // Public list prices are only a fair stand-in for providers that actually bill
        // per token. A subscription route (zed-ai, antigravity, ...) is covered by its
        // plan, so pricing it from a same-named OpenAI/Anthropic model would be fiction.
        let mirror = match provider {
            None => true,
            Some(p) => provider_candidates(p)
                .iter()
                .any(|c| MIRROR_PROVIDERS.contains(&c.as_str())),
        };
        if !mirror {
            return None;
        }
        self.from_embedded(model).or_else(|| self.family(model))
    }
}

/// Providers that bill per token through the public price lists.
const MIRROR_PROVIDERS: &[&str] = &[
    "openai", "anthropic", "google", "gemini", "vertex_ai-language-models", "azure",
    "openrouter", "deepseek", "mistral", "xai", "groq", "together_ai", "fireworks_ai",
    "bedrock", "ollama", "lm_studio",
];

/// "opencode-go-responses" and "opencode-go" are the same Zen route with a different
/// API shape, and "mimo" is xiaomi's vendor prefix. Try these aliases in order.
fn provider_candidates(provider: &str) -> Vec<String> {
    let p = provider.to_lowercase();
    let mut out = vec![p.clone()];
    for suffix in ["-responses", "-free-responses", "-chat", "-go", "-zen", "-free"] {
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
    Some(Price { i, o, cr, cw })
}

/// ~/.pi/agent/models-store.json : { provider: { models: [ { id, cost: {...} } ] } }
/// (older builds used an object keyed by model id; both shapes are accepted)
/// Returns the set of model ids that different providers price differently.
fn load_pi_catalog(path: &std::path::Path, out: &mut HashMap<String, Price>) -> std::collections::HashSet<String> {
    let mut conflicts = std::collections::HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else { return conflicts };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return conflicts };
    let Some(providers) = v.as_object() else { return conflicts };
    for (provider, pv) in providers {
        let Some(models) = pv.get("models") else { continue };
        let mut insert = |id: &str, mv: &serde_json::Value| {
            let Some(cost) = mv.get("cost") else { return };
            if let Some(p) = price_from_json(cost) {
                let idl = id.to_lowercase();
                let key = format!("{}/{}", provider.to_lowercase(), idl);
                // remember when two providers disagree about the same model id
                if let Some(prev) = out.get(&idl) {
                    if prev.i != p.i || prev.o != p.o {
                        conflicts.insert(idl.clone());
                    }
                }
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
    conflicts
}

/// ~/.pi/agent/clinepass-prices.json : { models: { key: { input, output, cacheRead } } }
fn load_clinepass(path: &std::path::Path, out: &mut HashMap<String, Price>) -> std::collections::HashSet<String> {
    let mut conflicts = std::collections::HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else { return conflicts };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return conflicts };
    let Some(models) = v.get("models").and_then(|m| m.as_object()) else { return conflicts };
    for (id, mv) in models {
        if let Some(p) = price_from_json(mv) {
            let key = id.to_lowercase();
            let t = tail(&key);
            if let Some(prev) = out.get(&t) {
                if prev.i != p.i || prev.o != p.o {
                    conflicts.insert(t.clone());
                }
            }
            out.insert(key, p);
            out.insert(t, p);
        }
    }
    conflicts
}
