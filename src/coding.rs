//! Basic coding: the approvals that wait per thread, a folder that cannot be written (the
//! sandbox runs read-only), the trust setting of a folder, the folder that was used last, and
//! reconnecting by hand.

use crate::state::*;
use crate::*;
use serde_json::{json, Value};
use std::cell::RefCell;

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ pure helpers (tested)

/// The approval of the screen from what the client collected.
pub(crate) fn to_approval(info: &client::ApprovalInfo) -> Approval {
    let lines: Vec<ALine> = info.lines.iter().map(|(t, k)| ALine { text: t.as_str().into(), kind: *k }).collect();
    Approval {
        key: info.key.as_str().into(),
        title: info.title.as_str().into(),
        detail: info.detail.as_str().into(),
        summary: info.summary.as_str().into(),
        lines: ModelRc::from(Rc::new(VecModel::from(lines))),
        long: info.long,
        simple: info.simple,
        persist_always: info.persist_always,
    }
}

/// Add a pending approval (a request that is sent again after a resume replaces the old card).
pub(crate) fn push_pending(list: &mut Vec<(String, Approval)>, thread: &str, a: Approval) {
    list.retain(|(_, x)| x.key != a.key);
    list.push((thread.to_string(), a));
}

/// The approvals shown for `active`: those of that thread (and those that name no thread).
pub(crate) fn visible(list: &[(String, Approval)], active: &str) -> Vec<Approval> {
    list.iter().filter(|(t, _)| t.is_empty() || t == active).map(|(_, a)| a.clone()).collect()
}

