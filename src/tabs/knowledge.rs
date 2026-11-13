//! 知識タブの「発動の記録」と「引き継ぎ」。概要・禁止事項・失敗の記録・共通知識は src/tabs/kb.rs。
//! 呼び出し元は src/tabs/mod.rs (登録は 1 か所)。
//!
//! ガードの発動(inocode/guard/list、通知 guard/fired)と、引き継ぎパックのプレビュー
//! (inocode/knowledge/handoffPreview)。応答は contract (ref/docs/09-overnight-contract.md) の形。
//! モックの応答は `{method, request, response}` の形なので、`response` があればそれを使う。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, KnowledgeState, LogRow};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

#[derive(Default)]
struct K {
    guard: Vec<Value>,
    guard_open: HashSet<String>,
}

thread_local! {
    static K: RefCell<K> = RefCell::new(K::default());
}

// ------------------------------------------------------------------ pure helpers (tested)

/// モックは `{method, request, response}`、実サーバーは result そのもの。
fn payload(result: &Value) -> &Value {
    result.get("response").unwrap_or(result)
}

fn guard_key(e: &Value) -> String {
    let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    format!("{}{}{}", e.get("tsMs").and_then(Value::as_i64).unwrap_or(0), s("toolUseId"), s("ruleId"))
}

/// ガードの発動 1 件 -> (見出し, 補足, 詳細, tone)。遮断は 2、警告は 1。
fn guard_row(e: &Value, thread: &str) -> (String, String, String, i32) {
    let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let block = s("severity") == "block";
    let head = format!("{} {}", if block { "遮断" } else { "警告" }, s("ruleTitle"));
    let meta = format!(
        "{} · {} · {} {}",
        super::team_log::time_label(e.get("tsMs").and_then(Value::as_i64).unwrap_or(0)),
        thread,
        s("tool"),
        s("target")
    );
    let detail = format!(
        "規則: {}\n理由: {}\n対象: {}\nツール: {}\n範囲: {}",
        s("ruleTitle"),
        s("reason"),
        s("target"),
        s("tool"),
        if s("scope") == "global" { "共通" } else { "プロジェクト" }
    );
    (head, meta, detail, if block { 2 } else { 1 })
}

// ------------------------------------------------------------------ screen

/// The work folder of the open thread: the project is decided from it. Without one there is no project, and
/// nothing is asked (never the folder the program was started in: that would be some other project).
fn cwd(_app: &AppWindow) -> String {
    crate::state::STATE.with(|s| s.borrow().active_cwd.clone())
}

