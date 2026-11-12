//! "モデルを比較": the table of the models (provider, model, price, overall rank, the benchmark of each
//! use, context length, speed), its sort and filters. Data: inocode/models/enrich (what the picker
//! already asked for) and model/list. A value that was not measured is "—", never a 0.
//! Design: ref/docs/63-model-picker-redesign.md.

use crate::models_enrich::{scores_of, Score};
use crate::state::*;
use crate::view::refresh_pickers;
use crate::*;
use std::cell::RefCell;
use std::collections::BTreeSet;

const PRICE_OPTS: [(&str, Option<f64>); 4] = [("すべて", None), ("$1 以下", Some(1.0)), ("$5 以下", Some(5.0)), ("$10 以下", Some(10.0))];
const RANK_OPTS: [(&str, Option<i64>); 4] = [("すべて", None), ("10位以内", Some(10)), ("20位以内", Some(20)), ("30位以内", Some(30))];
const CODE_OPTS: [(&str, Option<i64>); 4] = [("すべて", None), ("5位以内", Some(5)), ("10位以内", Some(10)), ("20位以内", Some(20))];

struct Cmp {
    sort: String,
    desc: bool,
    query: String,
    providers: BTreeSet<String>,
    free_only: bool,
    price: usize,
    rank: usize,
    code: usize,
    picked: Option<(String, String)>,
}

impl Default for Cmp {
    fn default() -> Self {
        Cmp { sort: "r".into(), desc: false, query: String::new(), providers: BTreeSet::new(), free_only: false, price: 0, rank: 0, code: 0, picked: None }
    }
}

thread_local! {
    static CMP: RefCell<Cmp> = RefCell::new(Cmp::default());
}

/// One row with what it is sorted and filtered by.
struct Line {
    provider_id: String,
    id: String,
    provider: String,
    model: String,
    price: String,
    /// The input price in USD per million tokens (free: -1; Theos: above everything), to sort by.
    price_key: Option<f64>,
    free: bool,
    rank: Option<(i64, i64)>,
    general: Option<f64>,
    coding: Option<(f64, i64)>,
    agentic: Option<f64>,
    reasoning: Option<f64>,
    ctx: Option<i64>,
    /// Tokens per second and whether this app measured it
    speed: Option<(f64, bool)>,
}

fn metric<'a>(scores: &'a [Score], name: &str) -> Option<&'a Score> {
    // One metric can be measured in several settings: the best value is the one shown.
    scores.iter().filter(|s| s.metric == name).max_by(|a, b| a.value.partial_cmp(&b.value).unwrap_or(std::cmp::Ordering::Equal))
}

/// 400000 -> "400K", 1000000 -> "1M"
fn ctx_text(c: i64) -> String {
    if c >= 1_000_000 {
        let m = c as f64 / 1_000_000.0;
        if (m - m.round()).abs() < 0.05 { format!("{}M", m.round() as i64) } else { format!("{m:.1}M") }
    } else {
        format!("{}K", (c as f64 / 1000.0).round() as i64)
    }
}

fn dash(v: Option<String>) -> String {
    v.unwrap_or_else(|| "—".to_string())
}

fn lines(st: &State) -> Vec<Line> {
    let mut out = Vec::new();
    for m in visible_models(st) {
        let e = st.enrich.get(&(m.provider.clone(), m.id.clone()));
        let scores = scores_of(st, m);
        let plain = e.map(|e| e.price_plain.clone()).unwrap_or_default();
        let free = plain == "無料";
        let price_key = if free {
            Some(-1.0)
        } else if plain.starts_with("Theos") {
            Some(1e6)
        } else {
            plain.trim_start_matches('$').split(" / ").next().and_then(|v| v.parse::<f64>().ok())
        };
        out.push(Line {
            provider_id: m.provider.clone(),
            id: m.id.clone(),
            provider: crate::settings_page::provider_name(&m.provider),
            model: m.name.clone(),
            price: if plain.is_empty() { "—".into() } else { plain },
            price_key,
            free,
            rank: metric(scores, "aa_intelligence_index").filter(|s| s.rank > 0).map(|s| (s.rank, s.n)),
            general: metric(scores, "aa_intelligence_index").map(|s| s.value),
            coding: metric(scores, "livebench_coding").map(|s| (s.value * 100.0, s.rank)),
            agentic: metric(scores, "livebench_long_agentic").map(|s| s.value * 100.0),
            reasoning: metric(scores, "epoch_gpqa_diamond").or_else(|| metric(scores, "livebench_reasoning")).map(|s| s.value * 100.0),
            ctx: e.and_then(|e| e.ctx),
            // What this app measured goes first; else the benchmark's value.
            speed: e.and_then(|e| e.speed).or_else(|| metric(scores, "or_output_tokens_per_second").map(|s| (s.value, false))),
        });
    }
    out
}

