//! The cards of the chat that are not a message: a guard rule that stopped an operation (with the
//! words of the user it comes from and a way to relax it), and what a tool registered in the
//! knowledge or the evidence (with approve / reject for a candidate). Data: the notification
//! inocode/guard/fired and the result of a tool call; contract and fixtures: ref/docs/64 §8 §9,
//! ref/contract-fixtures/guard-fired-card.json, evidence-record-result.json, kb-relaxation-add-ui.json,
//! kb-candidate-*.json. Only the user can relax, approve or reject: the buttons are the screen's own.

use crate::state::STATE;
use crate::view::messages_with;
use crate::*;
use serde_json::{json, Value};
use slint::Model;
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static INFO: RefCell<HashMap<String, Info>> = RefCell::new(HashMap::new());
    static DEMO_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum GState {
    Stopped,
    Relaxed,
    Revoked,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RState {
    /// What the tool said (registered, candidate or denied)
    Result,
    Rejected,
}

#[derive(Clone, Debug)]
enum Info {
    Guard { data: Value, state: GState, expanded: bool, relaxation: String, scope: String },
    Reg { data: Value, state: RState },
}

/// What a card shows (the screen's struct is made of it).
#[derive(Clone, Debug, Default, PartialEq)]
struct Card {
    icon: &'static str,
    tone: i32,
    title: String,
    status: String,
    sub: String,
    quote_head: String,
    quote_lines: Vec<String>,
    quote_dim: bool,
    items: Vec<String>,
    /// id, label, enabled, primary
    buttons: Vec<(String, String, bool, bool)>,
    pressable: bool,
    dim: bool,
}

const SHIELD: &str = "\u{e9e0}";
const LOCK_OPEN: &str = "\u{e898}";

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn scope_label(scope: &str) -> &'static str {
    match scope {
        "request" => "この依頼の間",
        "session" => "このセッションの間",
        "always" => "取り消すまで",
        _ => "",
    }
}

// ------------------------------------------------------------------ the cards (pure, tested)

fn guard_card(data: &Value, state: GState, expanded: bool, scope: &str) -> Card {
    let kind = data.pointer("/origin/kind").and_then(Value::as_str).unwrap_or("");
    let scopes: Vec<&str> = data.get("availableScopes").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let legacy = kind == "legacy";
    let mut c = Card {
        icon: SHIELD,
        title: s(data, "ruleTitle").to_string(),
        sub: s(data, "target").to_string(),
        ..Default::default()
    };
    // The words of the user the rule comes from.
    match kind {
        "legacy" => {
            c.quote_lines = vec!["旧規則(未移行)".to_string()];
            c.quote_dim = true;
        }
        "ledger" => {
            let missing = data.pointer("/origin/missing").and_then(Value::as_bool).unwrap_or(false);
            let seq = data.pointer("/origin/ledgerRef/seq").and_then(Value::as_i64);
            if missing {
                c.quote_lines = vec!["原文が見つかりません".to_string()];
                c.quote_dim = true;
            } else {
                c.quote_head = match seq {
                    Some(n) => format!("原文 · 自分 · #{n}"),
                    None => "原文 · 自分".to_string(),
                };
                c.quote_lines = crate::reply::excerpt_lines(data.pointer("/origin/quote").and_then(Value::as_str).unwrap_or(""));
            }
        }
        _ => {}
    }
    let open = ("open".to_string(), "禁止事項を開く".to_string(), true, false);
    match state {
        GState::Stopped => {
            c.tone = 1;
            c.status = "止めました".to_string();
            if expanded {
                for sc in ["request", "session", "always"] {
                    if scopes.contains(&sc) {
                        c.buttons.push((format!("scope:{sc}"), scope_label(sc).to_string(), true, false));
                    }
                }
                c.buttons.push(open);
            } else {
                c.buttons.push(open);
                // A rule that was not moved to the new structure cannot be relaxed.
                c.buttons.push(("relax".to_string(), "緩和".to_string(), !scopes.is_empty() && !legacy, true));
            }
        }
        GState::Relaxed => {
            c.icon = LOCK_OPEN;
            c.status = if scope_label(scope).is_empty() { "緩和中".to_string() } else { format!("緩和中 · {}", scope_label(scope)) };
            c.buttons = vec![open, ("revoke".to_string(), "取り消す".to_string(), true, false)];
        }
        GState::Revoked => {
            c.dim = true;
            c.status = "取り消しました".to_string();
            c.buttons = vec![open];
        }
    }
    c
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "evidence" => "根拠",
        "page" => "資料ページ",
        "prohibition" => "禁止事項",
        "checklist" => "チェックリスト",
        "relaxation" => "緩和",
        "overview" => "概要",
        _ => "",
    }
}

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "evidence" => "\u{e86c}",
        "page" => "\u{e873}",
        "prohibition" => SHIELD,
        "checklist" => "\u{e834}",
        "relaxation" => LOCK_OPEN,
        "overview" => "\u{e8d2}",
        _ => "\u{e873}",
    }
}

