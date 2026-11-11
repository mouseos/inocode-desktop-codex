//! 目標(goal)の固定性の画面: 目標文の変更の承認のカードと、完了の根拠の記録。
//! 通知 inocode/goal/editRequested、RPC inocode/goal/pendingEdit と inocode/goal/resolveEdit、
//! 通知と RPC inocode/goal/completionEvidence を使う(ref/docs/35 の 7 節)。
//! 完了は止めない: 記録は、モデルの申告とホストが観測した事実を別々に並べて見せるだけ。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, DiffRow, GoalLine, GoalState};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const TAG_PENDING: &str = "goal.pending";
const TAG_RESOLVE: &str = "goal.resolve";
const TAG_EVIDENCE: &str = "goal.evidence";

#[derive(Default)]
struct Data {
    thread_id: String,
    /// the request the card is about
    request_id: String,
    request_thread: String,
    /// records the user closed (goalId + recordedAt)
    closed: Vec<String>,
    shown: String,
}

thread_local! {
    static D: RefCell<Data> = RefCell::new(Data::default());
}

pub fn install(app: &AppWindow, host: &Host) {
    let h = host.clone();
    app.global::<GoalState>().on_resolve(move |approve| {
        let (thread, request) = D.with(|d| {
            let d = d.borrow();
            (d.request_thread.clone(), d.request_id.clone())
        });
        if request.is_empty() {
            return;
        }
        h.rpc(TAG_RESOLVE, "inocode/goal/resolveEdit", json!({ "threadId": thread, "requestId": request, "approve": approve }));
    });
    app.global::<GoalState>().on_evidence_close({
        let app = app.as_weak();
        move || {
            D.with(|d| {
                let mut d = d.borrow_mut();
                let key = d.shown.clone();
                if !key.is_empty() {
                    d.closed.push(key);
                }
            });
            if let Some(app) = app.upgrade() {
                app.global::<GoalState>().set_evidence_open(false);
            }
        }
    });
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn payload(v: &Value) -> &Value {
    v.get("response").unwrap_or(v)
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    match event {
        Event::ThreadActive { id, .. } => {
            let changed = D.with(|d| {
                let mut d = d.borrow_mut();
                let changed = d.thread_id != *id;
                d.thread_id = id.clone();
                changed
            });
            if changed {
                clear_edit(app);
                app.global::<GoalState>().set_evidence_open(false);
            }
            if !id.is_empty() {
                host.rpc(TAG_PENDING, "inocode/goal/pendingEdit", json!({ "threadId": id }));
                host.rpc(TAG_EVIDENCE, "inocode/goal/completionEvidence", json!({ "threadId": id }));
            }
        }
        Event::Notify { method, params } if method == "inocode/goal/editRequested" => {
            if for_this_thread(params) {
                show_edit(app, params);
            }
        }
        Event::Notify { method, params } if method == "inocode/goal/completionEvidence" => {
            if for_this_thread(params) {
                show_evidence(app, params);
            }
        }
        Event::Rpc { tag, result } if tag == TAG_PENDING => {
            if let Ok(v) = result {
                match payload(v).get("request") {
                    Some(r) if r.is_object() => show_edit(app, r),
                    _ => clear_edit(app),
                }
            }
        }
        Event::Rpc { tag, result } if tag == TAG_EVIDENCE => {
            if let Ok(v) = result {
                match payload(v).get("evidence") {
                    Some(e) if e.is_object() => show_evidence(app, e),
                    _ => app.global::<GoalState>().set_evidence_open(false),
                }
            }
        }
        Event::Rpc { tag, result } if tag == TAG_RESOLVE => {
            clear_edit(app);
            if result.is_err() {
                // the request was replaced by a newer one: ask for the one that waits now
                let thread = D.with(|d| d.borrow().thread_id.clone());
                if !thread.is_empty() {
                    host.rpc(TAG_PENDING, "inocode/goal/pendingEdit", json!({ "threadId": thread }));
                }
            }
        }
        _ => {}
    }
}

fn for_this_thread(params: &Value) -> bool {
    let t = s(params, "threadId");
    D.with(|d| {
        let d = d.borrow();
        d.thread_id.is_empty() || t.is_empty() || d.thread_id == t
    })
}

fn clear_edit(app: &AppWindow) {
    D.with(|d| {
        let mut d = d.borrow_mut();
        d.request_id.clear();
        d.request_thread.clear();
    });
    let gs = app.global::<GoalState>();
    gs.set_edit_open(false);
    gs.set_edit_diff(ModelRc::from(Rc::new(VecModel::from(Vec::<DiffRow>::new()))));
}

fn lines(v: Vec<GoalLine>) -> ModelRc<GoalLine> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn line(text: impl Into<String>, kind: i32) -> GoalLine {
    GoalLine { text: text.into().into(), kind }
}

fn show_edit(app: &AppWindow, r: &Value) {
    let request_id = s(r, "requestId").to_string();
    if request_id.is_empty() {
        return;
    }
    D.with(|d| {
        let mut d = d.borrow_mut();
        d.request_id = request_id;
        d.request_thread = s(r, "threadId").to_string();
    });
    let gs = app.global::<GoalState>();
    gs.set_edit_reason(format!("理由(モデルの主張): {}", s(r, "reason")).into());
    let rows = crate::diff_rows::rows(&unified_diff(s(r, "currentObjective"), s(r, "newObjective")));
    gs.set_edit_diff(ModelRc::from(Rc::new(VecModel::from(rows))));
    gs.set_edit_meta(usage_line(r).into());
    gs.set_edit_open(true);
}

/// The current and the new text as one unified diff of a "file" named 目標, in full: the rows of
/// the chat's diff then show it the way they show an edit (heading with +n −n, numbered lines).
pub fn unified_diff(current: &str, new: &str) -> String {
    let a: Vec<&str> = current.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // longest common subsequence of lines
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut body = String::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            body.push_str(&format!(" {}
", a[i]));
            i += 1;
            j += 1;
        } else if i < a.len() && (j >= b.len() || t[i + 1][j] >= t[i][j + 1]) {
            body.push_str(&format!("-{}
", a[i]));
            i += 1;
        } else {
            body.push_str(&format!("+{}
", b[j]));
            j += 1;
        }
    }
    format!("diff --git a/目標 b/目標
--- a/目標
+++ b/目標
@@ -1,{} +1,{} @@
{}", a.len(), b.len(), body)
}

fn status_label(st: &str) -> &str {
    match st {
        "active" => "進行中",
        "paused" => "一時停止",
        "budgetLimited" => "予算に達した",
        "complete" => "完了",
        other => other,
    }
}

fn group(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn span(secs: i64) -> String {
    if secs >= 3600 {
        format!("{}時間{}分", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{}分{}秒", secs / 60, secs % 60)
    } else {
        format!("{secs}秒")
    }
}

fn usage_line(r: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !s(r, "status").is_empty() {
        parts.push(format!("状態 {}", status_label(s(r, "status"))));
    }
    if let Some(used) = r.get("tokensUsed").and_then(Value::as_i64) {
        match r.get("tokenBudget").and_then(Value::as_i64) {
            Some(b) => parts.push(format!("使用 {} / {} トークン", group(used), group(b))),
            None => parts.push(format!("使用 {} トークン", group(used))),
        }
    }
    if let Some(t) = r.get("timeUsedSeconds").and_then(Value::as_i64) {
        parts.push(format!("経過 {}", span(t)));
    }
    parts.join(" · ")
}

fn claim_label(c: &str) -> &str {
    match c {
        "met" => "満たした",
        "partial" | "partially_met" => "一部を満たした",
        "unmet" | "not_met" => "満たしていない",
        other => other,
    }
}

fn kind_label(k: &str) -> &str {
    match k {
        "file" => "ファイル",
        "command" => "コマンド",
        "file_change" => "ファイル変更",
        other => other,
    }
}

fn outcome_label(o: &str) -> &str {
    match o {
        "completed" => "完了",
        "failed" => "失敗",
        "declined" => "拒否された",
        other => other,
    }
}

fn show_evidence(app: &AppWindow, e: &Value) {
    let key = format!("{}{}", s(e, "goalId"), e.get("recordedAt").and_then(Value::as_i64).unwrap_or(0));
    if D.with(|d| d.borrow().closed.contains(&key)) {
        return;
    }
    D.with(|d| d.borrow_mut().shown = key);
    // what the model said, in its own words
    let mut claims: Vec<GoalLine> = Vec::new();
    for r in e.get("requirements").and_then(Value::as_array).into_iter().flatten() {
        claims.push(line(format!("{} — {}", s(r, "requirement"), claim_label(s(r, "claim"))), 3));
        for ev in r.get("evidence").and_then(Value::as_array).into_iter().flatten() {
            let mut t = format!("{}  {}", kind_label(s(ev, "kind")), s(ev, "detail"));
            if let Some(code) = ev.get("exitCode").and_then(Value::as_i64) {
                t.push_str(&format!("  終了コード {code}"));
            }
            claims.push(line(t, 4));
        }
    }
    // what the host saw
    let mut facts: Vec<GoalLine> = Vec::new();
    for f in e.get("hostFacts").and_then(Value::as_array).into_iter().flatten() {
        let ok = f.get("success").and_then(Value::as_bool);
        let result = match ok {
            Some(true) => format!("{} · 成功", outcome_label(s(f, "outcome"))),
            Some(false) => format!("{} · 失敗", outcome_label(s(f, "outcome"))),
            None => outcome_label(s(f, "outcome")).to_string(),
        };
        let mut t = format!("{}  {}  {}", kind_label(s(f, "kind")), s(f, "detail"), result);
        if let Some(code) = f.get("exitCode").and_then(Value::as_i64) {
            t.push_str(&format!("  終了コード {code}"));
        }
        facts.push(line(t, if ok == Some(false) { 2 } else { 0 }));
    }
    let gs = app.global::<GoalState>();
    gs.set_evidence_expanded(false);
    gs.set_evidence_objective(s(e, "objective").into());
    gs.set_claims(lines(claims));
    gs.set_facts(lines(facts));
    gs.set_evidence_open(true);
}