/// config/value/write key of a folder's trust: projects."<path>".trust_level. The path is a
/// quoted segment, so dots stay in it, and backslashes and quotes are escaped.
pub(crate) fn trust_key_path(path: &str) -> String {
    format!("projects.\"{}\".trust_level", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Why a folder cannot be written, which decides the words and the button of the banner.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Cause {
    /// The Windows sandbox is not set up (or needs an update).
    Setup,
    /// The set-up could not be read.
    Retry,
    /// The folder is not trusted.
    Trust,
    /// Everything is in place and the thread still runs read-only (a permission restored by a
    /// resume, a requirement): a new chat starts with the new state.
    Fresh,
}

/// The text over the composer and the label of its button. The words of the set-up are the ones
/// of Codex Desktop's banner (codex.windowsSandboxBanner.*); the trust is Claude Desktop's
/// ("Trust workspace").
pub(crate) fn sandbox_banner(cause: Cause) -> (&'static str, &'static str) {
    match cause {
        Cause::Setup => ("このフォルダは書き込めません。続けるには、Windows のセットアップを完了してください。", "続ける"),
        Cause::Retry => ("Windows のセットアップを確認できませんでした。", "もう一度試す"),
        Cause::Trust => ("このフォルダは書き込めません。ここで作業するには、このフォルダを信頼してください。", "ワークスペースを信頼"),
        Cause::Fresh => ("このスレッドは読み取り専用で始まっています。", "新しいチャットで始める"),
    }
}

/// Whether the folder is trusted in a config/read reply (projects."<path>".trust_level).
pub(crate) fn is_trusted(config: &Value, cwd: &str) -> bool {
    let norm = |p: &str| p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let want = norm(cwd);
    config
        .get("projects")
        .and_then(Value::as_object)
        .map(|m| m.iter().any(|(k, v)| norm(k) == want && v.get("trust_level").and_then(Value::as_str) == Some("trusted")))
        .unwrap_or(false)
}

/// The cause from what was read: the Windows set-up state (None off Windows) and the trust.
pub(crate) fn cause_of(windows: bool, readiness: Option<&Result<String, String>>, trusted: bool) -> Cause {
    if windows {
        match readiness {
            Some(Ok(status)) if status == "ready" => {}
            Some(Ok(_)) => return Cause::Setup,
            _ => return Cause::Retry,
        }
    }
    if trusted {
        Cause::Fresh
    } else {
        Cause::Trust
    }
}

thread_local! {
    static CHECK: RefCell<Check> = RefCell::new(Check::default());
}

/// What is read to tell the cause (a banner is shown only once both answers are in).
#[derive(Default)]
struct Check {
    cwd: String,
    readiness: Option<Result<String, String>>,
    trusted: Option<bool>,
    cause: Option<Cause>,
}

fn rpc(tag: &str, method: &str, params: Value) {
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(Command::Rpc { tag: tag.into(), method: method.into(), params });
    }
}

/// Read the state that tells why the folder cannot be written.
fn start_check(app: &AppWindow, cwd: &str) {
    CHECK.with(|c| *c.borrow_mut() = Check { cwd: cwd.to_string(), ..Check::default() });
    app.global::<AppState>().set_sandbox_notice("".into());
    if cfg!(windows) {
        rpc("coding.cause.readiness", "windowsSandbox/readiness", Value::Null);
    } else {
        CHECK.with(|c| c.borrow_mut().readiness = Some(Ok("ready".into())));
    }
    rpc("coding.cause.config", "config/read", json!({ "includeLayers": false }));
}

fn show(app: &AppWindow, cause: Cause) {
    let (text, action) = sandbox_banner(cause);
    CHECK.with(|c| c.borrow_mut().cause = Some(cause));
    let s = app.global::<AppState>();
    s.set_sandbox_notice(text.into());
    s.set_sandbox_action(action.into());
}

/// Both answers are in: the banner of the cause.
fn classify(app: &AppWindow) {
    let (readiness, trusted) = CHECK.with(|c| {
        let c = c.borrow();
        (c.readiness.clone(), c.trusted)
    });
    let (Some(_), Some(trusted)) = (readiness.as_ref(), trusted) else { return };
    show(app, cause_of(cfg!(windows), readiness.as_ref(), trusted));
}

// ------------------------------------------------------------------ screen

pub(crate) fn render_approvals(app: &AppWindow, st: &State) {
    let rows = visible(&st.pending, &st.active);
    app.global::<AppState>().set_approvals(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// The server told what the thread runs with.
pub(crate) fn on_applied(app: &AppWindow, st: &mut State, sandbox: &str, cwd: &str) {
    // Writing was meant (workspace-write or full access) but the server runs it read-only.
    let downgraded = !sandbox.is_empty() && sandbox == "readOnly" && st.settings.wants_write();
    if downgraded {
        start_check(app, cwd);
    } else {
        app.global::<AppState>().set_sandbox_notice("".into());
    }
    // The folder used last is where the next new chat starts, also after a restart.
    if !cwd.is_empty() && st.settings.cwd.as_deref() != Some(cwd) {
        st.settings.cwd = Some(cwd.to_string());
        gui_prefs::save_last_cwd(cwd);
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            h.send(Command::SetSettings(st.settings.clone()));
        }
    }
}

pub(crate) fn on_rpc(app: &AppWindow, tag: &str, result: &Result<Value, String>) {
    let s = app.global::<AppState>();
    match (tag, result) {
        // The answers that tell the cause.
        ("coding.cause.readiness", r) => {
            let status = r.as_ref().map(|v| v.get("status").and_then(Value::as_str).unwrap_or("").to_string()).map_err(|e| e.clone());
            CHECK.with(|c| c.borrow_mut().readiness = Some(status));
            classify(app);
        }
        ("coding.cause.config", r) => {
            let cwd = CHECK.with(|c| c.borrow().cwd.clone());
            let trusted = r.as_ref().map(|v| is_trusted(v.get("config").unwrap_or(v), &cwd)).unwrap_or(false);
            CHECK.with(|c| c.borrow_mut().trusted = Some(trusted));
            classify(app);
        }
        // The button of the set-up: ready needs nothing, else the set-up is started.
        ("coding.sandbox.readiness", Ok(v)) => {
            if v.get("status").and_then(Value::as_str) == Some("ready") {
                let cwd = CHECK.with(|c| c.borrow().cwd.clone());
                start_check(app, &cwd);
            } else {
                let cwd = STATE.with(|st| st.borrow().active_cwd.clone());
                let mut p = json!({ "mode": "elevated" });
                if !cwd.is_empty() {
                    p["cwd"] = json!(cwd);
                }
                rpc("coding.sandbox.setup", "windowsSandbox/setupStart", p);
                s.set_sandbox_action("".into());
                s.set_sandbox_notice("Windows のセットアップを完了しています".into());
            }
        }
        ("coding.sandbox.readiness", Err(_)) => show(app, Cause::Retry),
        ("coding.sandbox.setup", Err(_)) => {
            s.set_sandbox_notice("Windows のセットアップが完了しませんでした".into());
            s.set_sandbox_action("セットアップを再試行".into());
            CHECK.with(|c| c.borrow_mut().cause = Some(Cause::Setup));
        }
        // The trust was written (chat.trust is the sidebar's answer): a new chat starts with it.
        ("chat.trust", Ok(_)) if CHECK.with(|c| c.borrow().cause) == Some(Cause::Trust) => new_chat_here(app),
        ("chat.trust", Err(_)) if CHECK.with(|c| c.borrow().cause) == Some(Cause::Trust) => {
            s.set_sandbox_notice("設定の保存に失敗しました".into());
            s.set_sandbox_action("".into());
        }
        _ => {}
    }
}

/// A new chat in the folder of the banner (it takes what is in place now), and the banner goes.
fn new_chat_here(app: &AppWindow) {
    let cwd = CHECK.with(|c| c.borrow().cwd.clone());
    let s = app.global::<AppState>();
    s.set_sandbox_notice("".into());
    s.set_sandbox_action("".into());
    if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
        h.send(if cwd.is_empty() { Command::NewThread } else { Command::NewThreadIn(cwd) });
    }
}

pub(crate) fn on_notify(app: &AppWindow, method: &str, params: &Value) {
    if method == "windowsSandbox/setupCompleted" {
        let s = app.global::<AppState>();
        if params.get("success").and_then(Value::as_bool).unwrap_or(false) {
            // Set up: the next thing is a new chat that has it.
            s.set_sandbox_notice("Windows のセットアップが完了しました".into());
            s.set_sandbox_action("新しいチャットで始める".into());
            CHECK.with(|c| c.borrow_mut().cause = Some(Cause::Fresh));
        } else {
            let e = params.get("error").and_then(Value::as_str).unwrap_or("");
            let text = if e.is_empty() { "Windows のセットアップが完了しませんでした".to_string() } else { format!("Windows のセットアップが完了しませんでした: {e}") };
            s.set_sandbox_notice(text.into());
            s.set_sandbox_action("セットアップを再試行".into());
            CHECK.with(|c| c.borrow_mut().cause = Some(Cause::Setup));
        }
    }
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let st = app.global::<AppState>();
    let h = handle.clone();
    st.on_reconnect(move || h.send(Command::Reconnect));
    let weak = app.as_weak();
    st.on_sandbox_setup(move || {
        let Some(app) = weak.upgrade() else { return };
        let (cause, cwd) = CHECK.with(|c| {
            let c = c.borrow();
            (c.cause, c.cwd.clone())
        });
        match cause {
            Some(Cause::Setup) => rpc("coding.sandbox.readiness", "windowsSandbox/readiness", Value::Null),
            Some(Cause::Retry) => start_check(&app, &cwd),
            // The same question as for a folder that was added: the path is shown, and only an
            // answer of yes writes the trust.
            Some(Cause::Trust) => crate::sidebar::ask_trust(&app, &cwd),
            Some(Cause::Fresh) => new_chat_here(&app),
            None => {}
        }
    });
    if let Some(which) = std::env::var("INOCODE_GUI_DEMO").ok().filter(|v| v == "1" || v == "2") {
        // After the connection came up (it clears the requests of the old one).
        let weak = app.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_secs(6), move || {
            if let Some(app) = weak.upgrade() {
                demo(&app, which == "2");
            }
        });
    }
    let weak = app.as_weak();
    st.on_sandbox_dismiss(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<AppState>().set_sandbox_notice("".into());
        }
    });
}

