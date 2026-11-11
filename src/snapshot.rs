//! A way to look at the window without the mouse, the keyboard or the front: with
//! INOCODE_GUI_SNAPSHOT=<file.png> the app opens the tab INOCODE_GUI_TAB (0 = chat ... 3 = team),
//! waits, writes what the window shows (rendered by the app itself) to the file and quits.

use crate::*;
use std::cell::RefCell;
use std::time::Duration;

thread_local! {
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub(crate) fn install(app: &AppWindow) {
    let Ok(path) = std::env::var("INOCODE_GUI_SNAPSHOT") else { return };
    // INOCODE_GUI_SIZE=<width>x<height> (logical pixels): a window as tall as the screen to look at.
    if let Some((w, h)) = std::env::var("INOCODE_GUI_SIZE").ok().and_then(|v| v.split_once('x').map(|(a, b)| (a.to_string(), b.to_string()))) {
        if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
            app.window().set_size(slint::LogicalSize::new(w, h));
        }
    }
    let tab = env_num("INOCODE_GUI_TAB", 0) as i32;
    let delay = env_num("INOCODE_GUI_SNAPSHOT_DELAY", 8);
    let weak = app.as_weak();
    let open = slint::Timer::default();
    open.start(slint::TimerMode::SingleShot, Duration::from_secs(delay), move || {
        if let Some(app) = weak.upgrade() {
            app.global::<Tabs>().invoke_select(tab);
        }
    });
    // INOCODE_GUI_ATTACH=<picture>: a sent message with the picture, and chips in the composer.
    let mut extra: Vec<slint::Timer> = Vec::new();
    if let Ok(file) = std::env::var("INOCODE_GUI_ATTACH") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            if let Some(app) = weak.upgrade() {
                crate::attach::demo(&app, std::path::Path::new(&file));
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_SEND=1: messages sent while a turn runs: taken, not taken (with a retry), sent again.
    if std::env::var("INOCODE_GUI_SEND").is_ok() {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            if let Some(app) = weak.upgrade() {
                crate::prompts::demo(&app);
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_TYPE=<text>: the text in the composer as if typed (the slash menu shows for "/..");
    // INOCODE_GUI_SUBMIT=<text>: the text sent from the composer (a slash command runs).
    if let Ok(text) = std::env::var("INOCODE_GUI_TYPE") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            if let Some(app) = weak.upgrade() {
                app.set_prompt(text.clone().into());
                app.global::<AttachState>().invoke_text_changed(text.clone().into());
            }
        });
        extra.push(t);
    }
    if let Ok(text) = std::env::var("INOCODE_GUI_SUBMIT") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            if let Some(app) = weak.upgrade() {
                app.invoke_submit(text.clone().into());
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_LONG=<turns>: a long conversation (long answers with a table, code, lists, many
    // command rows) put in as the history of a thread, to look for rows that overlap or are cut off.
    if let Some(n) = std::env::var("INOCODE_GUI_LONG").ok().and_then(|v| v.parse::<usize>().ok()) {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            let Some(app) = weak.upgrade() else { return };
            let mut rows: Vec<client::Msg> = Vec::new();
            let mut push = |role: &str, text: String, detail: String| {
                let id = format!("m{}", rows.len());
                rows.push(client::Msg { id, role: role.into(), text, detail, turn_id: String::new(), item_index: 0 });
            };
            for i in 0..n {
                push("user", format!("{}番目の質問です。{}", i + 1, if i % 4 == 0 { "とても長い依頼の文章が、折り返して何行にもなるときの高さを見るために、同じ文を何度も書きます。".repeat(6) } else { String::new() }), String::new());
                push("tool", "思考しました".into(), "調べる順番を考えた。".into());
                push(
                    "tool",
                    format!("実行: Get-ChildItem -LiteralPath 'D:\\work\\software\\OSS\\ino-code\\gui\\inocode-desktop\\src' -Recurse | Select-String 'pattern{i}' — 完了"),
                    format!("$ Get-ChildItem ...\n{}", "src\\main.rs:12: match\n".repeat(30)),
                );
                let table = "| 項目 | 値 | 備考 |\n|---|---|---|\n| 速度 | 120 トークン/秒 | 実測 |\n| 価格 | $2 / $10 | 参考 |\n";
                let code = "```rust\nfn main() {\n    let long_variable_name = some_function_with_a_long_name(argument_one, argument_two, argument_three);\n    println!(\"{long_variable_name}\");\n}\n```\n";
                push(
                    "assistant",
                    format!("## 答え {}\n\n長い段落です。{}\n\n- 箇条書きの一つ目\n- 箇条書きの二つ目\n\n{table}\n{code}\n最後の一文。", i + 1, "文が続きます。".repeat(20 + (i % 5) * 10)),
                    String::new(),
                );
            }
            STATE.with(|s| crate::events::apply(&app, s, client::Event::History(rows)));
        });
        extra.push(t);
    }
    // INOCODE_GUI_THINK=1: thinking and search rows (running, done, with a body), through the
    // same functions the server's items go through; INOCODE_GUI_THINK=busy also shows the turn running.
    if let Ok(mode) = std::env::var("INOCODE_GUI_THINK") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            let Some(app) = weak.upgrade() else { return };
            app.global::<AppState>().set_tools_open(true);
            let rows = [
                serde_json::json!({"type":"reasoning","id":"r1","startedAtMs":0,"completedAtMs":4200,"summary":["ファイルの構成を確認してから、テストを直す方針にする。"],"content":[]}),
                serde_json::json!({"type":"webSearch","id":"w1","query":"","action":null,"results":null}),
                serde_json::json!({"type":"webSearch","id":"w2","query":"Tesla V100 CUDA 12","action":{"type":"search","query":"Tesla V100 CUDA 12"},"tool":"web_search","provider":"yahoo_jp","results":[{"engine":"bing","title":"NVIDIA Tesla V100 | NVIDIA","url":"https://www.nvidia.com/ja-jp/data-center/tesla-v100/","snippet":"Volta 世代の GPU"},{"engine":"bing","title":"CUDA Toolkit Archive","url":"https://developer.nvidia.com/cuda-toolkit-archive"}]}),
                serde_json::json!({"type":"webSearch","id":"w3","query":"RTX 4090 価格","action":{"type":"search","query":"RTX 4090 価格"},"tool":"shopping_search","results":[]}),
                serde_json::json!({"type":"reasoning","id":"r2"}),
                serde_json::json!({"type":"dynamicToolCall","id":"e1","tool":"exec","status":"inProgress"}),
            ];
            crate::view::messages_with(&app, |m| {
                crate::view::push(m, "", "user", "V100 でも CUDA 12 が動くか調べて");
                for it in &rows {
                    if let (Some(id), Some(text)) = (it.get("id").and_then(|v| v.as_str()), client::tool_text(it)) {
                        let d = client::tool_detail(it);
                        crate::view::upsert_tool(m, id, &text, if d.is_empty() { None } else { Some(&d) });
                    }
                }
            });
            if mode == "busy" {
                app.global::<AppState>().set_busy(true);
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_DIFF=1: the edits of a turn as the chat shows them, open.
    if std::env::var("INOCODE_GUI_DIFF").is_ok() {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            let Some(app) = weak.upgrade() else { return };
            app.global::<AppState>().set_tools_open(true);
            crate::view::messages_with(&app, |m| {
                crate::view::push(m, "", "user", "src/main.rs を直して、NOTES.md を足して");
                crate::view::upsert_tool(m, "turn-diff", "2 件のファイルを編集しました", Some(DEMO_DIFF));
            });
        });
        extra.push(t);
    }
    // INOCODE_GUI_ENRICH=<file>: an inocode/models/enrich reply (the `response`) for the picker;
    // INOCODE_GUI_USE=<use> chooses the use the list is ordered by.
    if let Ok(file) = std::env::var("INOCODE_GUI_ENRICH") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 2), move || {
            let Some(app) = weak.upgrade() else { return };
            let Ok(text) = std::fs::read_to_string(&file) else { return };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
            let (map, credits, uses) = crate::models_enrich::parse(v.get("response").unwrap_or(&v));
            STATE.with(|s| {
                let mut st = s.borrow_mut();
                st.enrich = map;
                st.enrich_credits = credits;
                st.use_map = uses;
                st.use_filter = std::env::var("INOCODE_GUI_USE").ok().filter(|u| !u.is_empty());
                crate::view::refresh_pickers(&app, &st);
            });
        });
        extra.push(t);
    }
    // INOCODE_GUI_MODELS=<file>: a model/list reply (the `result`) that is shown as the list of
    // models; INOCODE_GUI_OPEN=picker opens the model popup.
    if let Ok(file) = std::env::var("INOCODE_GUI_MODELS") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        // After the real server's own list, so that this one stays.
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 2), move || {
            let Some(app) = weak.upgrade() else { return };
            let Ok(text) = std::fs::read_to_string(&file) else { return };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
            let list = crate::client::parse_models(v.get("response").unwrap_or(&v));
            STATE.with(|s| {
                let mut st = s.borrow_mut();
                st.settings.programs = list.clone();
                st.models = list;
                st.models_loaded = true;
            });
            crate::model_init::on_models(&app);
            STATE.with(|s| crate::view::refresh_pickers(&app, &s.borrow()));
        });
        extra.push(t);
    }
    if std::env::var("INOCODE_GUI_OPEN").as_deref() == Ok("picker") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 3), move || {
            if let Some(app) = weak.upgrade() {
                app.global::<PickerDemo>().set_open(true);
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_OPEN opens a screen of the settings first: settings:<section> | provider:<id> |
    // account:<provider>:<account id> | history:<provider>:<account id> | add
    if let Ok(what) = std::env::var("INOCODE_GUI_OPEN").map(|w| if w == "picker" { String::new() } else { w }).and_then(|w| if w.is_empty() { Err(std::env::VarError::NotPresent) } else { Ok(w) }) {
        let weak = app.as_weak();
        let first = slint::Timer::default();
        let w1 = what.clone();
        first.start(slint::TimerMode::SingleShot, Duration::from_secs(delay), move || {
            let Some(app) = weak.upgrade() else { return };
            match w1.split(':').next().unwrap_or("") {
                "settings" => {
                    let n = w1.split(':').nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                    app.global::<SettingsPageState>().invoke_open_section(n);
                }
                _ => crate::settings_page::open_usage(&app),
            }
        });
        let weak = app.as_weak();
        let second = slint::Timer::default();
        second.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 2), move || {
            let Some(app) = weak.upgrade() else { return };
            let parts: Vec<&str> = what.split(':').collect();
            match parts.as_slice() {
                ["provider", p] => crate::accounts_page::show_at(&app, p, ""),
                ["account", p, a] => crate::accounts_page::show_at(&app, p, a),
                ["history", p, a] => {
                    crate::accounts_page::show_at(&app, p, a);
                    crate::accounts_page::open_history_modal(&app, a);
                }
                ["add"] => app.global::<AccountsState>().invoke_begin("".into()),
                _ => {}
            }
        });
        extra.extend([first, second]);
    }
    // INOCODE_GUI_ACT=<step>;<step>...: what a hand would do on the open settings page, one step every
    // second from delay+3 (the picture is taken three seconds after the last): text:<row id>=<text> | action:<row id> | toggle:<row id>=on|off |
    // choose:<row id>=<index>. The rows are the ones of src/settings_page.rs (row-text, row-action,
    // row-toggle, row-choose): the same callbacks the page's own controls call.
    if let Ok(steps) = std::env::var("INOCODE_GUI_ACT") {
        for (i, step) in steps.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).enumerate() {
            let weak = app.as_weak();
            let t = slint::Timer::default();
            t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 3 + i as u64), move || {
                let Some(app) = weak.upgrade() else { return };
                let st = app.global::<SettingsPageState>();
                let (kind, rest) = step.split_once(':').unwrap_or((step.as_str(), ""));
                match kind {
                    "text" => {
                        if let Some((id, v)) = rest.split_once('=') {
                            st.invoke_row_text(id.into(), v.into());
                        }
                    }
                    "action" => st.invoke_row_action(rest.into()),
                    "toggle" => {
                        if let Some((id, v)) = rest.split_once('=') {
                            st.invoke_row_toggle(id.into(), v == "on");
                        }
                    }
                    "choose" => {
                        if let Some((id, v)) = rest.split_once('=') {
                            st.invoke_row_choose(id.into(), v.parse().unwrap_or(0));
                        }
                    }
                    _ => {}
                }
            });
            extra.push(t);
        }
    }
    // INOCODE_GUI_ELICIT=<file>: the params of an mcpServer/elicitation/request, shown the way the
    // server's request is (a form as questions, a tool approval as a card).
    if let Ok(file) = std::env::var("INOCODE_GUI_ELICIT") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            let Some(app) = weak.upgrade() else { return };
            let Ok(text) = std::fs::read_to_string(&file) else { return };
            let Ok(params) = serde_json::from_str::<serde_json::Value>(&text) else { return };
            let event = match crate::client::elicitation_questions(&params) {
                Some(items) => Event::Questions { key: "elicit-demo".into(), items },
                None => match crate::client::approval_info("mcpServer/elicitation/request", &params, None) {
                    Some(mut info) => {
                        info.key = "elicit-demo".into();
                        Event::Approval(info)
                    }
                    None => return,
                },
            };
            STATE.with(|s| crate::events::apply(&app, s, event));
        });
        extra.push(t);
    }
    // INOCODE_GUI_EVTAB=<n>: the part of the evidence tab that is open (0 sources, 1 claims, 2 map,
    // 3 verify) and INOCODE_GUI_TAB=2 for the tab itself.
    if let Some(n) = std::env::var("INOCODE_GUI_EVTAB").ok().and_then(|v| v.parse::<i32>().ok()) {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 2), move || {
            if let Some(app) = weak.upgrade() {
                app.global::<EvidenceState>().set_tab(n);
                app.global::<EvidenceState>().invoke_tab_changed(n);
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_COLLAB=<file>: a JSON array of collabAgentToolCall items, as the chat gets them
    // (spawnAgent, wait with agentsStates, sendInput): the team tab draws the sub-agents from them.
    if let Ok(file) = std::env::var("INOCODE_GUI_COLLAB") {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(delay + 1), move || {
            let Some(app) = weak.upgrade() else { return };
            let Ok(text) = std::fs::read_to_string(&file) else { return };
            // "MAIN" in the file is this thread
            let active = STATE.with(|st| st.borrow().active.clone());
            let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&text.replace("MAIN", &active)) else { return };
            for item in items {
                let event = Event::Notify { method: "gui/collabItem".into(), params: item };
                TAB_HOST.with(|h| {
                    if let Some(host) = h.borrow().as_ref() {
                        crate::tabs::event(&app, host, &event);
                    }
                });
            }
        });
        extra.push(t);
    }
    // INOCODE_GUI_PROMPT=<text>: a real turn. Once the app is connected (and INOCODE_GUI_MODEL=<id or
    // name>, when given, is chosen) the text goes the way a typed message goes (the composer's
    // submit: the same turn/start, with summary). INOCODE_GUI_SNAPSHOT_EVERY=<seconds> also writes
    // <file>-1.png, <file>-2.png ... on the way. The last picture is written INOCODE_GUI_SNAPSHOT_DELAY
    // seconds after the start.
    let live = std::env::var("INOCODE_GUI_PROMPT").ok().filter(|p| !p.is_empty());
    // INOCODE_GUI_TOOLS_OPEN=1: the rows of tools and thinking open (to read what they hold).
    if std::env::var("INOCODE_GUI_TOOLS_OPEN").is_ok() {
        app.global::<AppState>().set_tools_open(true);
    }
    if let Some(text) = live.clone() {
        // Several steps: INOCODE_GUI_PROMPT="/plan on ;; the request ;; /plan off ;; go on". Each is sent
        // the way a typed message is, the next one when no turn is running.
        let mut steps: std::collections::VecDeque<String> = text.split(" ;; ").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let mut since_send = 0u32;
        let weak = app.as_weak();
        let model = std::env::var("INOCODE_GUI_MODEL").ok().filter(|m| !m.is_empty());
        let started = std::time::Instant::now();
        let mut stage = 0u8;
        let mut waited = 0u32;
        let t = slint::Timer::default();
        t.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
            let Some(app) = weak.upgrade() else { return };
            if stage >= 2 {
                // The next step, once the turn of the last one is over (not at once: the turn
                // needs a moment to start).
                since_send += 1;
                if since_send >= 6 && !steps.is_empty() && !app.global::<AppState>().get_busy() {
                    if let Some(next) = steps.pop_front() {
                        app.invoke_submit(next.into());
                        since_send = 0;
                    }
                }
                return;
            }
            if started.elapsed() < Duration::from_secs(2) {
                return;
            }
            let ready = STATE.with(|s| {
                let st = s.borrow();
                !st.active.is_empty() && !visible_models(&st).is_empty()
            });
            if !ready {
                return;
            }
            if stage == 0 {
                match &model {
                    Some(m) => app.invoke_submit(format!("/model {m}").into()),
                    None => {}
                }
                stage = 1;
                waited = 0;
                return;
            }
            // Stage 1: wait for the model (and a new thread when the provider changed).
            waited += 1;
            let chosen = STATE.with(|s| {
                let st = s.borrow();
                let v = visible_models(&st);
                selected_model(&st, &v).is_some() && !st.active.is_empty()
            });
            if (chosen && waited >= 4) || waited > 40 {
                if chosen {
                    if let Some(first) = steps.pop_front() {
                        app.invoke_submit(first.into());
                    }
                }
                stage = 2;
                since_send = 0;
            }
        });
        extra.push(t);
    }
    let write = |app: &AppWindow, path: &std::path::Path| {
        if let Ok(buf) = app.window().take_snapshot() {
            if let Ok(file) = std::fs::File::create(path) {
                let mut enc = png::Encoder::new(std::io::BufWriter::new(file), buf.width(), buf.height());
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                if let Ok(mut w) = enc.write_header() {
                    let _ = w.write_image_data(buf.as_bytes());
                }
            }
        }
    };
    if live.is_some() {
        let every = env_num("INOCODE_GUI_SNAPSHOT_EVERY", 0);
        if every > 0 {
            let weak = app.as_weak();
            let base = std::path::PathBuf::from(&path);
            let mut n = 0u32;
            let t = slint::Timer::default();
            t.start(slint::TimerMode::Repeated, Duration::from_secs(every), move || {
                if let Some(app) = weak.upgrade() {
                    n += 1;
                    let stem = base.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    write(&app, &base.with_file_name(format!("{stem}-{n}.png")));
                }
            });
            extra.push(t);
        }
    }
    let weak = app.as_weak();
    let shot = slint::Timer::default();
    let act_steps = std::env::var("INOCODE_GUI_ACT").map(|s| s.split(';').filter(|x| !x.trim().is_empty()).count() as u64).unwrap_or(0);
    let shot_at = if live.is_some() { delay } else { (delay + 6).max(if act_steps > 0 { delay + 3 + act_steps + 3 } else { 0 }) };
    shot.start(slint::TimerMode::SingleShot, Duration::from_secs(shot_at), move || {
        if let Some(app) = weak.upgrade() {
            write(&app, std::path::Path::new(&path));
        }
        let _ = slint::quit_event_loop();
    });
    TIMERS.with(|t| t.borrow_mut().extend([open, shot]));
    TIMERS.with(|t| t.borrow_mut().extend(extra));
}

const DEMO_DIFF: &str = "diff --git a/src/main.rs b/src/main.rs
index 6ea728c..b1a5ac8 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,6 +1,7 @@
 fn main() {
-    println!(\"hello\");
+    let name = std::env::args().nth(1).unwrap_or_default();
+    println!(\"hello {name}\");
     run();
 }
 
 fn run() {}
@@ -20,4 +21,5 @@ fn helper() {
     let a = 1;
-    let b = 2;
+    let b = a + 1;
+    let c = b * 2;
     drop((a, b));
diff --git a/NOTES.md b/NOTES.md
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/NOTES.md
@@ -0,0 +1,3 @@
+# Notes
+
+The first line of a new file.
";