fn op_label(op: &str) -> &'static str {
    match op {
        "create" => "作成",
        "update" => "更新",
        "add" => "追加",
        "done" => "済",
        "propose" | "append" => "追記の案",
        _ => "",
    }
}

fn reg_card(data: &Value, state: RState) -> Card {
    // A batch: one card with the kind and the number, and the first three.
    if let Some(items) = data.get("items").and_then(Value::as_array).filter(|a| a.len() > 1) {
        let kind = items.first().map(|i| s(i, "kind")).unwrap_or("");
        let count = data.get("count").and_then(Value::as_i64).unwrap_or(items.len() as i64);
        return Card {
            icon: kind_icon(kind),
            tone: 2,
            title: format!("{} {} 件", kind_label(kind), count),
            status: "登録しました".to_string(),
            items: items.iter().take(3).map(|i| format!("{} {}", kind_label(s(i, "kind")), s(i, "title"))).collect(),
            pressable: true,
            ..Default::default()
        };
    }
    let one = data.get("items").and_then(Value::as_array).and_then(|a| a.first()).unwrap_or(data);
    let kind = s(one, "kind");
    let op = op_label(s(one, "op"));
    let mut c = Card { icon: kind_icon(kind), title: s(one, "title").to_string(), ..Default::default() };
    let what = if op.is_empty() { kind_label(kind).to_string() } else { format!("{} · {}", kind_label(kind), op) };
    if state == RState::Rejected {
        c.dim = true;
        c.status = "却下しました".to_string();
        c.sub = what;
        return c;
    }
    match s(one, "status") {
        "denied" => {
            c.tone = 3;
            c.dim = true;
            c.status = "拒否".to_string();
            c.sub = s(one, "reason").to_string();
        }
        "candidate" => {
            c.status = "承認待ち".to_string();
            c.sub = what;
            c.buttons = vec![("approve".to_string(), "承認".to_string(), true, true), ("reject".to_string(), "却下".to_string(), true, false)];
        }
        _ => {
            c.tone = 2;
            c.status = "登録しました".to_string();
            c.sub = what;
            c.pressable = true;
        }
    }
    c
}

/// A result of a tool that is a card: an object with a kind and a status, found in the text of the
/// result (a JSON text) or in its structure.
fn find_card(v: &Value) -> Option<Value> {
    match v {
        Value::Object(m) => {
            if m.get("kind").and_then(Value::as_str).is_some() && m.get("status").and_then(Value::as_str).is_some() {
                return Some(v.clone());
            }
            if m.get("items").and_then(Value::as_array).map_or(false, |a| !a.is_empty()) && m.get("count").is_some() {
                return Some(v.clone());
            }
            m.values().find_map(find_card)
        }
        Value::Array(a) => a.iter().find_map(find_card),
        Value::String(t) if t.trim_start().starts_with('{') => serde_json::from_str::<Value>(t).ok().and_then(|j| find_card(&j)),
        _ => None,
    }
}