fn render_guard(app: &AppWindow) {
    let rows: Vec<LogRow> = K.with(|k| {
        let k = k.borrow();
        k.guard
            .iter()
            .map(|e| {
                let thread = super::team::thread_name(e.get("threadId").and_then(Value::as_str).unwrap_or(""));
                let (head, meta, detail, tone) = guard_row(e, &thread);
                LogRow {
                    head: head.into(),
                    meta: meta.into(),
                    detail: detail.into(),
                    tone,
                    open: k.guard_open.contains(&guard_key(e)),
                    action_label: "禁止事項を開く".into(),
                    action2_label: "誤検知として記録".into(),
                }
            })
            .collect()
    });
    app.global::<KnowledgeState>().set_guard(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn request_guard(app: &AppWindow, host: &Host) {
    if cwd(app).is_empty() {
        K.with(|k| k.borrow_mut().guard.clear());
        render_guard(app);
        return;
    }
    host.rpc("knowledge.guard", "inocode/guard/list", json!({ "cwd": cwd(app), "limit": 100 }));
}

/// 起動時に 1 回。コールバックの登録をここで行う。
pub fn install(app: &AppWindow, host: &Host) {
    let st = app.global::<KnowledgeState>();
    let weak = app.as_weak();
    st.on_guard_toggled(move |i| {
        let Some(app) = weak.upgrade() else { return };
        K.with(|k| {
            let mut k = k.borrow_mut();
            if let Some(key) = k.guard.get(i as usize).map(guard_key) {
                if !k.guard_open.remove(&key) {
                    k.guard_open.insert(key);
                }
            }
        });
        render_guard(&app);
    });
    let weak = app.as_weak();
    st.on_guard_open_rule(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let id = K.with(|k| k.borrow().guard.get(i as usize).map(|e| e.get("ruleId").and_then(Value::as_str).unwrap_or("").to_string()));
        if let Some(id) = id {
            super::kb::go_target(&app, "knowledge", "prohibitions", &id);
        }
    });
    let h = host.clone();
    st.on_guard_false_positive(move |i| {
        let thread = K.with(|k| k.borrow().guard.get(i as usize).and_then(|e| e.get("threadId").and_then(Value::as_str).map(str::to_string)));
        if let Some(thread) = thread {
            h.rpc("knowledge.falsePositive", "inocode/metrics/markFalsePositive", json!({ "threadId": thread, "source": "guard" }));
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_handoff_preview(move || {
        let Some(app) = weak.upgrade() else { return };
        let st = app.global::<KnowledgeState>();
        let task = st.get_handoff_task().to_string();
        st.set_handoff_text("".into());
        h.rpc("knowledge.handoff", "inocode/knowledge/handoffPreview", json!({ "cwd": cwd(&app), "task": task }));
    });
    let (weak, h) = (app.as_weak(), host.clone());
    st.on_part_changed(move |part| {
        let Some(app) = weak.upgrade() else { return };
        app.global::<KnowledgeState>().set_note("".into());
        if part == 4 {
            request_guard(&app, &h);
        }
    });
}

/// このタブが開かれるたびに呼ばれる。発動の記録を開いているなら、読み直す。
pub fn opened(app: &AppWindow, host: &Host) {
    if app.global::<crate::KbState>().get_know_sub() == 4 {
        request_guard(app, host);
    }
}

/// クライアントのイベント (Event::Rpc / Event::Notify) が届くたびに呼ばれる。
pub fn event(app: &AppWindow, _host: &Host, event: &Event) {
    if let Event::Notify { method, params } = event {
        if method == "inocode/guard/fired" {
            K.with(|k| k.borrow_mut().guard.insert(0, params.clone()));
            render_guard(app);
        }
        return;
    }
    let Event::Rpc { tag, result } = event else { return };
    let st = app.global::<KnowledgeState>();
    match (tag.as_str(), result) {
        ("knowledge.guard", Ok(v)) => {
            let mut list = payload(v).get("events").and_then(Value::as_array).cloned().unwrap_or_default();
            list.sort_by_key(|e| std::cmp::Reverse(e.get("tsMs").and_then(Value::as_i64).unwrap_or(0)));
            st.set_note(if list.is_empty() { "ガードの発動はまだありません".into() } else { SharedString::default() });
            K.with(|k| k.borrow_mut().guard = list);
            render_guard(app);
        }
        ("knowledge.guard", Err(e)) => st.set_note(e.as_str().into()),
        ("knowledge.handoff", Ok(v)) => {
            st.set_handoff_text(payload(v).get("text").and_then(Value::as_str).unwrap_or("").into());
        }
        ("knowledge.handoff", Err(e)) => st.set_handoff_text(e.as_str().into()),
        ("knowledge.falsePositive", Err(e)) => st.set_note(e.as_str().into()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("contract").join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn guard_events_show_blocked_and_warned() {
        let list = payload(&fixture("guard-list.json")).get("events").and_then(Value::as_array).cloned().unwrap();
        let blocked = guard_row(&list[0], "T");
        assert_eq!(blocked.0, "遮断 force push 禁止");
        assert_eq!(blocked.3, 2);
        assert!(blocked.2.contains("理由: 履歴が消える"));
        assert!(!blocked.2.contains("rule-0001"), "規則の ID は画面に出さない");
        let mut w = list[0].clone();
        w["severity"] = json!("warn");
        w["toolUseId"] = json!("call_other");
        let warned = guard_row(&w, "T");
        assert!(warned.0.starts_with("警告") && warned.3 == 1);
        assert_ne!(guard_key(&list[0]), guard_key(&w));
    }
}
