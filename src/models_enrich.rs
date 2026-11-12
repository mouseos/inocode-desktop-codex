//! What the model picker tells about a model besides its name: how it ranks in the benchmarks of
//! a use (coding, reasoning, ...), and its price (the list price, a price the user set, or what
//! was measured; never mixed). Contract: inocode/models/enrich (ref/docs/16, "追加 10").
//! Benchmark data is shown with the attribution the sources ask for.

use crate::state::*;
use crate::view::refresh_pickers;
use crate::*;
use serde_json::{json, Value};
use std::collections::HashMap;

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    /// The models the last request was made for (a new request only when they change).
    static ASKED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn register(handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Score {
    pub(crate) use_: String,
    pub(crate) value: f64,
    pub(crate) unit: String,
    pub(crate) metric: String,
    pub(crate) rank: i64,
    pub(crate) n: i64,
}

/// What is known of one model.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Enrich {
    pub(crate) scores: Vec<Score>,
    /// "参考 $10 / $50", "設定 …" or "実測 …"; empty when no price is known.
    pub(crate) price_text: String,
    /// The output price in USD per million tokens, to put the cheap first.
    pub(crate) price_out: Option<f64>,
}

/// The uses of the filter, in the order shown: (key, label). "price" is the order by price.
const USES: [(&str, &str); 8] = [
    ("coding", "コーディング"),
    ("reasoning", "推論"),
    ("math", "数学"),
    ("agentic", "エージェント"),
    ("long_context", "長文"),
    ("general", "総合順位"),
    ("speed", "速度"),
    ("price", "価格"),
];

pub(crate) fn use_label(key: &str) -> &'static str {
    USES.iter().find(|u| u.0 == key).map(|u| u.1).unwrap_or("")
}

/// The uses that can be chosen: those with a metric in the use map, and the price.
pub(crate) fn offered_uses(st: &State) -> Vec<&'static str> {
    USES.iter().filter(|u| u.0 == "price" || st.use_map.iter().any(|k| k == u.0)).map(|u| u.0).collect()
}

/// The labels of the chips: "すべて", then the uses that can be chosen.
pub(crate) fn use_labels(st: &State) -> Vec<String> {
    std::iter::once("すべて".to_string()).chain(offered_uses(st).iter().map(|u| use_label(u).to_string())).collect()
}

fn money(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn amount(p: &Value, key: &str) -> Option<f64> {
    p.get(key).and_then(Value::as_f64)
}

/// The price line: the user's own price first, then what was measured, then the list price.
fn price_of(price: &Value) -> (String, Option<f64>) {
    let line = |label: &str, p: &Value, suffix: &str| -> Option<(String, Option<f64>)> {
        let (i, o) = (amount(p, "input"), amount(p, "output"));
        if i.is_none() && o.is_none() {
            return None;
        }
        let parts: Vec<String> = [i, o].iter().flatten().map(|v| format!("${}", money(*v))).collect();
        Some((format!("{label} {}{suffix}", parts.join(" / ")), o.or(i)))
    };
    if let Some(r) = price.get("userOverride").filter(|v| v.is_object()).and_then(|p| line("設定", p, "")) {
        return r;
    }
    if let Some(m) = price.get("measured").filter(|v| v.is_object()) {
        let upper = m.get("upperBound").and_then(Value::as_bool).unwrap_or(false);
        if let Some(r) = line("実測", m, if upper { "(上限)" } else { "" }) {
            return r;
        }
    }
    if let Some(r) = price.get("reference").filter(|v| v.get("kind").and_then(Value::as_str) == Some("per_token_list")) {
        if let Some(x) = line("参考", r, "") {
            return x;
        }
    }
    (String::new(), None)
}

/// The enrich reply as what the picker keeps: per model, and the attributions to show.
pub(crate) fn parse(v: &Value) -> (HashMap<(String, String), Enrich>, Vec<String>, Vec<String>) {
    let mut out = HashMap::new();
    for m in v.get("models").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        let key = (
            m.get("provider").and_then(Value::as_str).unwrap_or("").to_string(),
            m.get("modelId").and_then(Value::as_str).unwrap_or("").to_string(),
        );
        let scores = m
            .get("scores")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some(Score {
                            use_: s.get("use")?.as_str()?.to_string(),
                            value: s.get("value")?.as_f64()?,
                            unit: s.get("unit").and_then(Value::as_str).unwrap_or("").to_string(),
                            metric: s.get("metric").and_then(Value::as_str).unwrap_or("").to_string(),
                            rank: s.get("rank").and_then(Value::as_i64).unwrap_or(0),
                            n: s.get("nModelsInMetric").and_then(Value::as_i64).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (mut price_text, mut price_out) = m.get("price").map(price_of).unwrap_or_default();
        // DeepAI (Theos) and free models: only "free" or the Theos a request costs. A list price or a
        // measured $ is for the models that are paid per token, so it is not shown for these.
        let billing = m.pointer("/price/billing").and_then(Value::as_str).unwrap_or("");
        if let Some(t) = m.get("theos").filter(|t| t.is_object()) {
            price_text = if t.get("isFree").and_then(Value::as_bool).unwrap_or(false) {
                "無料".to_string()
            } else {
                match t.get("theosPerRequest").and_then(Value::as_str) {
                    Some(n) => format!("Theos {n} / 回"),
                    None => "Theos 消費".to_string(),
                }
            };
            price_out = None;
        } else if billing == "free" {
            price_text = "無料".to_string();
            price_out = None;
        } else if billing == "theos" {
            price_text = "Theos 消費".to_string();
            price_out = None;
        }
        out.insert(key, Enrich { scores, price_text, price_out });
    }
    let credits = v
        .get("sources")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.get("attribution").and_then(Value::as_str)).filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    // The uses that have a metric.
    let uses = v
        .get("useMap")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter(|(_, l)| l.as_array().map(|a| !a.is_empty()).unwrap_or(false)).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default();
    (out, credits, uses)
}

/// The name of a model without what differs between providers' spellings (case, "." or "-").
fn base_name(id: &str) -> String {
    id.to_lowercase().replace('.', "-")
}

/// The scores of a model: its own, else those of the same base model through another provider (the
/// ranking is of the model, not of the provider it runs on). The price stays the provider's own.
pub(crate) fn scores_of<'a>(st: &'a State, m: &ModelInfo) -> &'a [Score] {
    if let Some(e) = st.enrich.get(&(m.provider.clone(), m.id.clone())) {
        if !e.scores.is_empty() {
            return &e.scores;
        }
    }
    let name = base_name(&m.id);
    let mut keys: Vec<&(String, String)> = st.enrich.keys().filter(|k| base_name(&k.1) == name).collect();
    keys.sort();
    keys.iter().map(|k| &st.enrich[*k].scores).find(|s| !s.is_empty()).map(Vec::as_slice).unwrap_or(&[])
}