// ------------------------------------------------------------------ the screen

fn to_row(id: &str, c: &Card) -> Message {
    let lines = |v: &[String]| ModelRc::from(Rc::new(VecModel::from(v.iter().map(|l| SharedString::from(l.as_str())).collect::<Vec<_>>())));
    let buttons: Vec<CardButton> = c
        .buttons
        .iter()
        .map(|(i, l, e, p)| CardButton { id: i.as_str().into(), label: l.as_str().into(), enabled: *e, primary: *p })
        .collect();
    Message {
        id: id.into(),
        role: "card".into(),
        card: ChatCard {
            icon: c.icon.into(),
            tone: c.tone,
            title: c.title.as_str().into(),
            status: c.status.as_str().into(),
            sub: c.sub.as_str().into(),
            quote_head: c.quote_head.as_str().into(),
            quote_lines: lines(&c.quote_lines),
            quote_dim: c.quote_dim,
            items: lines(&c.items),
            buttons: ModelRc::from(Rc::new(VecModel::from(buttons))),
            pressable: c.pressable,
            dim: c.dim,
        },
        ..Default::default()
    }
}

fn card_of(info: &Info) -> Card {
    match info {
        Info::Guard { data, state, expanded, scope, .. } => guard_card(data, *state, *expanded, scope),
        Info::Reg { data, state } => reg_card(data, *state),
    }
}

/// Put the card of `id` in the chat (a new row at the end, or the row it already has).
fn show(app: &AppWindow, id: &str) {
    let Some(info) = INFO.with(|i| i.borrow().get(id).cloned()) else { return };
    let row = to_row(id, &card_of(&info));
    let mut fresh = false;
    messages_with(app, |model| {
        match (0..model.row_count()).find(|i| model.row_data(*i).map_or(false, |m| m.id.as_str() == id)) {
            Some(i) => model.set_row_data(i, row),
            None => {
                model.push(row);
                fresh = true;
            }
        }
    });
    if fresh {
        app.invoke_scroll_bottom();
    }
}

fn set_info(app: &AppWindow, id: &str, info: Info) {
    INFO.with(|i| i.borrow_mut().insert(id.to_string(), info));
    show(app, id);
}

pub(crate) fn on_notify(app: &AppWindow, method: &str, params: &Value) {
    match method {
        "inocode/guard/fired" => {
            let active = STATE.with(|st| st.borrow().active.clone());
            let thread = s(params, "threadId");
            let stopped = s(params, "result") == "blocked" || s(params, "state") == "stopped";
            if (!thread.is_empty() && thread != active) || !(stopped || s(params, "state") == "relaxed") || s(params, "ruleTitle").is_empty() {
                return;
            }
            let key = if s(params, "toolUseId").is_empty() { params.get("tsMs").map(|t| t.to_string()).unwrap_or_default() } else { s(params, "toolUseId").to_string() };
            let id = format!("guard:{key}");
            let state = if s(params, "state") == "relaxed" { GState::Relaxed } else { GState::Stopped };
            let scope = s(params, "scope").to_string();
            let relaxation = params.get("relaxationId").and_then(Value::as_str).unwrap_or("").to_string();
            set_info(app, &id, Info::Guard { data: params.clone(), state, expanded: false, relaxation, scope: if state == GState::Relaxed { scope } else { String::new() } });
        }
        "gui/toolResult" => {
            let Some(result) = params.get("result").filter(|r| !r.is_null()) else { return };
            let Some(card) = find_card(result) else { return };
            let id = format!("reg:{}", s(params, "id"));
            set_info(app, &id, Info::Reg { data: card, state: RState::Result });
        }
        _ => {}
    }
}

fn rpc(tag: String, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag, method: method.into(), params });
    }
}

fn status(app: &AppWindow, text: &str) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(2);
}

fn open_tab(app: &AppWindow, tab: &str) {
    let index = match tab {
        "knowledge" => 1,
        "evidence" => 2,
        _ => return,
    };
    app.global::<Tabs>().invoke_select(index);
}