/// Mock only (INOCODE_GUI_DEMO=1: the cards, 2: the notices): the cards of the three kinds of requests, a read-only folder
/// and a lost connection, so that the screen can be looked at without a server.
fn demo(app: &AppWindow, notices: bool) {
    if notices {
        let s = app.global::<AppState>();
        let (text, action) = sandbox_banner(match std::env::var("INOCODE_GUI_DEMO_CAUSE").as_deref() {
            Ok("trust") => Cause::Trust,
            Ok("fresh") => Cause::Fresh,
            Ok("retry") => Cause::Retry,
            _ => Cause::Setup,
        });
        s.set_sandbox_notice(text.into());
        s.set_sandbox_action(action.into());
        s.set_disconnected(true);
        return;
    }
    let changes = json!([
        { "path": "src/main.rs", "kind": { "type": "update" }, "diff": "@@ -1,4 +1,5 @@\n fn main() {\n-    println!(\"hello\");\n+    let name = \"world\";\n+    println!(\"hello {name}\");\n }" },
        { "path": "docs/new.md", "kind": { "type": "add" }, "diff": "# 新しい文書\n\n本文" },
    ]);
    let cmd = format!("cargo build --release --target x86_64-pc-windows-msvc && copy target\\release\\app.exe D:\\deploy\\app.exe\ncd D:\\deploy\nstart app.exe --config {}", "x".repeat(260));
    let reqs = [
        ("item/commandExecution/requestApproval", json!({ "threadId": "", "command": cmd, "cwd": "D:/work/demo", "reason": "リリースビルドを配置します" }), None),
        ("item/fileChange/requestApproval", json!({ "threadId": "", "reason": "main.rs を直し、文書を足します", "grantRoot": "D:/work/demo" }), Some(changes)),
        ("item/permissions/requestApproval", json!({ "threadId": "", "cwd": "D:/work/demo", "reason": "ビルドの出力先に書き込みます", "permissions": { "network": { "enabled": true }, "fileSystem": { "write": ["D:/work/demo/out"] } } }), None),
    ];
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        for (i, (method, params, changes)) in reqs.iter().enumerate() {
            if let Some(mut info) = client::approval_info(method, params, changes.as_ref()) {
                info.key = format!("demo{i}");
                push_pending(&mut st.pending, "", to_approval(&info));
            }
        }
        render_approvals(app, &st);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(key: &str) -> Approval {
        Approval { key: key.into(), ..Approval::default() }
    }

    #[test]
    fn approvals_wait_per_thread() {
        let mut list = Vec::new();
        push_pending(&mut list, "a", approval("1"));
        push_pending(&mut list, "b", approval("2"));
        assert_eq!(visible(&list, "a").len(), 1);
        assert_eq!(visible(&list, "a")[0].key, "1");
        // Switching away and back: the card of the first thread is still there.
        assert_eq!(visible(&list, "b")[0].key, "2");
        assert_eq!(visible(&list, "a")[0].key, "1");
        // The same request sent again after a resume is one card.
        push_pending(&mut list, "a", approval("1"));
        assert_eq!(list.len(), 2);
        // A request that names no thread is shown on every thread.
        push_pending(&mut list, "", approval("3"));
        assert_eq!(visible(&list, "zzz").len(), 1);
    }

    #[test]
    fn trust_key_path_quotes_the_folder() {
        assert_eq!(trust_key_path("D:\\work\\my.app"), "projects.\"D:\\\\work\\\\my.app\".trust_level");
        assert_eq!(trust_key_path("/home/a"), "projects.\"/home/a\".trust_level");
    }

    #[test]
    fn the_cause_tells_the_banner() {
        let ready = Ok("ready".to_string());
        let not = Ok("notConfigured".to_string());
        let err: Result<String, String> = Err("x".into());
        assert_eq!(cause_of(true, Some(&not), true), Cause::Setup);
        assert_eq!(cause_of(true, Some(&err), true), Cause::Retry);
        assert_eq!(cause_of(true, Some(&ready), false), Cause::Trust);
        assert_eq!(cause_of(true, Some(&ready), true), Cause::Fresh);
        assert_eq!(cause_of(false, None, false), Cause::Trust);
        assert!(sandbox_banner(Cause::Setup).0.contains("Windows"));
        assert!(!sandbox_banner(Cause::Trust).0.contains("Windows"));
    }

    #[test]
    fn trust_is_read_from_the_projects_table() {
        let cfg = json!({ "projects": { "D:\\work\\app": { "trust_level": "trusted" }, "D:\\other": { "trust_level": "untrusted" } } });
        assert!(is_trusted(&cfg, "d:/work/app/"));
        assert!(!is_trusted(&cfg, "D:\\other"));
        assert!(!is_trusted(&cfg, "D:\\none"));
        assert!(!is_trusted(&json!({}), "D:\\work\\app"));
    }
}