fn price_limit_ok(l: &Line, limit: Option<f64>) -> bool {
    match limit {
        None => true,
        Some(max) => !l.free && l.price_key.map_or(false, |k| k <= max),
    }
}

fn filtered(st: &State, c: &Cmp) -> Vec<Line> {
    let q = c.query.trim().to_lowercase();
    let mut l: Vec<Line> = lines(st)
        .into_iter()
        .filter(|l| q.is_empty() || format!("{} {}", l.model, l.provider).to_lowercase().contains(&q))
        .filter(|l| c.providers.is_empty() || c.providers.contains(&l.provider_id))
        .filter(|l| !c.free_only || l.free)
        .filter(|l| price_limit_ok(l, PRICE_OPTS[c.price].1))
        .filter(|l| RANK_OPTS[c.rank].1.map_or(true, |max| l.rank.map_or(false, |(r, _)| r <= max)))
        .filter(|l| CODE_OPTS[c.code].1.map_or(true, |max| l.coding.map_or(false, |(_, r)| r > 0 && r <= max)))
        .collect();
    let key = c.sort.as_str();
    // A value that was not measured goes last, whatever the direction.
    let num = |l: &Line| -> Option<f64> {
        match key {
            "pr" => l.price_key,
            "r" => l.rank.map(|(r, _)| r as f64),
            "g" => l.general,
            "c" => l.coding.map(|(v, _)| v),
            "a" => l.agentic,
            "s" => l.reasoning,
            "v" => l.speed.map(|(v, _)| v),
            "x" => l.ctx.map(|c| c as f64),
            _ => None,
        }
    };
    l.sort_by(|a, b| {
        let ord = match key {
            "pv" => a.provider.cmp(&b.provider),
            "n" => a.model.to_lowercase().cmp(&b.model.to_lowercase()),
            _ => {
                let (x, y) = (num(a), num(b));
                return match (x, y) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, _) => std::cmp::Ordering::Greater,
                    (_, None) => std::cmp::Ordering::Less,
                    (Some(x), Some(y)) => {
                        let o = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                        if c.desc { o.reverse() } else { o }
                    }
                };
            }
        };
        if c.desc { ord.reverse() } else { ord }
    });
    l
}

/// Put the table on the screen.
pub(crate) fn rebuild(app: &AppWindow) {
    STATE.with(|s| {
        let st = s.borrow();
        CMP.with(|c| {
            let c = c.borrow();
            let rows: Vec<CmpRow> = filtered(&st, &c)
                .into_iter()
                .map(|l| CmpRow {
                    picked: c.picked.as_ref().map_or(false, |(p, i)| *p == l.provider_id && *i == l.id),
                    provider: l.provider.into(),
                    model: l.model.into(),
                    price: l.price.into(),
                    rank: dash(l.rank.map(|(r, n)| format!("{r}位 / {n}"))).into(),
                    general: dash(l.general.map(|v| format!("{v:.1}"))).into(),
                    coding: dash(l.coding.map(|(v, _)| format!("{v:.1}"))).into(),
                    agentic: dash(l.agentic.map(|v| format!("{v:.1}"))).into(),
                    reasoning: dash(l.reasoning.map(|v| format!("{v:.1}"))).into(),
                    ctx: dash(l.ctx.map(ctx_text)).into(),
                    speed: dash(l.speed.map(|(v, _)| format!("{}", v.round() as i64))).into(),
                    speed_measured: l.speed.map_or(false, |(_, m)| m),
                })
                .collect();
            let mut providers: Vec<String> = Vec::new();
            for m in visible_models(&st) {
                if !providers.contains(&m.provider) {
                    providers.push(m.provider.clone());
                }
            }
            let chips: Vec<CmpChip> = providers
                .iter()
                .map(|p| CmpChip { label: crate::settings_page::provider_name(p).into(), on: c.providers.contains(p) })
                .collect();
            let g = app.global::<CompareState>();
            g.set_can_use(c.picked.is_some());
            g.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
            g.set_provider_chips(ModelRc::from(Rc::new(VecModel::from(chips))));
            g.set_free_only(c.free_only);
            g.set_sort_key(c.sort.as_str().into());
            g.set_sort_desc(c.desc);
            g.set_price_current(PRICE_OPTS[c.price].0.into());
            g.set_rank_current(RANK_OPTS[c.rank].0.into());
            g.set_code_current(CODE_OPTS[c.code].0.into());
            g.set_credit(st.enrich_credits.join(" / ").into());
        });
    });
}

