//! 履歴の取り込み(設定ページの「履歴の取り込み」)と、項目の抽出。
//!
//! - 見つける: `inocode/import/detect` が、Claude Code と Codex のセッションと、メモリ・指示のファイルを返す。
//! - 取り込む: 選んだセッションを 1 件ずつ `inocode/import/run`。取り込みでは抽出しない。
//! - 取り込んだセッションを初めて開いたとき(`inocode/import/state` の `ask`)、チャットの下に 1 行で尋ねる。
//!   「抽出する」は、そのセッションでモデルに、InoCode の道具(kb_propose、record_evidence、kb_prohibit)で
//!   記録させる 1 回の発言。
//! - メモリ・指示のファイルの「抽出」は、そのプロジェクトで新しいセッションを開き、ファイルの中身を渡して同じことを頼む。

use crate::client::Command;
use crate::{AppState, AppWindow, ImportRow, ImportState, MemoryRow, SettingsPageState};
use serde_json::{json, Value};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

#[derive(Default)]
struct Data {
    sessions: Vec<Value>,
    memories: Vec<Value>,
    checked: Vec<bool>,
    status: Vec<String>,
    queue: VecDeque<usize>,
    total: usize,
    done: usize,
    failed: usize,
    /// The message to send in the session that is being opened for a memory file.
    pending_prompt: Option<String>,
    /// The imported thread being asked about: (thread id, source label).
    asking: Option<(String, String)>,
    context_tokens: i64,
}

thread_local! {
    static D: RefCell<Data> = RefCell::new(Data::default());
}

fn rpc(tag: &str, method: &str, params: Value) {
    crate::settings_page::rpc(tag, method, params);
}

fn source_label(s: &str) -> &'static str {
    match s {
        "claude" => "Claude Code",
        "codex" => "Codex",
        _ => "",
    }
}

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

/// Seconds since the epoch -> "2026-11-14" (UTC).
fn date_label(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn last_part(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

fn status_of(v: &Value) -> (String, bool) {
    if v.get("threadId").and_then(Value::as_str).is_some() {
        ("取り込み済み".into(), true)
    } else if v.get("changed").and_then(Value::as_bool) == Some(true) {
        ("変更あり".into(), false)
    } else {
        (String::new(), false)
    }
}

fn render(app: &AppWindow) {
    D.with(|d| {
        let d = d.borrow();
        let rows: Vec<ImportRow> = d
            .sessions
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let path = v.get("path").and_then(Value::as_str).unwrap_or_default();
                let (mut status, imported) = status_of(v);
                if let Some(s) = d.status.get(i).filter(|s| !s.is_empty()) {
                    status = s.clone();
                }
                ImportRow {
                    source: source_label(v.get("source").and_then(Value::as_str).unwrap_or_default()).into(),
                    project: v.get("cwd").and_then(Value::as_str).map(last_part).unwrap_or_default().into(),
                    title: v.get("title").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| last_part(path)).into(),
                    date: date_label(v.get("modified").and_then(Value::as_i64).unwrap_or(0)).into(),
                    size: size_label(v.get("sizeBytes").and_then(Value::as_u64).unwrap_or(0)).into(),
                    status: status.into(),
                    imported,
                    checked: d.checked.get(i).copied().unwrap_or(false),
                }
            })
            .collect();
        let mems: Vec<MemoryRow> = d
            .memories
            .iter()
            .map(|v| {
                let path = v.get("path").and_then(Value::as_str).unwrap_or_default();
                let parent = std::path::Path::new(path).parent().map(|p| last_part(&p.to_string_lossy())).unwrap_or_default();
                MemoryRow {
                    source: source_label(v.get("source").and_then(Value::as_str).unwrap_or_default()).into(),
                    name: format!("{parent}/{}", last_part(path)).into(),
                    project: v.get("cwd").and_then(Value::as_str).map(last_part).unwrap_or_default().into(),
                    date: date_label(v.get("modified").and_then(Value::as_i64).unwrap_or(0)).into(),
                    size: size_label(v.get("sizeBytes").and_then(Value::as_u64).unwrap_or(0)).into(),
                }
            })
            .collect();
        let st = app.global::<ImportState>();
        st.set_checked_count(d.checked.iter().filter(|c| **c).count() as i32);
        st.set_sessions(ModelRc::from(Rc::new(VecModel::from(rows))));
        st.set_memories(ModelRc::from(Rc::new(VecModel::from(mems))));
    });
}

/// The section was opened: find what can be imported.
pub(crate) fn open(app: &AppWindow) {
    let st = app.global::<ImportState>();
    if st.get_running() {
        return;
    }
    st.set_loading(true);
    rpc("spage.import.detect", "inocode/import/detect", json!({}));
}