/// The best rank of a model in a use (lower is better), None when it was not measured.
pub(crate) fn rank_in(e: &Enrich, use_: &str) -> Option<i64> {
    e.scores.iter().filter(|s| s.use_ == use_ && s.rank > 0).map(|s| s.rank).min()
}

/// The order key of a model for the use that is chosen: the best first, the unknown last.
pub(crate) fn order_key(st: &State, m: &ModelInfo) -> f64 {
    let Some(use_) = st.use_filter.as_deref() else { return 0.0 };
    let e = st.enrich.get(&(m.provider.clone(), m.id.clone()));
    let key = if use_ == "price" {
        e.and_then(|e| e.price_out)
    } else {
        scores_of(st, m).iter().filter(|s| s.use_ == use_ && s.rank > 0).map(|s| s.rank).min().map(|r| r as f64)
    };
    key.unwrap_or(f64::MAX)
}

fn value_text(s: &Score) -> String {
    match s.unit.as_str() {
        "ratio" => format!("{:.1}%", s.value * 100.0),
        _ if s.metric.contains("per_second") => format!("{} トークン/秒", s.value.round() as i64),
        _ => format!("{}", (s.value * 10.0).round() / 10.0),
    }
}

/// The second line of a model's row: how it ranks for the chosen use, and the price.
pub(crate) fn describe(st: &State, m: &ModelInfo) -> String {
    let own = st.enrich.get(&(m.provider.clone(), m.id.clone()));
    let scores = scores_of(st, m);
    if own.is_none() && scores.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    // The order of "総合順位" mixes the providers: the row says which provider it runs on.
    if st.use_filter.as_deref() == Some("general") {
        parts.push(crate::settings_page::provider_name(&m.provider));
    }
    if let Some(u) = st.use_filter.as_deref().filter(|u| *u != "price") {
        let best = scores.iter().filter(|s| s.use_ == u && s.rank > 0).min_by_key(|s| s.rank);
        parts.push(match best {
            Some(s) => format!("{} {}位 / {} · {}", use_label(u), s.rank, s.n, value_text(s)),
            None => format!("{} 未測定", use_label(u)),
        });
    }
    // The overall rank (the best of the metrics of "general"), on every row that has one.
    if st.use_filter.as_deref() != Some("general") {
        if let Some(s) = scores.iter().filter(|s| s.use_ == "general" && s.rank > 0).min_by_key(|s| s.rank) {
            parts.push(format!("総合 {}位 / {}", s.rank, s.n));
        }
    }
    if let Some(e) = own.filter(|e| !e.price_text.is_empty()) {
        parts.push(e.price_text.clone());
    }
    parts.join(" · ")
}