/// The data of the table changed (a reply of enrich or model/list): redraw it when it is open.
pub(crate) fn refresh_if_open(app: &AppWindow) {
    if app.global::<CompareState>().get_open() {
        rebuild(app);
    }
}

pub(crate) fn register(app: &AppWindow) {
    let g = app.global::<CompareState>();
    g.set_price_options(ModelRc::from(Rc::new(VecModel::from(PRICE_OPTS.iter().map(|o| SharedString::from(o.0)).collect::<Vec<_>>()))));
    g.set_rank_options(ModelRc::from(Rc::new(VecModel::from(RANK_OPTS.iter().map(|o| SharedString::from(o.0)).collect::<Vec<_>>()))));
    g.set_code_options(ModelRc::from(Rc::new(VecModel::from(CODE_OPTS.iter().map(|o| SharedString::from(o.0)).collect::<Vec<_>>()))));

    let weak = app.as_weak();
    g.on_opened(move || {
        let Some(app) = weak.upgrade() else { return };
        // The chosen model is the one that is marked.
        let cur = STATE.with(|s| {
            let st = s.borrow();
            st.settings.model.clone().map(|m| (st.settings.model_provider.clone().unwrap_or_else(|| "openai".to_string()), m))
        });
        CMP.with(|c| c.borrow_mut().picked = cur);
        crate::models_enrich::request();
        rebuild(&app);
    });
    let weak = app.as_weak();
    g.on_sort(move |key| {
        let Some(app) = weak.upgrade() else { return };
        CMP.with(|c| {
            let mut c = c.borrow_mut();
            if c.sort == key.as_str() {
                c.desc = !c.desc;
            } else {
                c.sort = key.to_string();
                // The values where more is better start with the best.
                c.desc = matches!(key.as_str(), "g" | "c" | "a" | "s" | "x" | "v");
            }
        });
        rebuild(&app);
    });
    let weak = app.as_weak();
    g.on_search(move |q| {
        let Some(app) = weak.upgrade() else { return };
        CMP.with(|c| c.borrow_mut().query = q.to_string());
        rebuild(&app);
    });
    let weak = app.as_weak();
    g.on_toggle_provider(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let p = STATE.with(|s| {
            let st = s.borrow();
            let mut providers: Vec<String> = Vec::new();
            for m in visible_models(&st) {
                if !providers.contains(&m.provider) {
                    providers.push(m.provider.clone());
                }
            }
            providers.get(i as usize).cloned()
        });
        if let Some(p) = p {
            CMP.with(|c| {
                let mut c = c.borrow_mut();
                if !c.providers.remove(&p) {
                    c.providers.insert(p);
                }
            });
        }
        rebuild(&app);
    });
    let weak = app.as_weak();
    g.on_toggle_free(move || {
        let Some(app) = weak.upgrade() else { return };
        CMP.with(|c| {
            let mut c = c.borrow_mut();
            c.free_only = !c.free_only;
        });
        rebuild(&app);
    });
    let weak = app.as_weak();
    g.on_pick_filter(move |kind, i| {
        let Some(app) = weak.upgrade() else { return };
        let i = i.max(0) as usize;
        CMP.with(|c| {
            let mut c = c.borrow_mut();
            match kind.as_str() {
                "price" if i < PRICE_OPTS.len() => c.price = i,
                "rank" if i < RANK_OPTS.len() => c.rank = i,
                "code" if i < CODE_OPTS.len() => c.code = i,
                _ => {}
            }
        });
        rebuild(&app);
    });
    let weak = app.as_weak();
    g.on_row_clicked(move |i| {
        let Some(app) = weak.upgrade() else { return };
        // The row of the table as it is shown now.
        let picked = STATE.with(|s| {
            let st = s.borrow();
            CMP.with(|c| filtered(&st, &c.borrow()).get(i as usize).map(|l| (l.provider_id.clone(), l.id.clone())))
        });
        if picked.is_some() {
            CMP.with(|c| c.borrow_mut().picked = picked);
            rebuild(&app);
        }
    });
    let weak = app.as_weak();
    g.on_use_picked(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some((provider, id)) = CMP.with(|c| c.borrow().picked.clone()) else { return };
        // Choosing it is a click on its row of the picker (the section is opened for that).
        let index = STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.provider_open.insert(provider.clone(), true);
            shown_models(&st).iter().position(|it| matches!(it, Item::Model(m, Kind::Normal) if m.id == id && m.provider == provider))
        });
        if let Some(i) = index {
            app.global::<AppState>().invoke_pick_model(i as i32);
        }
        app.global::<CompareState>().set_open(false);
        STATE.with(|s| refresh_pickers(&app, &s.borrow()));
    });
}