fn next(app: &AppWindow) {
    let st = app.global::<ImportState>();
    let job = D.with(|d| {
        let mut d = d.borrow_mut();
        let i = d.queue.pop_front()?;
        let v = d.sessions.get(i)?.clone();
        d.status[i] = "取り込み中…".into();
        Some((i, v, d.done + 1, d.total))
    });
    let Some((i, v, n, total)) = job else {
        let (done, failed) = D.with(|d| (d.borrow().done, d.borrow().failed));
        st.set_running(false);
        st.set_progress(if failed > 0 {
            format!("{} 件を取り込みました。失敗 {failed} 件", done - failed).into()
        } else {
            format!("{done} 件を取り込みました").into()
        });
        crate::sidebar::refresh();
        render(app);
        return;
    };
    st.set_progress(format!("取り込み中 {n} / {total}").into());
    render(app);
    rpc(
        &format!("spage.import.run:{i}"),
        "inocode/import/run",
        json!({ "path": v.get("path"), "source": v.get("source") }),
    );
}

fn extraction_prompt(source: &str) -> String {
    format!(
        "このセッションは {source} から取り込んだ履歴です。履歴を読み、後で役に立つものを InoCode の道具で記録してください。\n{}",
        RECORD_WHAT
    )
}

const RECORD_WHAT: &str = "- 失敗した操作(実行したこと、結果、失敗した条件、避け方): kb_propose(kind: failure)\n\
- 調べて分かった事実や、まとめておく価値のある内容: kb_propose(kind: fact または page)\n\
- 出典(URL やファイル)のある主張: record_evidence\n\
- ユーザーが「今後〜しない」のように決めた決まり: kb_prohibit(ユーザーの言葉をそのまま引用)\n\
先に search_knowledge と search_evidence で既にあるものを確かめ、同じものは記録しないでください。最後に、記録したものを短く報告してください。";

fn close_settings(app: &AppWindow) {
    app.global::<SettingsPageState>().set_open(false);
}

pub(crate) fn install(app: &AppWindow) {
    let st = app.global::<ImportState>();
    let weak = app.as_weak();
    st.on_detect(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
    let weak = app.as_weak();
    st.on_toggle(move |i| {
        let Some(app) = weak.upgrade() else { return };
        D.with(|d| {
            if let Some(c) = d.borrow_mut().checked.get_mut(i as usize) {
                *c = !*c;
            }
        });
        render(&app);
    });
    let weak = app.as_weak();
    st.on_toggle_all(move || {
        let Some(app) = weak.upgrade() else { return };
        D.with(|d| {
            let mut d = d.borrow_mut();
            // All the ones not imported yet; again: none.
            let open: Vec<bool> = d.sessions.iter().map(|v| !status_of(v).1).collect();
            let all = d.checked.iter().zip(&open).filter(|(_, o)| **o).all(|(c, _)| *c);
            d.checked = open.iter().map(|o| *o && !all).collect();
        });
        render(&app);
    });
    let weak = app.as_weak();
    st.on_run(move || {
        let Some(app) = weak.upgrade() else { return };
        let n = D.with(|d| {
            let mut d = d.borrow_mut();
            let picked: VecDeque<usize> = d.checked.iter().enumerate().filter(|(_, c)| **c).map(|(i, _)| i).collect();
            d.total = picked.len();
            d.done = 0;
            d.failed = 0;
            d.queue = picked;
            d.total
        });
        if n == 0 {
            return;
        }
        app.global::<ImportState>().set_running(true);
        next(&app);
    });
    let weak = app.as_weak();
    st.on_open(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let id = D.with(|d| d.borrow().sessions.get(i as usize).and_then(|v| v.get("threadId")).and_then(Value::as_str).map(str::to_string));
        if let Some(id) = id {
            close_settings(&app);
            app.global::<AppState>().invoke_open_thread(id.into());
        }
    });
    let weak = app.as_weak();
    st.on_extract_memory(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some(m) = D.with(|d| d.borrow().memories.get(i as usize).cloned()) else { return };
        let path = m.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
        let source = source_label(m.get("source").and_then(Value::as_str).unwrap_or_default());
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                app.global::<ImportState>().set_progress(format!("{path} を読めません: {e}").into());
                return;
            }
        };
        let cwd = m
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| crate::state::STATE.with(|s| s.borrow().active_cwd.clone()));
        let prompt = format!(
            "次のファイルは {source} のメモリまたは指示のファイルです({path})。内容を読み、後で役に立つものを InoCode の道具で記録してください。\n{RECORD_WHAT}\n\n```markdown\n{text}\n```"
        );
        D.with(|d| d.borrow_mut().pending_prompt = Some(prompt));
        close_settings(&app);
        crate::settings_page::send(Command::NewThreadIn(cwd));
    });
    let weak = app.as_weak();
    st.on_extract(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some((id, source)) = D.with(|d| d.borrow_mut().asking.take()) else { return };
        app.global::<ImportState>().set_ask(false);
        rpc("spage.import.decide", "inocode/import/decide", json!({ "threadId": id, "decision": "started" }));
        app.invoke_submit(extraction_prompt(&source).into());
    });
    for (decision, quiet) in [("later", false), ("declined", true)] {
        let weak = app.as_weak();
        let f = move || {
            let Some(app) = weak.upgrade() else { return };
            let Some((id, _)) = D.with(|d| d.borrow_mut().asking.take()) else { return };
            app.global::<ImportState>().set_ask(false);
            rpc("spage.import.decide", "inocode/import/decide", json!({ "threadId": id, "decision": decision }));
        };
        if quiet {
            st.on_quiet(f);
        } else {
            st.on_later(f);
        }
    }
}