/// Ask for what is known of the models of the picker (when they changed).
pub(crate) fn request() {
    let models: Vec<(String, String)> = STATE.with(|s| {
        let st = s.borrow();
        visible_models(&st).iter().map(|m| (m.provider.clone(), m.id.clone())).collect()
    });
    if models.is_empty() || ASKED.with(|a| *a.borrow() == models) {
        return;
    }
    ASKED.with(|a| *a.borrow_mut() = models.clone());
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        let list: Vec<Value> = models.iter().map(|(p, m)| json!({ "provider": p, "modelId": m })).collect();
        h.send(Command::Rpc { tag: "model.enrich".into(), method: "inocode/models/enrich".into(), params: json!({ "models": list }) });
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    if tag != "model.enrich" {
        return;
    }
    // INOCODE_GUI_RPC_LOG=<file>: what the server answered.
    if let Ok(path) = std::env::var("INOCODE_GUI_RPC_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "enrich reply {}", match result { Ok(v) => v.to_string().chars().take(2500).collect::<String>(), Err(e) => format!("error {e}") });
        }
    }
    // A server that does not know the call: the picker stays as it is.
    let Ok(v) = result else { return };
    let (map, credits, uses) = parse(v);
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        st.enrich = map;
        st.enrich_credits = credits;
        st.use_map = uses;
        refresh_pickers(app, &st);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_price_of_the_user_goes_before_the_list_price() {
        let p = json!({
            "reference": { "kind": "per_token_list", "input": 10.0, "output": 50.0 },
            "userOverride": { "input": 7.0, "output": 33.0 },
            "billing": "plan_included"
        });
        assert_eq!(price_of(&p), ("設定 $7 / $33".to_string(), Some(33.0)));
        let q = json!({ "reference": { "kind": "per_token_list", "input": 0.1, "output": 0.075 } });
        assert_eq!(price_of(&q).0, "参考 $0.1 / $0.075");
        let unknown = json!({ "reference": { "kind": "unknown", "reason": "not_listed" } });
        assert_eq!(price_of(&unknown), (String::new(), None));
    }

    #[test]
    fn a_free_or_theos_model_shows_no_dollar_price() {
        let v = json!({ "models": [
            { "provider": "deepai", "modelId": "a", "scores": [],
              "theos": { "isFree": true },
              "price": { "reference": { "kind": "per_token_list", "input": 1.0, "output": 2.0 }, "measured": { "input": 1.0, "output": 2.0, "upperBound": false }, "billing": "theos" } },
            { "provider": "deepai", "modelId": "b", "scores": [],
              "theos": { "isFree": false, "theosPerRequest": "3" },
              "price": { "reference": { "kind": "per_token_list", "input": 1.0, "output": 2.0 }, "billing": "theos" } },
            { "provider": "x", "modelId": "c", "scores": [],
              "price": { "reference": { "kind": "per_token_list", "input": 1.0, "output": 2.0 }, "billing": "free" } } ] });
        let (map, _, _) = parse(&v);
        let get = |p: &str, m: &str| map[&(p.to_string(), m.to_string())].clone();
        assert_eq!((get("deepai", "a").price_text.as_str(), get("deepai", "a").price_out), ("無料", None));
        assert_eq!((get("deepai", "b").price_text.as_str(), get("deepai", "b").price_out), ("Theos 3 / 回", None));
        assert_eq!((get("x", "c").price_text.as_str(), get("x", "c").price_out), ("無料", None));
    }

    #[test]
    fn unmeasured_has_no_scores_and_the_rank_is_the_best_of_a_use() {
        let v = json!({ "models": [
            { "provider": "openai", "modelId": "a", "scores": [
                { "use": "coding", "metric": "m1", "value": 0.8, "unit": "ratio", "nModelsInMetric": 30, "rank": 4 },
                { "use": "coding", "metric": "m2", "value": 70.0, "unit": "points", "nModelsInMetric": 20, "rank": 2 } ],
              "price": { "reference": { "kind": "unknown" }, "billing": "unknown" } },
            { "provider": "openai", "modelId": "b", "scores": [], "price": { "reference": { "kind": "unknown" }, "billing": "unknown" } } ],
            "sources": [ { "name": "x", "attribution": "A" }, { "name": "y" } ],
            "useMap": { "coding": ["m1"], "long_context": [] } });
        let (map, credits, uses) = parse(&v);
        let a = &map[&("openai".to_string(), "a".to_string())];
        assert_eq!(rank_in(a, "coding"), Some(2));
        assert_eq!(rank_in(&map[&("openai".to_string(), "b".to_string())], "coding"), None);
        assert_eq!(credits, vec!["A".to_string()]);
        assert_eq!(uses, vec!["coding".to_string()]);
    }
}