fn act(app: &AppWindow, index: usize, button: &str) {
    let model = app.global::<AppState>().get_messages();
    let Some(row) = model.row_data(index) else { return };
    let id = row.id.to_string();
    let Some(info) = INFO.with(|i| i.borrow().get(&id).cloned()) else { return };
    let (cwd, thread) = STATE.with(|st| {
        let st = st.borrow();
        (st.active_cwd.clone(), st.active.clone())
    });
    match (info, button) {
        (Info::Guard { .. }, "open") => open_tab(app, "knowledge"),
        (Info::Guard { data, state, relaxation, scope, .. }, "relax") => set_info(app, &id, Info::Guard { data, state, expanded: true, relaxation, scope }),
        (Info::Guard { data, .. }, b) if b.starts_with("scope:") => {
            let scope = b.trim_start_matches("scope:");
            let mut params = json!({
                "cwd": cwd,
                "prohibitionId": s(&data, "ruleId"),
                "effect": "allowOnly",
                "allow": { "commands": [s(&data, "target")] },
                "scope": scope,
                "by": "user-ui",
            });
            if scope != "always" {
                params["threadId"] = json!(thread);
            }
            rpc(format!("card.relax|{id}|{scope}"), "inocode/kb/relaxation/add", params);
        }
        (Info::Guard { relaxation, .. }, "revoke") => {
            rpc(format!("card.revoke|{id}"), "inocode/kb/relaxation/revoke", json!({ "cwd": cwd, "id": relaxation }));
        }
        (Info::Reg { data, .. }, b @ ("approve" | "reject")) => {
            let one = data.get("items").and_then(Value::as_array).and_then(|a| a.first()).unwrap_or(&data);
            let cand = if s(one, "id").is_empty() { s(&data, "id") } else { s(one, "id") };
            rpc(format!("card.{b}|{id}"), &format!("inocode/kb/candidate/{b}"), json!({ "cwd": cwd, "id": cand }));
        }
        (Info::Reg { data, .. }, "") => {
            let one = data.get("items").and_then(Value::as_array).and_then(|a| a.first()).unwrap_or(&data);
            open_tab(app, one.pointer("/target/tab").and_then(Value::as_str).unwrap_or("evidence"));
        }
        _ => {}
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    let Some(rest) = tag.strip_prefix("card.") else { return };
    let mut parts = rest.split('|');
    let kind = parts.next().unwrap_or("");
    let Some(id) = parts.next().map(str::to_string) else { return };
    let Some(info) = INFO.with(|i| i.borrow().get(&id).cloned()) else { return };
    match (kind, info, result) {
        ("relax", Info::Guard { data, .. }, Ok(v)) => {
            let scope = parts.next().unwrap_or("").to_string();
            let relaxation = v.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            set_info(app, &id, Info::Guard { data, state: GState::Relaxed, expanded: false, relaxation, scope });
        }
        ("revoke", Info::Guard { data, scope, .. }, Ok(_)) => {
            set_info(app, &id, Info::Guard { data, state: GState::Revoked, expanded: false, relaxation: String::new(), scope });
        }
        ("approve", Info::Reg { mut data, .. }, Ok(v)) => {
            // It is registered now: the kind and the target the server gave.
            let one = if data.get("items").is_some() { data.get_mut("items").and_then(|a| a.get_mut(0)) } else { Some(&mut data) };
            if let Some(o) = one {
                o["status"] = json!("registered");
                if let Some(t) = v.get("target") {
                    o["target"] = t.clone();
                }
            }
            set_info(app, &id, Info::Reg { data, state: RState::Result });
        }
        ("reject", Info::Reg { data, .. }, Ok(_)) => set_info(app, &id, Info::Reg { data, state: RState::Rejected }),
        (_, _, Err(e)) => status(app, &format!("できませんでした: {}", crate::errmsg::short(e))),
        _ => {}
    }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let weak = app.as_weak();
    app.global::<CardActions>().on_act(move |index, button| {
        if let Some(app) = weak.upgrade() {
            act(&app, index.max(0) as usize, button.as_str());
        }
    });
    if std::env::var("INOCODE_GUI_DEMO").map(|v| v == "cards").unwrap_or(false) {
        // Mock only: the cards as the contract's fixtures give them.
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(6), move || {
            let Some(app) = weak.upgrade() else { return };
            let guard = |tool: &str, origin: Value, scopes: Value| {
                json!({ "toolUseId": tool, "tool": "shell", "target": "Remove-Item -Recurse build\\out", "result": "blocked", "ruleId": "proh-0001",
                    "ruleTitle": "許可なく削除しない", "origin": origin, "state": "stopped", "availableScopes": scopes })
            };
            let ledger = json!({ "kind": "ledger", "ledgerRef": { "seq": 12 }, "quote": "ユーザーの許可なく削除しない", "missing": false });
            on_notify(&app, "inocode/guard/fired", &guard("c1", ledger.clone(), json!(["request", "session", "always"])));
            let mut relaxed = guard("c2", ledger, json!(["request", "session", "always"]));
            relaxed["state"] = json!("relaxed");
            relaxed["scope"] = json!("request");
            relaxed["relaxationId"] = json!("rel-0003");
            on_notify(&app, "inocode/guard/fired", &relaxed);
            on_notify(&app, "inocode/guard/fired", &guard("c3", json!({ "kind": "legacy" }), json!([])));
            on_notify(&app, "inocode/guard/fired", &guard("c4", json!({ "kind": "ledger", "missing": true }), json!(["request"])));
            let tool = |id: &str, result: Value| json!({ "id": id, "type": "dynamicToolCall", "tool": "record_evidence", "result": result });
            let one = |kind: &str, op: &str, title: &str, status: &str| json!({ "kind": kind, "op": op, "title": title, "status": status, "requiresApproval": status == "candidate", "target": { "tab": "evidence", "sub": "claims", "ref": "x" } });
            on_notify(&app, "gui/toolResult", &tool("r1", one("evidence", "create", "Widget X is 1,200 yen", "registered")));
            on_notify(&app, "gui/toolResult", &tool("r2", one("checklist", "add", "実装前に仕様を確認する", "candidate")));
            let mut denied = one("evidence", "create", "Widget Y", "denied");
            denied["reason"] = json!("引用が原文に含まれません");
            on_notify(&app, "gui/toolResult", &tool("r3", denied));
            on_notify(&app, "gui/toolResult", &tool("r4", json!({ "count": 3, "items": [one("evidence", "create", "価格は 1,200 円", "registered"), one("evidence", "create", "在庫は 5 個", "registered"), one("evidence", "create", "送料は無料", "registered")] })));
        });
        DEMO_TIMER.with(|t| *t.borrow_mut() = Some(timer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fired() -> Value {
        json!({ "toolUseId": "c1", "target": "Remove-Item -Recurse build\\out", "ruleId": "proh-0001", "ruleTitle": "許可なく削除しない",
            "origin": { "kind": "ledger", "ledgerRef": { "threadId": "t", "seq": 12 }, "quote": "ユーザーの許可なく削除しない", "missing": false },
            "state": "stopped", "availableScopes": ["request", "session", "always"] })
    }

    #[test]
    fn a_stopped_card_has_the_rule_the_operation_the_words_and_the_buttons() {
        let c = guard_card(&fired(), GState::Stopped, false, "");
        assert_eq!((c.title.as_str(), c.status.as_str(), c.tone), ("許可なく削除しない", "止めました", 1));
        assert_eq!(c.sub, "Remove-Item -Recurse build\\out");
        assert_eq!(c.quote_head, "原文 · 自分 · #12");
        assert_eq!(c.quote_lines, vec!["ユーザーの許可なく削除しない"]);
        let ids: Vec<&str> = c.buttons.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(ids, vec!["open", "relax"]);
        assert!(c.buttons[1].2);
        // pressed "relax": the three ranges
        let e = guard_card(&fired(), GState::Stopped, true, "");
        let ids: Vec<&str> = e.buttons.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(ids, vec!["scope:request", "scope:session", "scope:always", "open"]);
        assert_eq!(e.buttons[2].1, "取り消すまで");
    }

    #[test]
    fn a_relaxed_card_can_be_revoked_and_a_legacy_rule_cannot_be_relaxed() {
        let r = guard_card(&fired(), GState::Relaxed, false, "session");
        assert_eq!((r.icon, r.tone, r.status.as_str()), (LOCK_OPEN, 0, "緩和中 · このセッションの間"));
        assert_eq!(r.buttons.iter().map(|b| b.0.as_str()).collect::<Vec<_>>(), vec!["open", "revoke"]);
        let v = guard_card(&fired(), GState::Revoked, false, "");
        assert!(v.dim && v.status == "取り消しました" && v.buttons.len() == 1);
        let mut legacy = fired();
        legacy["origin"] = json!({ "kind": "legacy" });
        legacy["availableScopes"] = json!([]);
        let l = guard_card(&legacy, GState::Stopped, false, "");
        assert_eq!(l.quote_lines, vec!["旧規則(未移行)"]);
        assert!(l.quote_dim);
        assert!(!l.buttons.iter().find(|b| b.0 == "relax").unwrap().2);
        let mut missing = fired();
        missing["origin"] = json!({ "kind": "ledger", "missing": true });
        assert_eq!(guard_card(&missing, GState::Stopped, false, "").quote_lines, vec!["原文が見つかりません"]);
    }

    fn one(kind: &str, op: &str, title: &str, status: &str) -> Value {
        json!({ "kind": kind, "op": op, "title": title, "status": status })
    }

    #[test]
    fn a_registered_card_has_the_kind_and_goes_to_its_target() {
        let c = reg_card(&one("evidence", "create", "Widget X", "registered"), RState::Result);
        assert_eq!((c.title.as_str(), c.status.as_str(), c.sub.as_str(), c.tone, c.pressable), ("Widget X", "登録しました", "根拠 · 作成", 2, true));
        let p = reg_card(&one("checklist", "add", "確認する", "candidate"), RState::Result);
        assert_eq!(p.status, "承認待ち");
        assert_eq!(p.buttons.iter().map(|b| b.0.as_str()).collect::<Vec<_>>(), vec!["approve", "reject"]);
        assert!(!p.pressable);
        let mut d = one("evidence", "create", "Y", "denied");
        d["reason"] = json!("引用が原文に含まれません");
        let d = reg_card(&d, RState::Result);
        assert!(d.dim && d.tone == 3 && d.sub == "引用が原文に含まれません" && d.buttons.is_empty());
        let r = reg_card(&one("checklist", "add", "確認する", "candidate"), RState::Rejected);
        assert!(r.dim && r.status == "却下しました" && r.buttons.is_empty());
    }

    #[test]
    fn several_registered_at_once_are_one_card() {
        let batch = json!({ "count": 4, "items": [one("evidence", "create", "a", "registered"), one("evidence", "create", "b", "registered"),
            one("evidence", "create", "c", "registered"), one("evidence", "create", "d", "registered")] });
        let c = reg_card(&batch, RState::Result);
        assert_eq!(c.title, "根拠 4 件");
        assert_eq!(c.items, vec!["根拠 a", "根拠 b", "根拠 c"]);
        assert!(c.pressable);
    }

    #[test]
    fn a_card_is_found_in_the_result_of_a_tool() {
        let inner = one("evidence", "create", "W", "registered").to_string();
        let result = json!({ "content": [{ "type": "text", "text": inner }] });
        assert_eq!(find_card(&result).unwrap()["title"], "W");
        assert!(find_card(&json!({ "content": [{ "type": "text", "text": "plain" }] })).is_none());
        assert!(find_card(&json!({ "kind": "x" })).is_none());
    }
}
