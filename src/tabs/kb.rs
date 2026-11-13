//! 知識と根拠の作り直し(doc 60、契約は doc 61)の画面: 知識タブの概要・禁止事項・失敗の記録・共通知識、
//! 根拠タブのページ・引用、右の side panel のチェックリスト。値はすべて inocode/kb/* から取る。
//! ID(`proh-0001` など)は操作の相手を指す不透明な文字列で、画面には出さない。
//! UI: ui/tabs/kb.slint

use super::Host;
use crate::client::Event;
use crate::{AppWindow, KbAct, KbCheck, KbHist, KbKv, KbQuote, KbRow, KbSection, KbState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const SUB_OVERVIEW: i32 = 0;
const SUB_PROHIB: i32 = 1;
const SUB_FAIL: i32 = 2;
const SUB_COMMON: i32 = 3;
const SUB_PAGES: i32 = 10;
const SUB_QUOTES: i32 = 11;
/// 引用の一覧のために読むページの数の上限
const QUOTE_PAGES: usize = 40;

#[derive(Default)]
struct K {
    project: String,
    overview_text: String,
    overview_rev: String,
    index: Value,
    prohib: Vec<Value>,
    /// 承認済みの失敗の記録と、承認待ちの候補(すべての種類)
    failures: Vec<Value>,
    cands: Vec<Value>,
    pages: Vec<Value>,
    /// 読んだページ(題名 -> page/read の応答)
    read: Vec<(String, Value)>,
    tags: Vec<String>,
    /// 選んでいる行(-1: なし)と、旧規則の選んでいる行
    sel: i32,
    sel_title: Option<String>,
    extra_sel: i32,
    /// 概要の区画の行の行き先
    section_kinds: Vec<&'static str>,
    section_jumps: Vec<Vec<i32>>,
    role: String,
    roles: Vec<String>,
    items: Vec<Value>,
    /// 保存で競合したときの現在の版
    pending_rev: Option<String>,
    /// カードが指した記録(題): 一覧を読んだら、これを開く
    pending_ref: Option<String>,
}

thread_local! {
    static K: RefCell<K> = RefCell::new(K { sel: -1, extra_sel: -1, ..K::default() });
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ pure helpers (tested)

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn payload(v: &Value) -> &Value {
    v.get("response").unwrap_or(v)
}

pub(crate) fn strength_label(w: &str) -> &'static str {
    match w {
        "warn" | "warning" => "警告",
        _ => "止める",
    }
}

fn scope_label(w: &str) -> &'static str {
    match w {
        "session" => "このセッション",
        "until" => "期限まで",
        "always" => "取り消すまで",
        _ => "この依頼の間",
    }
}

fn effect_label(w: &str) -> &'static str {
    match w {
        "warnOnly" => "警告にする",
        "disable" => "無効にする",
        _ => "この条件の行為だけ許す",
    }
}

/// "2026-11-12T03:10:00Z" -> "2026-11-12 03:10"(秒より先を落とす)
fn stamp(t: &str) -> String {
    let t = t.replace('T', " ");
    t.chars().take(16).collect()
}

/// 条件(match)を 1 行の文にする: commands などの値を並べる("re:" の印は外す)
fn match_text(m: &Value) -> String {
    let Some(obj) = m.as_object() else { return String::new() };
    let mut parts: Vec<String> = Vec::new();
    for (_, v) in obj {
        match v {
            Value::Array(a) => parts.extend(a.iter().filter_map(Value::as_str).map(|x| x.strip_prefix("re:").unwrap_or(x).to_string())),
            Value::String(x) => parts.push(x.strip_prefix("re:").unwrap_or(x).to_string()),
            _ => {}
        }
    }
    parts.join("、")
}

fn relaxed_now(p: &Value) -> bool {
    p.get("relaxations").and_then(Value::as_array).is_some_and(|a| a.iter().any(|r| r.get("active").and_then(Value::as_bool).unwrap_or(false)))
}