/// A thread became the active one: a new session opened for a memory file gets its message; an
/// imported one is asked about once.
pub(crate) fn on_thread_active(app: &AppWindow, id: &str, title: &str) {
    let st = app.global::<ImportState>();
    st.set_ask(false);
    D.with(|d| d.borrow_mut().asking = None);
    if title.is_empty() {
        if let Some(prompt) = D.with(|d| d.borrow_mut().pending_prompt.take()) {
            app.invoke_submit(prompt.into());
        }
        return;
    }
    rpc(&format!("spage.import.state:{id}"), "inocode/import/state", json!({ "threadId": id }));
}

fn ask_text(source: &str, estimate: &str, tokens: i64) -> String {
    let mut t = format!("このセッションは {source} から取り込みました。失敗・事実・出典・禁止事項を抽出しますか?");
    if tokens > 0 {
        let size = if tokens >= 10_000 { format!("約 {} 万トークン", tokens / 10_000) } else { format!("約 {tokens} トークン") };
        t.push_str(&format!(" 入力 {size}"));
        if !estimate.is_empty() {
            t.push_str(&format!(": {estimate}"));
        }
    }
    t
}

/// Replies of the `spage.import.*` requests. True when it was one of them.
pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) -> bool {
    let Some(rest) = tag.strip_prefix("spage.import.") else { return false };
    let st = app.global::<ImportState>();
    if rest == "detect" {
        st.set_loading(false);
        match result {
            Ok(v) => {
                let sessions = v.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
                let memories = v.get("memories").and_then(Value::as_array).cloned().unwrap_or_default();
                D.with(|d| {
                    let mut d = d.borrow_mut();
                    d.checked = vec![false; sessions.len()];
                    d.status = vec![String::new(); sessions.len()];
                    d.sessions = sessions;
                    d.memories = memories;
                });
                st.set_progress("".into());
            }
            Err(e) => st.set_progress(format!("見つけられません: {e}").into()),
        }
        render(app);
    } else if let Some(i) = rest.strip_prefix("run:").and_then(|i| i.parse::<usize>().ok()) {
        D.with(|d| {
            let mut d = d.borrow_mut();
            d.done += 1;
            match result {
                Ok(v) => {
                    if let Some(s) = d.sessions.get_mut(i) {
                        if let (Some(o), Some(t)) = (s.as_object_mut(), v.get("threadId").cloned()) {
                            o.insert("threadId".into(), t);
                            o.insert("changed".into(), Value::Bool(false));
                        }
                    }
                    d.status[i] = String::new();
                    d.checked[i] = false;
                }
                Err(e) => {
                    d.failed += 1;
                    d.status[i] = e.clone();
                }
            }
        });
        next(app);
    } else if let Some(id) = rest.strip_prefix("state:") {
        let Ok(v) = result else { return true };
        if v.get("ask").and_then(Value::as_bool) != Some(true) {
            return true;
        }
        let source = source_label(v.get("source").and_then(Value::as_str).unwrap_or_default()).to_string();
        let tokens = v.get("contextTokens").and_then(Value::as_i64).unwrap_or(0);
        D.with(|d| {
            let mut d = d.borrow_mut();
            d.asking = Some((id.to_string(), source.clone()));
            d.context_tokens = tokens;
        });
        st.set_ask_text(ask_text(&source, "", tokens).into());
        st.set_ask(true);
        // What it would take of the windows, from the recent use of the chosen model.
        let model = crate::state::STATE.with(|s| {
            let s = s.borrow();
            let visible = crate::state::visible_models(&s);
            crate::state::selected_model(&s, &visible).map(|m| m.id.clone())
        });
        if let (Some(model), true) = (model, tokens > 0) {
            rpc(
                "spage.import.estimate",
                "inocode/stats/estimate",
                json!({ "model": model, "days": 3, "tokens": { "uncachedInput": tokens, "cachedInput": 0, "output": 0 } }),
            );
        }
    } else if rest == "estimate" {
        let Ok(v) = result else { return true };
        let Some((_, source)) = D.with(|d| d.borrow().asking.clone()) else { return true };
        let tokens = D.with(|d| d.borrow().context_tokens);
        st.set_ask_text(ask_text(&source, &crate::tabs::estimate_label(v), tokens).into());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_sizes_and_the_question() {
        assert_eq!(date_label(1_794_663_921), "2026-11-14");
        assert_eq!(size_label(3 * (1 << 20) + 1), "3.0 MB");
        assert_eq!(last_part("D:\\work\\ino-code\\"), "ino-code");
        assert_eq!(
            ask_text("Codex", "5時間枠 約 1%、週枠 約 0.2%、約 30 秒", 123_456),
            "このセッションは Codex から取り込みました。失敗・事実・出典・禁止事項を抽出しますか? 入力 約 12 万トークン: 5時間枠 約 1%、週枠 約 0.2%、約 30 秒"
        );
    }
}