/// A line of a list is one line: breaks and runs of blanks become a single space.
fn one_line(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "unknown" is written by the model when it does not know the condition: the screen says so.
fn cond_text(c: &str) -> String {
    let t = c.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("unknown") || t == "条件不明" { "不明".to_string() } else { t.to_string() }
}

fn cand_kind_label(c: &Value) -> String {
    let base = match s(c, "kind") {
        "failureCase" | "failure" => "失敗の記録",
        "checklistItem" => "チェックリストの項目",
        "overviewAppend" => "概要への追記",
        "page" => "ページ",
        "fact" => "事実",
        _ => "",
    };
    if s(c, "op") == "update" { format!("{base} · 更新の提案") } else { base.to_string() }
}

/// 候補(承認待ち)の行
fn cand_rows(list: &[Value], sel: i32) -> Vec<KbRow> {
    list.iter().enumerate().map(|(i, c)| row(s(c, "title"), &cand_kind_label(c), "承認待ち", 1, i as i32 == sel)).collect()
}

/// 失敗の記録(承認済み・候補)の欄: (見出し, 値)。更新の提案は、前の中身があれば「前 → 後」で出す
fn failure_fields(rec: &Value, current: Option<&Value>) -> Vec<(&'static str, String)> {
    let get = |v: &Value, k: &str, k2: &str| -> String {
        let t = s(v, k);
        if t.is_empty() { s(v, k2).to_string() } else { t.to_string() }
    };
    let fields: [(&'static str, &str, &str); 7] = [
        ("条件", "conditions", "conditions"),
        ("実行したこと", "attempted", "attempted"),
        ("結果", "outcome", "outcome"),
        ("回避・予防", "avoid", "avoid"),
        ("原因", "cause", "cause"),
        ("出典", "source", "source"),
        ("確認日", "checked_at", "checkedAt"),
    ];
    let mut out = Vec::new();
    for (label, k, k2) in fields {
        let new = get(rec, k, k2);
        let old = current.map(|c| get(c, k, k2)).unwrap_or_default();
        let show = |t: &str| if label == "条件" { cond_text(t) } else { t.to_string() };
        if !new.is_empty() && !old.is_empty() && new != old {
            out.push((label, format!("{}\n→ {}", show(&old), show(&new))));
        } else if !new.is_empty() {
            out.push((label, show(&new)));
        } else if label == "条件" {
            // a failure with no condition at all is shown as unknown, not left blank
            out.push((label, "不明".to_string()));
        }
    }
    out
}

fn row(title: &str, sub: &str, end: &str, tone: i32, selected: bool) -> KbRow {
    KbRow { title: one_line(title).into(), sub: one_line(sub).into(), end: end.into(), end_tone: tone, selected }
}

fn prohib_rows(list: &[Value], sel: i32) -> Vec<KbRow> {
    list.iter()
        .enumerate()
        .map(|(i, p)| {
            let end = if relaxed_now(p) { "緩和中" } else { strength_label(s(p, "strength")) };
            row(s(p, "title"), s(p, "reason"), end, 0, i as i32 == sel)
        })
        .collect()
}

/// 緩和の履歴の行
fn hist_rows(p: &Value) -> Vec<KbHist> {
    p.get("relaxations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .rev()
                .map(|r| {
                    let revoked = r.get("revokedAt").is_some_and(|x| !x.is_null());
                    let mut meta = format!("{} · {} · {}", stamp(s(r, "at")), scope_label(s(r, "scope")), effect_label(s(r, "effect")));
                    if let Some(allow) = r.get("allow").map(match_text).filter(|t| !t.is_empty()) {
                        meta.push_str(&format!(" · {allow}"));
                    }
                    if revoked {
                        meta.push_str(" · 取り消し済み");
                    }
                    KbHist {
                        text: format!("「{}」", s(r, "quote")).into(),
                        meta: meta.into(),
                        can_revoke: !revoked && r.get("active").and_then(Value::as_bool).unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 本文を段落に分ける: 引用の行(`> quote:`)は引用として別に出すので除く
fn paragraphs(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line.trim_start().starts_with("> quote:") {
            continue;
        }
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out
}

fn page_rows(list: &[Value], sel_title: &Option<String>) -> Vec<KbRow> {
    list.iter()
        .map(|p| {
            let confirmed = p.get("confirmed").and_then(Value::as_bool).unwrap_or(false);
            let selected = sel_title.as_deref() == Some(s(p, "title"));
            row(s(p, "title"), s(p, "snippet"), if confirmed { "" } else { "未確認" }, if confirmed { 0 } else { 1 }, selected)
        })
        .collect()
}

/// ページの引用(読んだページから): (ページの題名, 引用)
fn all_quotes(read: &[(String, Value)]) -> Vec<(String, Value)> {
    read.iter().flat_map(|(t, v)| v.get("quotes").and_then(Value::as_array).into_iter().flatten().map(move |q| (t.clone(), q.clone()))).collect()
}

/// チェックリストの 1 項目
fn check_row(it: &Value) -> KbCheck {
    let check = s(it, "check");
    let done = it.get("done").and_then(Value::as_bool).unwrap_or(false);
    KbCheck {
        text: s(it, "text").into(),
        // 自己申告は、そうだと見える(自己申告の判定はほぼ偶然水準)
        detail: if check == "self" && done { "自己申告".into() } else { SharedString::default() },
        done,
        can_toggle: check != "machine",
    }
}

/// 概要の本文: 先頭の「# 題」を題に、残りを段落に
fn overview_parts(text: &str, project: &str) -> (String, Vec<String>) {
    let mut title = project.to_string();
    let mut rest = String::new();
    let mut first = true;
    for line in text.lines() {
        if first && line.starts_with('#') && !line.starts_with("#タグ") {
            title = line.trim_start_matches('#').trim().to_string();
        } else {
            rest.push_str(line);
            rest.push('\n');
        }
        first = false;
    }
    (title, paragraphs(&rest))
}

// ------------------------------------------------------------------ screen

fn cwd(app: &AppWindow) -> String {
    let c = app.global::<crate::AppState>().get_cwd_text().to_string();
    if !c.is_empty() {
        return c;
    }
    std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn strs(v: Vec<String>) -> ModelRc<SharedString> {
    model(v.into_iter().map(SharedString::from).collect())
}

fn clear_detail(st: &KbState<'_>) {
    st.set_has_detail(false);
    st.set_d_title("".into());
    st.set_d_meta("".into());
    st.set_d_badge("".into());
    st.set_d_tags(ModelRc::default());
    st.set_d_body(ModelRc::default());
    st.set_d_kv(ModelRc::default());
    st.set_d_quotes(ModelRc::default());
    st.set_d_hist(ModelRc::default());
    st.set_d_links(ModelRc::default());
    st.set_d_backlinks(ModelRc::default());
    st.set_d_acts(ModelRc::default());
}

fn act(id: &str, label: &str, primary: bool) -> KbAct {
    KbAct { id: id.into(), label: label.into(), primary }
}

fn render(app: &AppWindow) {
    let st = app.global::<KbState>();
    let sub = st.get_sub();
    let mut meta: Option<(Vec<&'static str>, Vec<Vec<i32>>)> = None;
    K.with(|k| {
        let k = k.borrow();
        match sub {
            SUB_OVERVIEW => meta = Some(render_overview(app, &k)),
            SUB_PROHIB => {
                st.set_can_new(true);
                st.set_new_label("新規".into());
                st.set_search_hint("知識を検索".into());
                st.set_tags(ModelRc::default());
                st.set_rows(model(prohib_rows(&k.prohib, k.sel)));
                st.set_extra_heading("".into());
                st.set_extra_rows(ModelRc::default());
                match usize::try_from(k.sel).ok().and_then(|i| k.prohib.get(i)) {
                    Some(p) => detail_prohib(&st, p),
                    None => clear_detail(&st),
                }
            }
            SUB_FAIL => {
                st.set_can_new(false);
                st.set_search_hint("知識を検索".into());
                st.set_tags(ModelRc::default());
                // 承認済みの記録 / 承認待ちの候補 / 旧データ(読み取り専用)
                let rows: Vec<KbRow> = k
                    .failures
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let confirmed = matches!(s(f, "status"), "confirmed" | "verified" | "registered" | "");
                        row(s(f, "title"), &cond_text(if s(f, "conditions").is_empty() { "unknown" } else { s(f, "conditions") }), if confirmed { "" } else { "未確認" }, if confirmed { 0 } else { 1 }, i as i32 == k.sel)
                    })
                    .collect();
                st.set_rows(model(rows));
                st.set_extra_heading(if k.cands.is_empty() { "" } else { "承認待ち" }.into());
                st.set_extra_rows(model(cand_rows(&k.cands, k.extra_sel)));
                if let Some(c) = usize::try_from(k.extra_sel).ok().and_then(|i| k.cands.get(i)) {
                    detail_candidate(&st, c);
                } else if let Some(f) = usize::try_from(k.sel).ok().and_then(|i| k.failures.get(i)) {
                    clear_detail(&st);
                    st.set_has_detail(true);
                    st.set_d_title(s(f, "title").into());
                    let confirmed = matches!(s(f, "status"), "confirmed" | "verified" | "registered" | "");
                    if !confirmed {
                        st.set_d_badge("未確認".into());
                        st.set_d_badge_tone(1);
                    }
                    st.set_d_body(strs(if s(f, "body").is_empty() { vec![] } else { vec![s(f, "body").to_string()] }));
                    st.set_d_kv(model(failure_fields(f, None).into_iter().map(|(a, b)| KbKv { k: a.into(), v: b.into() }).collect()));
                } else {
                    clear_detail(&st);
                }
            }
            SUB_COMMON => {
                st.set_can_new(false);
                st.set_tags(ModelRc::default());
                st.set_rows(ModelRc::default());
                st.set_extra_rows(ModelRc::default());
                clear_detail(&st);
            }
            SUB_PAGES => {
                st.set_can_new(true);
                st.set_new_label("新規".into());
                st.set_search_hint("資料を検索".into());
                st.set_extra_rows(ModelRc::default());
                st.set_tags(strs(k.tags.iter().map(|t| format!("#{t}")).collect()));
                st.set_rows(model(page_rows(&k.pages, &k.sel_title)));
                let cur = k.sel_title.as_ref().and_then(|t| k.read.iter().find(|(x, _)| x == t));
                match cur {
                    Some((_, p)) => detail_page(&st, p),
                    None => clear_detail(&st),
                }
            }
            SUB_QUOTES => {
                st.set_can_new(false);
                st.set_search_hint("資料を検索".into());
                st.set_tags(ModelRc::default());
                st.set_extra_rows(ModelRc::default());
                let quotes = all_quotes(&k.read);
                let rows: Vec<KbRow> = quotes.iter().enumerate().map(|(i, (_, q))| row(s(q, "text"), s(q, "sourceTitle"), "", 0, i as i32 == k.sel)).collect();
                st.set_rows(model(rows));
                match usize::try_from(k.sel).ok().and_then(|i| quotes.get(i)) {
                    Some((page, q)) => {
                        clear_detail(&st);
                        st.set_has_detail(true);
                        st.set_d_title("".into());
                        st.set_d_quotes(model(vec![KbQuote { text: s(q, "text").into(), source: s(q, "sourceTitle").into(), broken: !q.get("matches").and_then(Value::as_bool).unwrap_or(true) }]));
                        st.set_d_links_heading("使っているページ".into());
                        st.set_d_links(strs(vec![page.clone()]));
                    }
                    None => clear_detail(&st),
                }
            }
            _ => {}
        }
    });
    // (the borrow above is over: the lines the overview's rows lead to are kept now)
    if let Some((kinds, jumps)) = meta {
        K.with(|k| {
            let mut k = k.borrow_mut();
            k.section_kinds = kinds;
            k.section_jumps = jumps;
        });
    }
}

/// 候補(承認待ち)の詳細: 提案の中身と、承認・却下
fn detail_candidate(st: &KbState<'_>, c: &Value) {
    clear_detail(st);
    st.set_has_detail(true);
    let payload = c.get("payload").cloned().unwrap_or(Value::Null);
    let current = c.get("current");
    st.set_d_title(s(c, "title").into());
    st.set_d_meta(cand_kind_label(c).into());
    st.set_d_badge("承認待ち".into());
    st.set_d_badge_tone(1);
    let mut kv: Vec<KbKv> = Vec::new();
    match s(c, "kind") {
        "failureCase" | "failure" => {
            if let Some(old) = current.map(|o| s(o, "title")).filter(|t| !t.is_empty() && *t != s(c, "title")) {
                kv.push(KbKv { k: "題".into(), v: format!("{old}\n→ {}", s(c, "title")).into() });
            }
            kv.extend(failure_fields(&payload, current).into_iter().map(|(a, b)| KbKv { k: a.into(), v: b.into() }));
        }
        "checklistItem" => {
            if !s(&payload, "role").is_empty() {
                kv.push(KbKv { k: "担当".into(), v: s(&payload, "role").into() });
            }
            kv.push(KbKv { k: "項目".into(), v: s(&payload, "text").into() });
        }
        "overviewAppend" => st.set_d_body(strs(vec![s(&payload, "text").to_string()])),
        _ => {
            // a page or a fact: the new text, and what it replaces when it is an update
            let old_title = current.map(|o| s(o, "title")).filter(|t| !t.is_empty() && *t != s(c, "title"));
            if let Some(old) = old_title {
                kv.push(KbKv { k: "題".into(), v: format!("{old}
→ {}", s(c, "title")).into() });
            }
            if let Some(old) = current.map(|o| s(o, "body")).filter(|t| !t.is_empty() && *t != s(&payload, "body")) {
                kv.push(KbKv { k: "前の本文".into(), v: old.into() });
            }
            st.set_d_body(strs(paragraphs(s(&payload, "body"))));
        }
    }
    st.set_d_kv(model(kv));
    st.set_d_acts(model(vec![act("cand-approve", "承認", true), act("cand-reject", "却下", false)]));
}

fn detail_prohib(st: &KbState<'_>, p: &Value) {
    clear_detail(st);
    st.set_has_detail(true);
    st.set_d_title(s(p, "title").into());
    st.set_d_meta(strength_label(s(p, "strength")).into());
    let origin = p.get("origin").cloned().unwrap_or(Value::Null);
    st.set_d_body(strs(if s(p, "reason").is_empty() { vec![] } else { vec![s(p, "reason").to_string()] }));
    let mut kv = Vec::new();
    let cond = match_text(p.get("match").unwrap_or(&Value::Null));
    if !cond.is_empty() {
        kv.push(KbKv { k: "条件".into(), v: cond.into() });
    }
    if !s(&origin, "quote").is_empty() {
        kv.push(KbKv { k: "原文".into(), v: format!("「{}」", s(&origin, "quote")).into() });
    }
    st.set_d_kv(model(kv));
    let hist = hist_rows(p);
    st.set_d_hist_heading("緩和の履歴".into());
    st.set_d_hist(model(hist));
    st.set_d_acts(model(vec![act("proh-edit", "編集", true), act("proh-retract", "撤回", false)]));
}

fn detail_page(st: &KbState<'_>, p: &Value) {
    clear_detail(st);
    st.set_has_detail(true);
    st.set_d_title(s(p, "title").into());
    st.set_d_tags(strs(p.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()));
    let confirmed = p.get("confirmed").and_then(Value::as_bool).unwrap_or(false);
    let by = p.get("revs").and_then(Value::as_array).and_then(|a| a.first()).map(|r| s(r, "by")).unwrap_or("");
    st.set_d_meta(if by == "model" { "AI" } else if by == "user" { "自分" } else { "" }.into());
    if !confirmed {
        st.set_d_badge("未確認".into());
        st.set_d_badge_tone(1);
    }
    st.set_d_body(strs(paragraphs(s(p, "body"))));
    let quotes: Vec<KbQuote> = p
        .get("quotes")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|q| KbQuote { text: s(q, "text").into(), source: s(q, "sourceTitle").into(), broken: !q.get("matches").and_then(Value::as_bool).unwrap_or(true) }).collect())
        .unwrap_or_default();
    st.set_d_quotes(model(quotes));
    let links: Vec<String> = p.get("links").and_then(Value::as_array).map(|a| a.iter().map(|l| s(l, "title").to_string()).collect()).unwrap_or_default();
    st.set_d_links_heading("リンク".into());
    st.set_d_links(strs(links));
    st.set_d_backlinks(strs(p.get("backlinks").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()));
    let mut acts = Vec::new();
    if !confirmed {
        acts.push(act("page-confirm", "確認済みにする", true));
    }
    acts.push(act("page-edit", "編集", confirmed));
    acts.push(act("page-delete", "削除", false));
    st.set_d_acts(model(acts));
}

fn render_overview(app: &AppWindow, k: &K) -> (Vec<&'static str>, Vec<Vec<i32>>) {
    let st = app.global::<KbState>();
    let (title, body) = overview_parts(&k.overview_text, &k.project);
    st.set_o_title(title.into());
    st.set_o_body(strs(body));
    let mut sections: Vec<KbSection> = Vec::new();
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut jumps: Vec<Vec<i32>> = Vec::new();
    // タグ
    let tags: Vec<String> = k.index.get("tags").and_then(Value::as_array).map(|a| a.iter().map(|t| format!("#{} {}", s(t, "tag"), t.get("count").and_then(Value::as_i64).unwrap_or(0))).collect()).unwrap_or_default();
    if !tags.is_empty() {
        sections.push(KbSection { heading: "タグ".into(), rows: ModelRc::default(), chips: strs(tags) });
        kinds.push("tags");
        jumps.push(vec![]);
    }
    // 有効な禁止事項
    let active: Vec<&Value> = k.index.get("activeProhibitions").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    if !active.is_empty() {
        let rows: Vec<KbRow> = active
            .iter()
            .map(|p| row(s(p, "title"), "", if p.get("relaxed").and_then(Value::as_bool).unwrap_or(false) { "緩和中" } else { strength_label(s(p, "strength")) }, 0, false))
            .collect();
        sections.push(KbSection { heading: "禁止事項".into(), rows: model(rows), chips: ModelRc::default() });
        kinds.push("prohib");
        jumps.push((0..active.len() as i32).collect());
    }
    // チェックリスト
    let open: i64 = k.index.get("openChecklist").and_then(Value::as_array).map(|a| a.iter().map(|r| r.get("open").and_then(Value::as_i64).unwrap_or(0)).sum()).unwrap_or(0);
    let total: i64 = k.index.get("openChecklist").and_then(Value::as_array).map(|a| a.iter().map(|r| r.get("total").and_then(Value::as_i64).unwrap_or(0)).sum()).unwrap_or(0);
    if total > 0 {
        sections.push(KbSection { heading: "チェックリスト".into(), rows: model(vec![row(&format!("未了の項目 {open} 件"), "", "", 0, false)]), chips: ModelRc::default() });
        kinds.push("check");
        jumps.push(vec![0]);
    }
    // 未作成のリンク
    let unresolved: Vec<String> = k.index.get("unresolvedLinks").and_then(Value::as_array).map(|a| a.iter().map(|l| s(l, "title").to_string()).collect()).unwrap_or_default();
    if !unresolved.is_empty() {
        sections.push(KbSection { heading: "未作成のリンク".into(), rows: ModelRc::default(), chips: strs(unresolved) });
        kinds.push("unresolved");
        jumps.push(vec![]);
    }
    // 承認待ちの候補
    if !k.cands.is_empty() {
        let n = k.cands.len() as i32;
        sections.push(KbSection { heading: "承認待ち".into(), rows: model(cand_rows(&k.cands, -1)), chips: ModelRc::default() });
        kinds.push("cand");
        jumps.push((0..n).collect());
    }
    // 最近の失敗の記録
    let recent: Vec<KbRow> = k.failures.iter().take(2).map(|f| row(s(f, "title"), &cond_text(s(f, "conditions")), "", 0, false)).collect();
    if !recent.is_empty() {
        let n = recent.len() as i32;
        sections.push(KbSection { heading: "最近の失敗の記録".into(), rows: model(recent), chips: ModelRc::default() });
        kinds.push("fail");
        jumps.push((0..n).collect());
    }
    st.set_o_sections(model(sections));
    (kinds, jumps)
}

fn refresh(app: &AppWindow, host: &Host) {
    let cwd = cwd(app);
    let st = app.global::<KbState>();
    let sub = st.get_sub();
    host.rpc("kb.project", "inocode/kb/project/get", json!({ "cwd": cwd }));
    match sub {
        SUB_OVERVIEW => {
            host.rpc("kb.overview", "inocode/kb/overview/read", json!({ "cwd": cwd }));
            host.rpc("kb.index", "inocode/kb/index/get", json!({ "cwd": cwd }));
            host.rpc("kb.failures", "inocode/kb/failure/list", json!({ "cwd": cwd, "query": Value::Null, "status": Value::Null }));
            host.rpc("kb.cands", "inocode/kb/candidate/list", json!({ "cwd": cwd }));
        }
        SUB_PROHIB => {
            host.rpc("kb.prohib", "inocode/kb/prohibition/list", json!({ "cwd": cwd, "status": "active" }));
        }
        SUB_FAIL => {
            host.rpc("kb.failures", "inocode/kb/failure/list", json!({ "cwd": cwd, "query": Value::Null, "status": Value::Null }));
            host.rpc("kb.cands", "inocode/kb/candidate/list", json!({ "cwd": cwd }));
        }
        SUB_PAGES | SUB_QUOTES => {
            let tag = st.get_tag().to_string();
            let tag = tag.trim_start_matches('#');
            host.rpc(
                "kb.pages",
                "inocode/kb/page/list",
                json!({ "cwd": cwd, "query": if st.get_query().is_empty() { Value::Null } else { json!(st.get_query().to_string()) }, "tag": if tag.is_empty() { Value::Null } else { json!(tag) }, "limit": 100, "cursor": Value::Null }),
            );
            host.rpc("kb.tags", "inocode/kb/tag/list", json!({ "cwd": cwd, "prefix": "" }));
        }
        _ => {}
    }
}

fn refresh_checklist(app: &AppWindow, host: &Host) {
    let role = K.with(|k| k.borrow().role.clone());
    let mut p = json!({ "cwd": cwd(app) });
    if !role.is_empty() {
        p["role"] = json!(role);
    }
    host.rpc("kb.check", "inocode/kb/checklist/list", p);
}

fn render_checklist(app: &AppWindow) {
    let st = app.global::<KbState>();
    K.with(|k| {
        let k = k.borrow();
        st.set_roles(strs(k.roles.clone()));
        st.set_role(k.role.clone().into());
        st.set_checks(model(k.items.iter().map(check_row).collect()));
    });
}

fn open_page(app: &AppWindow, host: &Host, title: &str) {
    K.with(|k| k.borrow_mut().sel_title = Some(title.to_string()));
    host.rpc("kb.page", "inocode/kb/page/read", json!({ "cwd": cwd(app), "title": title }));
}

fn begin_edit(app: &AppWindow, kind: i32, new: bool, title: &str, body: &str, reason: &str, strength: i32) {
    let st = app.global::<KbState>();
    st.set_note("".into());
    st.set_edit_new(new);
    st.set_edit_title(title.into());
    st.set_edit_body(body.into());
    st.set_edit_reason(reason.into());
    st.set_edit_strength(strength);
    st.set_edit_kind(kind);
}

fn show_error(app: &AppWindow, e: &str) {
    let st = app.global::<KbState>();
    // 競合は読み直す。理由があれば、その理由(人が読める文)を出す
    let text = if e.contains("-32010") || e.to_lowercase().contains("conflict") {
        "ほかで更新されています。読み込み直してください".to_string()
    } else {
        crate::errmsg::short(e)
    };
    st.set_note(text.into());
}

fn go_pages(app: &AppWindow, host: &Host, tag: &str, title: Option<&str>) {
    app.global::<crate::Tabs>().invoke_select(2);
    let st = app.global::<KbState>();
    st.set_evid_sub(SUB_PAGES);
    st.set_sub(SUB_PAGES);
    st.set_tag(tag.into());
    st.set_query("".into());
    st.set_edit_kind(0);
    if let Some(t) = title {
        K.with(|k| k.borrow_mut().sel_title = Some(t.to_string()));
    }
    refresh(app, host);
    if let Some(t) = title {
        open_page(app, host, t);
    }
}

/// Where a registered card or a candidate leads (`target: { tab, sub, ref }` of the server): the tab, its part,
/// and the record. Used by the cards of the chat.
pub(crate) fn go_target(app: &AppWindow, tab: &str, sub: &str, reference: &str) {
    let Some(host) = HOST.with(|h| h.borrow().clone()) else { return };
    let st = app.global::<KbState>();
    match (tab, sub) {
        ("knowledge", "checklist") => {
            app.set_panel_open(true);
            app.global::<crate::SidePanelState>().invoke_select(4);
        }
        ("knowledge", _) => {
            let n = match sub {
                "overview" => SUB_OVERVIEW,
                "prohibitions" | "prohibition" => SUB_PROHIB,
                "common" => SUB_COMMON,
                _ => SUB_FAIL,
            };
            app.global::<crate::Tabs>().invoke_select(1);
            st.set_know_sub(n);
            st.set_sub(n);
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.sel = -1;
                k.extra_sel = -1;
                // the record the card named: by its title, once the list is read
                k.pending_ref = Some(reference.to_string());
            });
            refresh(app, &host);
            render(app);
        }
        ("evidence", "pages") | ("evidence", "quotes") => {
            go_pages(app, &host, "", if reference.is_empty() { None } else { Some(reference) });
        }
        _ => {
            app.global::<crate::Tabs>().invoke_select(if tab == "knowledge" { 1 } else { 2 });
        }
    }
}

pub fn install(app: &AppWindow, host: &Host) {
    HOST.with(|h| *h.borrow_mut() = Some(host.clone()));
    let st = app.global::<KbState>();
    st.set_sub(SUB_OVERVIEW);
    st.set_evid_sub(SUB_PAGES);

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_sub_changed(move |n| {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KbState>();
        st.set_edit_kind(0);
        st.set_note("".into());
        st.set_query("".into());
        st.set_tag("".into());
        // 発動の記録・引き継ぎ(知識タブ)と、マインドマップ・文書の検証(根拠タブ)は、それぞれの画面が出す
        if (4..=5).contains(&n) || (12..=13).contains(&n) {
            return;
        }
        st.set_sub(n);
        K.with(|k| {
            let mut k = k.borrow_mut();
            k.sel = -1;
            k.extra_sel = -1;
            k.sel_title = None;
        });
        clear_detail(&st);
        refresh(&app, &h);
        render(&app);
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_search(move |_| {
        if let Some(app) = weak.upgrade() {
            let sub = app.global::<KbState>().get_sub();
            if matches!(sub, SUB_PAGES | SUB_QUOTES) {
                refresh(&app, &h);
            }
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick_tag(move |t| {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KbState>();
        st.set_tag(if st.get_tag() == t { "".into() } else { t });
        refresh(&app, &h);
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let sub = app.global::<KbState>().get_sub();
        K.with(|k| {
            let mut k = k.borrow_mut();
            k.sel = i;
            k.extra_sel = -1;
        });
        if sub == SUB_PAGES {
            let title = K.with(|k| usize::try_from(i).ok().and_then(|i| k.borrow().pages.get(i).map(|p| s(p, "title").to_string())));
            if let Some(t) = title {
                open_page(&app, &h, &t);
            }
        }
        app.global::<KbState>().set_edit_kind(0);
        render(&app);
    });

    let weak = app.as_weak();
    st.on_pick_extra(move |i| {
        let Some(app) = weak.upgrade() else { return };
        K.with(|k| {
            let mut k = k.borrow_mut();
            k.extra_sel = i;
            k.sel = -1;
        });
        render(&app);
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick_section_row(move |si, ri| {
        let Some(app) = weak.upgrade() else { return };
        let (kind, jump) = K.with(|k| {
            let k = k.borrow();
            (k.section_kinds.get(si as usize).copied().unwrap_or(""), k.section_jumps.get(si as usize).and_then(|j| j.get(ri as usize).copied()).unwrap_or(0))
        });
        let st = app.global::<KbState>();
        match kind {
            "cand" => {
                st.set_know_sub(SUB_FAIL);
                st.set_sub(SUB_FAIL);
                K.with(|k| {
                    let mut k = k.borrow_mut();
                    k.sel = -1;
                            k.extra_sel = jump;
                });
                refresh(&app, &h);
                render(&app);
            }
            "prohib" | "fail" => {
                let sub = if kind == "prohib" { SUB_PROHIB } else { SUB_FAIL };
                st.set_know_sub(sub);
                st.set_sub(sub);
                K.with(|k| k.borrow_mut().sel = jump);
                refresh(&app, &h);
                render(&app);
            }
            "check" => {
                app.set_panel_open(true);
                app.global::<crate::SidePanelState>().invoke_select(4);
                app.global::<crate::Tabs>().invoke_select(0);
            }
            _ => {}
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick_chip(move |c| {
        let Some(app) = weak.upgrade() else { return };
        let c = c.to_string();
        let kind_unresolved = K.with(|k| k.borrow().section_kinds.contains(&"unresolved"));
        // "#タグ 件数" はタグの絞り込み、それ以外(未作成のリンク)は新しいページ
        if let Some(rest) = c.strip_prefix('#') {
            let tag = rest.split(' ').next().unwrap_or("").to_string();
            go_pages(&app, &h, &format!("#{tag}"), None);
        } else if kind_unresolved {
            go_pages(&app, &h, "", None);
            begin_edit(&app, 2, true, &c, "", "", 0);
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick_link(move |t| {
        if let Some(app) = weak.upgrade() {
            go_pages(&app, &h, "", Some(t.as_str()));
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_act(move |id| {
        let Some(app) = weak.upgrade() else { return };
        let cwd = cwd(&app);
        let act_id = id.to_string();
        match id.as_str() {
            "overview-edit" => {
                let text = K.with(|k| k.borrow().overview_text.clone());
                begin_edit(&app, 1, false, "", &text, "", 0);
            }
            "proh-edit" => {
                let cur = K.with(|k| usize::try_from(k.borrow().sel).ok().and_then(|i| k.borrow().prohib.get(i).cloned()));
                if let Some(p) = cur {
                    begin_edit(&app, 3, false, s(&p, "title"), "", s(&p, "reason"), if strength_label(s(&p, "strength")) == "警告" { 1 } else { 0 });
                }
            }
            "proh-retract" => {
                let cur = K.with(|k| usize::try_from(k.borrow().sel).ok().and_then(|i| k.borrow().prohib.get(i).cloned()));
                if let Some(p) = cur {
                    h.rpc("kb.write", "inocode/kb/prohibition/retract", json!({ "cwd": cwd, "id": s(&p, "id") }));
                    K.with(|k| k.borrow_mut().sel = -1);
                }
            }
            "cand-approve" | "cand-reject" => {
                let id = K.with(|k| {
                    let k = k.borrow();
                    usize::try_from(k.extra_sel).ok().and_then(|i| k.cands.get(i)).map(|c| s(c, "id").to_string())
                });
                if let Some(id) = id {
                    let m = if act_id == "cand-approve" { "approve" } else { "reject" };
                    h.rpc("kb.cand.act", &format!("inocode/kb/candidate/{m}"), json!({ "cwd": cwd, "id": id }));
                }
            }
            "page-edit" => {
                let cur = K.with(|k| k.borrow().sel_title.clone().and_then(|t| k.borrow().read.iter().find(|(x, _)| *x == t).map(|(_, v)| v.clone())));
                if let Some(p) = cur {
                    K.with(|k| k.borrow_mut().pending_rev = Some(s(&p, "rev").to_string()));
                    begin_edit(&app, 2, false, s(&p, "title"), s(&p, "body"), "", 0);
                }
            }
            "page-confirm" => {
                let cur = K.with(|k| k.borrow().sel_title.clone().and_then(|t| k.borrow().read.iter().find(|(x, _)| *x == t).map(|(_, v)| v.clone())));
                if let Some(p) = cur {
                    h.rpc("kb.page.saved", "inocode/kb/page/write", json!({ "cwd": cwd, "title": s(&p, "title"), "body": s(&p, "body"), "baseRev": s(&p, "rev"), "by": "user", "confirm": true }));
                }
            }
            "page-delete" => {
                let t = K.with(|k| k.borrow().sel_title.clone());
                if let Some(t) = t {
                    h.rpc("kb.write", "inocode/kb/page/delete", json!({ "cwd": cwd, "title": t }));
                    K.with(|k| k.borrow_mut().sel_title = None);
                }
            }
            _ => {}
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_revoke(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let cur = K.with(|k| usize::try_from(k.borrow().sel).ok().and_then(|i| k.borrow().prohib.get(i).cloned()));
        let Some(p) = cur else { return };
        let rels: Vec<Value> = p.get("relaxations").and_then(Value::as_array).cloned().unwrap_or_default();
        // 履歴は新しい順に出している
        let Some(r) = rels.iter().rev().nth(i as usize) else { return };
        h.rpc("kb.write", "inocode/kb/relaxation/revoke", json!({ "cwd": cwd(&app), "prohibitionId": s(&p, "id"), "relaxationId": s(r, "id") }));
    });

    let (weak, _h) = (app.as_weak(), host.clone());
    st.on_create(move || {
        let Some(app) = weak.upgrade() else { return };
        match app.global::<KbState>().get_sub() {
            SUB_PROHIB => begin_edit(&app, 3, true, "", "", "", 0),
            SUB_PAGES => begin_edit(&app, 2, true, "", "", "", 0),
            _ => {}
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_save(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KbState>();
        let cwd = cwd(&app);
        match st.get_edit_kind() {
            1 => {
                let rev = K.with(|k| k.borrow().overview_rev.clone());
                h.rpc("kb.overview.saved", "inocode/kb/overview/write", json!({ "cwd": cwd, "text": st.get_edit_body().to_string(), "baseRev": rev }));
            }
            2 => {
                let title = st.get_edit_title().trim().to_string();
                if title.is_empty() {
                    st.set_note("題名を入れてください".into());
                    return;
                }
                let mut p = json!({ "cwd": cwd, "title": title, "body": st.get_edit_body().to_string(), "by": "user", "confirm": true });
                if !st.get_edit_new() {
                    if let Some(rev) = K.with(|k| k.borrow().pending_rev.clone()) {
                        p["baseRev"] = json!(rev);
                    }
                }
                h.rpc("kb.page.saved", "inocode/kb/page/write", p);
            }
            3 => {
                let title = st.get_edit_title().trim().to_string();
                if title.is_empty() {
                    st.set_note("題名を入れてください".into());
                    return;
                }
                let existing = if st.get_edit_new() { None } else { K.with(|k| usize::try_from(k.borrow().sel).ok().and_then(|i| k.borrow().prohib.get(i).cloned())) };
                let mut p = json!({
                    "cwd": cwd,
                    "id": existing.as_ref().map(|e| s(e, "id").to_string()),
                    "title": title,
                    "reason": st.get_edit_reason().to_string(),
                    "strength": if st.get_edit_strength() == 1 { "warn" } else { "block" },
                    "match": existing.as_ref().and_then(|e| e.get("match").cloned()).unwrap_or(Value::Null),
                    "role": existing.as_ref().and_then(|e| e.get("role").cloned()).unwrap_or(Value::Null),
                    "expires": existing.as_ref().and_then(|e| e.get("expires").cloned()).unwrap_or(Value::Null),
                    "irreversible": existing.as_ref().and_then(|e| e.get("irreversible").and_then(Value::as_bool)).unwrap_or(false),
                    "origin": existing.as_ref().and_then(|e| e.get("origin").cloned()).unwrap_or_else(|| json!({ "kind": "ui" })),
                });
                if existing.is_none() {
                    p["id"] = Value::Null;
                }
                h.rpc("kb.prohib.saved", "inocode/kb/prohibition/write", p);
            }
            _ => {}
        }
    });

    let weak = app.as_weak();
    st.on_cancel(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<KbState>().set_edit_kind(0);
            app.global::<KbState>().set_note("".into());
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_pick_role(move |r| {
        let Some(app) = weak.upgrade() else { return };
        K.with(|k| k.borrow_mut().role = r.to_string());
        refresh_checklist(&app, &h);
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_toggle_check(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let (role, item) = K.with(|k| {
            let k = k.borrow();
            (k.role.clone(), usize::try_from(i).ok().and_then(|i| k.items.get(i).cloned()))
        });
        if let Some(it) = item {
            if s(&it, "check") != "machine" {
                let done = it.get("done").and_then(Value::as_bool).unwrap_or(false);
                h.rpc("kb.check.set", "inocode/kb/checklist/setDone", json!({ "cwd": cwd(&app), "role": role, "itemId": s(&it, "id"), "done": !done }));
            }
        }
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_add_check(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KbState>();
        let text = st.get_check_add().trim().to_string();
        if text.is_empty() {
            return;
        }
        st.set_check_add("".into());
        write_items(&app, &h, |items| items.push(json!({ "id": Value::Null, "text": text, "when": "beforeReport", "check": "user", "origin": Value::Null })));
    });

    let (weak, h) = (app.as_weak(), host.clone());
    st.on_remove_check(move |i| {
        if let Some(app) = weak.upgrade() {
            write_items(&app, &h, |items| {
                if (i as usize) < items.len() {
                    items.remove(i as usize);
                }
            });
        }
    });

    let weak = app.as_weak();
    st.on_open_checklist(move || {
        if let Some(app) = weak.upgrade() {
            app.set_panel_open(true);
            app.global::<crate::SidePanelState>().invoke_select(4);
        }
    });
}

/// チェックリストの全項目を置き換えて書く(契約: 担当ごとに全項目を置き換える)
fn write_items(app: &AppWindow, host: &Host, change: impl FnOnce(&mut Vec<Value>)) {
    let (role, mut items) = K.with(|k| {
        let k = k.borrow();
        (
            k.role.clone(),
            k.items
                .iter()
                .map(|i| json!({ "id": s(i, "id"), "text": s(i, "text"), "when": i.get("when").cloned().unwrap_or(Value::Null), "check": s(i, "check"), "origin": i.get("origin").cloned().unwrap_or(Value::Null) }))
                .collect::<Vec<_>>(),
        )
    });
    if role.is_empty() {
        return;
    }
    change(&mut items);
    host.rpc("kb.check.write", "inocode/kb/checklist/write", json!({ "cwd": cwd(app), "role": role, "items": items }));
}

/// 知識タブ・根拠タブが開かれたとき
pub fn opened(app: &AppWindow, host: &Host) {
    refresh(app, host);
    render(app);
}

/// 右の side panel のチェックリストが開かれたとき
pub fn checklist_opened(app: &AppWindow, host: &Host) {
    refresh_checklist(app, host);
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    match event {
        Event::Rpc { tag, result } if tag.starts_with("kb.") => on_rpc(app, host, tag, result),
        Event::Notify { method, params } if method == "inocode/kb/changed" => {
            if s(params, "area") == "checklist" {
                refresh_checklist(app, host);
            }
            refresh(app, host);
        }
        _ => {}
    }
}

fn on_rpc(app: &AppWindow, host: &Host, tag: &str, result: &Result<Value, String>) {
    // INOCODE_GUI_KB_LOG=1: what the server answered (for the snapshot runs)
    if std::env::var("INOCODE_GUI_KB_LOG").is_ok() {
        eprintln!("kb {tag}: {}", match result { Ok(v) => v.to_string().chars().take(160).collect::<String>(), Err(e) => format!("error {e}") });
    }
    let st = app.global::<KbState>();
    let v = match result {
        Ok(v) => payload(v),
        Err(e) => {
            match tag {
                // 画面を開いただけの読み取りが失敗しても、何も出さない(サーバーが古いなど)
                "kb.project" | "kb.index" | "kb.tags" | "kb.check" | "kb.cands" | "kb.failures" => {}
                "kb.overview.saved" | "kb.page.saved" | "kb.prohib.saved" => show_error(app, e),
                _ => show_error(app, e),
            }
            if tag == "kb.overview.saved" && (e.contains("-32010") || e.to_lowercase().contains("conflict")) {
                host.rpc("kb.overview", "inocode/kb/overview/read", json!({ "cwd": cwd(app) }));
            }
            return;
        }
    };
    match tag {
        "kb.project" => K.with(|k| k.borrow_mut().project = s(v, "name").to_string()),
        "kb.overview" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.overview_text = s(v, "text").to_string();
                k.overview_rev = s(v, "rev").to_string();
            });
            render(app);
        }
        "kb.index" => {
            K.with(|k| k.borrow_mut().index = v.clone());
            render(app);
        }
        "kb.prohib" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.prohib = v.get("prohibitions").and_then(Value::as_array).cloned().unwrap_or_default();
                if let Some(r) = k.pending_ref.clone() {
                    if let Some(i) = k.prohib.iter().position(|p| s(p, "id") == r || s(p, "title") == r) {
                        k.sel = i as i32;
                        k.pending_ref = None;
                    }
                }
                // the first one is open until another is picked
                if k.sel < 0 && k.extra_sel < 0 && !k.prohib.is_empty() {
                    k.sel = 0;
                }
            });
            render(app);
        }
        "kb.cands" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.cands = v.get("candidates").and_then(Value::as_array).cloned().unwrap_or_default();
                if k.extra_sel >= 0 && (k.extra_sel as usize) >= k.cands.len() {
                    k.extra_sel = -1;
                }
                // the card named a candidate
                if let Some(r) = k.pending_ref.clone() {
                    if let Some(i) = k.cands.iter().position(|c| s(c, "title") == r || s(c, "id") == r) {
                        k.extra_sel = i as i32;
                        k.sel = -1;
                        k.pending_ref = None;
                    }
                }
            });
            render(app);
        }
        "kb.failures" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.failures = v.get("failures").and_then(Value::as_array).cloned().unwrap_or_default();
                if let Some(r) = k.pending_ref.clone() {
                    if let Some(i) = k.failures.iter().position(|f| s(f, "title") == r || s(f, "id") == r) {
                        k.sel = i as i32;
                        k.extra_sel = -1;
                        k.pending_ref = None;
                    }
                }
                if k.sel < 0 && k.extra_sel < 0 && !k.failures.is_empty() {
                    k.sel = 0;
                }
            });
            render(app);
        }
        "kb.pages" => {
            let pages = v.get("pages").and_then(Value::as_array).cloned().unwrap_or_default();
            let sub = st.get_sub();
            let titles: Vec<String> = pages.iter().take(if sub == SUB_QUOTES { QUOTE_PAGES } else { 1 }).map(|p| s(p, "title").to_string()).collect();
            let first = pages.first().map(|p| s(p, "title").to_string());
            let none_open = K.with(|k| k.borrow().sel_title.is_none());
            K.with(|k| k.borrow_mut().pages = pages);
            if sub == SUB_PAGES && none_open {
                if let Some(t) = first {
                    open_page(app, host, &t);
                }
            }
            // 引用の一覧は、ページを読んで引用を集める
            if sub == SUB_QUOTES {
                for t in titles {
                    host.rpc("kb.page", "inocode/kb/page/read", json!({ "cwd": cwd(app), "title": t }));
                }
            }
            if sub == SUB_QUOTES {
                K.with(|k| {
                    let mut k = k.borrow_mut();
                    if k.sel < 0 {
                        k.sel = 0;
                    }
                });
            }
            render(app);
        }
        "kb.tags" => {
            K.with(|k| k.borrow_mut().tags = v.get("tags").and_then(Value::as_array).map(|a| a.iter().map(|t| s(t, "tag").to_string()).collect()).unwrap_or_default());
            render(app);
        }
        "kb.page" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                let t = s(v, "title").to_string();
                match k.read.iter_mut().find(|(x, _)| *x == t) {
                    Some(e) => e.1 = v.clone(),
                    None => k.read.push((t, v.clone())),
                }
            });
            render(app);
        }
        "kb.check" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.roles = v.get("roles").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                if k.role.is_empty() || !k.roles.contains(&k.role) {
                    k.role = k.roles.first().cloned().unwrap_or_default();
                }
                k.items = v.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
            });
            render_checklist(app);
        }
        "kb.cand.act" => {
            K.with(|k| {
                let mut k = k.borrow_mut();
                k.extra_sel = -1;
                k.sel = -1;
            });
            refresh(app, host);
            refresh_checklist(app, host);
        }
        "kb.overview.saved" | "kb.prohib.saved" | "kb.write" => {
            st.set_edit_kind(0);
            refresh(app, host);
        }
        "kb.page.saved" => {
            let errors = v.get("quoteErrors").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            st.set_edit_kind(0);
            if errors > 0 {
                st.set_note(format!("原資料の本文に無い引用が {errors} 件あります").into());
            }
            // 保存したページを読み直す
            let title = K.with(|k| k.borrow().sel_title.clone());
            if let Some(t) = title {
                open_page(app, host, &t);
            } else if let Some(t) = v.get("title").and_then(Value::as_str) {
                open_page(app, host, t);
            }
            refresh(app, host);
        }
        "kb.check.set" | "kb.check.write" => refresh_checklist(app, host),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract").join(name);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        v["response"].clone()
    }

    #[test]
    fn prohibitions_show_strength_and_relaxation_without_ids() {
        let list = fixture("kb-prohibition-list.json").get("prohibitions").and_then(Value::as_array).cloned().unwrap();
        let rows = prohib_rows(&list, 0);
        assert_eq!(rows[0].title.as_str(), "プロセスを名前で止めない");
        // a relaxation that is active now: the row says so instead of the strength
        assert_eq!(rows[0].end.as_str(), "緩和中");
        assert!(rows[0].selected);
        assert!(!rows.iter().any(|r| r.title.contains("proh-") || r.sub.contains("proh-")));
        let h = hist_rows(&list[0]);
        assert_eq!(h.len(), 1);
        assert!(h[0].can_revoke);
        assert!(h[0].text.contains("この app だけは名前で止めていい") || h[0].text.starts_with('「'));
        assert!(h[0].meta.contains("この依頼の間"));
        assert!(!h[0].meta.contains("allowOnly") && !h[0].meta.contains("request"));
    }

    #[test]
    fn page_bodies_drop_quote_lines_and_split_paragraphs() {
        let body = "#設計\n[WAL の設定] を参照。\n\n> quote:src_0001: database is locked\n次の段落\n";
        let p = paragraphs(body);
        assert_eq!(p, vec!["#設計\n[WAL の設定] を参照。".to_string(), "次の段落".to_string()]);
        assert!(paragraphs("").is_empty());
    }

    #[test]
    fn overview_takes_its_heading_as_title() {
        let (t, body) = overview_parts("# demo-project\n目的: 試す\n方針: 小さく\n", "x");
        assert_eq!(t, "demo-project");
        assert_eq!(body, vec!["目的: 試す\n方針: 小さく".to_string()]);
        let (t, _) = overview_parts("", "プロジェクト名");
        assert_eq!(t, "プロジェクト名");
    }

    #[test]
    fn page_rows_mark_unconfirmed_pages() {
        let pages = fixture("kb-page-list.json").get("pages").and_then(Value::as_array).cloned().unwrap();
        let rows = page_rows(&pages, &None);
        assert_eq!(rows[0].end.as_str(), "未確認");
        assert_eq!(rows[0].end_tone, 1);
        let q = all_quotes(&[("A".to_string(), fixture("kb-page-read.json"))]);
        assert_eq!(q.len(), 1);
        assert_eq!(s(&q[0].1, "sourceTitle"), "SQLite docs");
    }

    #[test]
    fn checklist_rows_say_self_reports_and_lock_machine_items() {
        let items = fixture("kb-checklist-list.json").get("items").and_then(Value::as_array).cloned().unwrap();
        let r = check_row(&items[0]);
        assert!(r.done);
        assert!(!r.can_toggle, "機械が付ける項目は本人が外せない");
        let own = check_row(&json!({ "text": "確認", "check": "self", "done": true }));
        assert_eq!(own.detail.as_str(), "自己申告");
        assert!(own.can_toggle);
    }

    #[test]
    fn list_lines_stay_one_line() {
        let r = row("一行目
二行目	三行目", "  a   b

c ", "", 0, false);
        assert_eq!(r.title.as_str(), "一行目 二行目 三行目");
        assert_eq!(r.sub.as_str(), "a b c");
    }

    #[test]
    fn failure_fields_say_unknown_and_show_what_an_update_changes() {
        assert_eq!(cond_text("unknown"), "不明");
        assert_eq!(cond_text("  Unknown "), "不明");
        assert_eq!(cond_text("fastboot flash の最中"), "fastboot flash の最中");
        let new = json!({ "conditions": "unknown", "attempted": "adb shell", "outcome": "exit 1", "avoid": "使わない" });
        let f = failure_fields(&new, None);
        assert_eq!(f[0], ("条件", "不明".to_string()));
        assert!(f.iter().any(|(k, v)| *k == "実行したこと" && v == "adb shell"));
        let old = json!({ "conditions": "fastboot の最中", "attempted": "adb shell", "outcome": "exit 2" });
        let d = failure_fields(&new, Some(&old));
        assert!(d.iter().any(|(k, v)| *k == "条件" && v == "fastboot の最中
→ 不明"));
        assert!(d.iter().any(|(k, v)| *k == "結果" && v == "exit 2
→ exit 1"));
        // what did not change is shown once
        assert!(d.iter().any(|(k, v)| *k == "実行したこと" && v == "adb shell"));
        // a record without any condition is not left blank
        assert_eq!(failure_fields(&json!({}), None)[0], ("条件", "不明".to_string()));
    }

    #[test]
    fn candidates_are_listed_with_their_kind() {
        let list = fixture("kb-candidate-list.json").get("candidates").and_then(Value::as_array).cloned().unwrap();
        let rows = cand_rows(&list, 0);
        assert_eq!(rows[0].sub.as_str(), "チェックリストの項目");
        assert_eq!(rows[0].end.as_str(), "承認待ち");
        assert_eq!(cand_kind_label(&json!({ "kind": "failureCase", "op": "update" })), "失敗の記録 · 更新の提案");
    }

    #[test]
    fn stamps_and_match_texts_are_short() {
        assert_eq!(stamp("2026-11-12T03:10:00Z"), "2026-11-12 03:10");
        assert_eq!(match_text(&json!({ "commands": ["re:taskkill", "pkill"] })), "taskkill、pkill");
    }
}
