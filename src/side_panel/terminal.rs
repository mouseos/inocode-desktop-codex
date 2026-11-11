//! Integrated terminal: `process/spawn` with a PTY on the app-server host, output through
//! `process/outputDelta`, input through `process/writeStdin`, size through `process/resizePty`.
//! The bytes go through the emulator in vt.rs; the view draws its lines.

use super::vt::{Run, Screen};
use super::{b64, Host};
use crate::client::Event;
use crate::{AppWindow, TermLine, TermRun, TermTab, TerminalState};
use serde_json::{json, Value};
use slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const CELL_W: f32 = 7.3;
const CELL_H: f32 = 17.0;

struct Term {
    handle: String,
    title: String,
    screen: Screen,
    exited: bool,
    /// How many lines the view is scrolled back (0 is the live screen).
    offset: usize,
    /// The selected rows of the view (from, to), in either order.
    sel: Option<(usize, usize)>,
}

struct Terminals {
    list: Vec<Term>,
    current: usize,
    next: u32,
    cols: usize,
    rows: usize,
}

thread_local! {
    static TERMS: RefCell<Terminals> = RefCell::new(Terminals { list: Vec::new(), current: 0, next: 1, cols: 80, rows: 24 });
}

pub fn install(app: &AppWindow, host: &Host) {
    let state = app.global::<TerminalState>();
    let (weak, h) = (app.as_weak(), host.clone());
    state.on_new_terminal(move || {
        if let Some(app) = weak.upgrade() {
            spawn(&app, &h);
        }
    });
    let weak = app.as_weak();
    state.on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            TERMS.with(|t| {
                let mut t = t.borrow_mut();
                if (i as usize) < t.list.len() {
                    t.current = i as usize;
                }
            });
            render(&app);
        }
    });
    let (weak, h) = (app.as_weak(), host.clone());
    state.on_close(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let handle = TERMS.with(|t| {
            let mut t = t.borrow_mut();
            let i = i as usize;
            if i >= t.list.len() {
                return None;
            }
            let term = t.list.remove(i);
            if t.current >= t.list.len() {
                t.current = t.list.len().saturating_sub(1);
            }
            Some(term)
        });
        if let Some(term) = handle {
            if !term.exited {
                h.rpc("term.kill", "process/kill", json!({ "processHandle": term.handle }));
            }
        }
        render(&app);
    });
    let (weak, h) = (app.as_weak(), host.clone());
    state.on_key(move |text, ctrl, alt, shift| {
        let Some(app) = weak.upgrade() else { return };
        // Ctrl+Shift+C copies, Ctrl+V and Ctrl+Shift+V paste (as in Windows Terminal).
        let lower = text.to_lowercase();
        if ctrl && shift && (lower == "c" || lower == "\u{3}") {
            copy_selection(&app);
            return;
        }
        if ctrl && (lower == "v" || lower == "\u{16}") {
            paste(&h);
            return;
        }
        // Typing returns to the live screen.
        let moved = TERMS.with(|t| {
            let mut t = t.borrow_mut();
            let cur = t.current;
            match t.list.get_mut(cur) {
                Some(term) if term.offset > 0 || term.sel.is_some() => {
                    term.offset = 0;
                    term.sel = None;
                    true
                }
                _ => false,
            }
        });
        if moved {
            render(&app);
        }
        if let Some(bytes) = key_bytes(text.as_str(), ctrl, alt, shift) {
            write(&h, &bytes);
        }
    });
    let weak = app.as_weak();
    state.on_scroll(move |delta| {
        let Some(app) = weak.upgrade() else { return };
        TERMS.with(|t| {
            let mut t = t.borrow_mut();
            let cur = t.current;
            if let Some(term) = t.list.get_mut(cur) {
                let max = term.screen.scrollback_len() as i32;
                term.offset = (term.offset as i32 + delta).clamp(0, max) as usize;
                term.sel = None;
            }
        });
        render(&app);
    });
    let weak = app.as_weak();
    state.on_sel_start(move |row| {
        let Some(app) = weak.upgrade() else { return };
        TERMS.with(|t| {
            let mut t = t.borrow_mut();
            let cur = t.current;
            if let Some(term) = t.list.get_mut(cur) {
                term.sel = Some((row.max(0) as usize, row.max(0) as usize));
            }
        });
        render(&app);
    });
    let weak = app.as_weak();
    state.on_sel_update(move |row| {
        let Some(app) = weak.upgrade() else { return };
        TERMS.with(|t| {
            let mut t = t.borrow_mut();
            let cur = t.current;
            if let Some(term) = t.list.get_mut(cur) {
                if let Some((from, _)) = term.sel {
                    term.sel = Some((from, row.max(0) as usize));
                }
            }
        });
        render(&app);
    });
    let weak = app.as_weak();
    state.on_copy(move || {
        if let Some(app) = weak.upgrade() {
            copy_selection(&app);
        }
    });
    let h = host.clone();
    state.on_paste(move || paste(&h));
    let (weak, h) = (app.as_weak(), host.clone());
    state.on_viewport(move |w, hgt| {
        let Some(app) = weak.upgrade() else { return };
        let cols = (((w - 16.0) / CELL_W) as usize).max(10);
        let rows = (((hgt - 12.0) / CELL_H) as usize).max(3);
        let changed = TERMS.with(|t| {
            let mut t = t.borrow_mut();
            if t.cols == cols && t.rows == rows {
                return Vec::new();
            }
            t.cols = cols;
            t.rows = rows;
            let mut live = Vec::new();
            for term in &mut t.list {
                term.screen.resize(cols, rows);
                if !term.exited {
                    live.push(term.handle.clone());
                }
            }
            live
        });
        for handle in changed {
            h.rpc(
                "term.resize",
                "process/resizePty",
                json!({ "processHandle": handle, "size": { "rows": rows, "cols": cols } }),
            );
        }
        render(&app);
    });
}

/// The terminal tab was shown: start the first terminal.
pub fn opened(app: &AppWindow, host: &Host) {
    if TERMS.with(|t| t.borrow().list.is_empty()) {
        spawn(app, host);
    }
}

fn shell() -> Vec<String> {
    if let Ok(s) = std::env::var("INOCODE_TERM_SHELL") {
        let parts: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    if cfg!(windows) {
        vec!["powershell.exe".into(), "-NoLogo".into()]
    } else {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "sh".into())]
    }
}

fn spawn(app: &AppWindow, host: &Host) {
    let cwd = crate::state::STATE.with(|s| s.borrow().active_cwd.clone());
    let cwd = if cwd.is_empty() {
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
    } else {
        cwd
    };
    let (handle, cols, rows) = TERMS.with(|t| {
        let mut t = t.borrow_mut();
        let n = t.next;
        t.next += 1;
        let (cols, rows) = (t.cols, t.rows);
        t.list.push(Term {
            handle: format!("inocode-term-{n}"),
            title: format!("ターミナル {n}"),
            screen: Screen::new(cols, rows),
            exited: false,
            offset: 0,
            sel: None,
        });
        t.current = t.list.len() - 1;
        (t.list[t.current].handle.clone(), cols, rows)
    });
    host.rpc(
        &format!("term.spawn:{handle}"),
        "process/spawn",
        json!({
            "command": shell(),
            "processHandle": handle,
            "cwd": cwd,
            "tty": true,
            "streamStdin": true,
            "streamStdoutStderr": true,
            "outputBytesCap": null,
            "timeoutMs": null,
            "env": { "TERM": "xterm-256color", "COLORTERM": "truecolor" },
            "size": { "rows": rows, "cols": cols },
        }),
    );
    render(app);
}

fn write(host: &Host, bytes: &[u8]) {
    let handle = TERMS.with(|t| {
        let t = t.borrow();
        t.list.get(t.current).filter(|x| !x.exited).map(|x| x.handle.clone())
    });
    if let Some(handle) = handle {
        host.rpc("term.write", "process/writeStdin", json!({ "processHandle": handle, "deltaBase64": b64::encode(bytes) }));
    }
}

/// The selected rows (or the whole view when nothing is selected) go to the clipboard.
fn copy_selection(app: &AppWindow) {
    let text = TERMS.with(|t| {
        let t = t.borrow();
        let term = t.list.get(t.current)?;
        let lines = term.screen.text_at(term.offset);
        let (from, to) = match term.sel {
            Some((a, b)) => (a.min(b), a.max(b)),
            None => (0, lines.len().saturating_sub(1)),
        };
        let picked: Vec<&str> = lines.iter().skip(from).take(to + 1 - from).map(String::as_str).collect();
        Some(picked.join("\n").trim_end().to_string())
    });
    let Some(text) = text else { return };
    let _ = app;
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
    }
}

/// The clipboard text is typed into the terminal (line breaks as Enter).
fn paste(host: &Host) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
    let Ok(text) = clipboard.get_text() else { return };
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if !text.is_empty() {
        write(host, text.as_bytes());
    }
}

pub fn send_text(host: &Host, text: &str) {
    write(host, text.as_bytes());
}

/// A key as the terminal view reports it (Slint key text and modifiers), for the snapshot demo.
pub fn send_key(host: &Host, text: &str, ctrl: bool) {
    if let Some(bytes) = key_bytes(text, ctrl, false, false) {
        write(host, &bytes);
    }
}

/// Slint key text -> the bytes a terminal sends.
fn key_bytes(text: &str, ctrl: bool, alt: bool, _shift: bool) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let seq: &str = match first {
        '\n' => "\r",
        '\u{8}' => "\x7f",
        '\t' => "\t",
        '\u{1b}' => "\x1b",
        '\u{7f}' => "\x1b[3~",
        '\u{f700}' => "\x1b[A",
        '\u{f701}' => "\x1b[B",
        '\u{f702}' => "\x1b[D",
        '\u{f703}' => "\x1b[C",
        '\u{f729}' => "\x1b[H",
        '\u{f72b}' => "\x1b[F",
        '\u{f72c}' => "\x1b[5~",
        '\u{f72d}' => "\x1b[6~",
        '\u{f727}' => "\x1b[2~",
        c if ('\u{f700}'..='\u{f8ff}').contains(&c) => return None,
        _ => "",
    };
    if !seq.is_empty() {
        return Some(seq.as_bytes().to_vec());
    }
    if chars.next().is_none() {
        if ctrl && first.is_ascii_alphabetic() {
            return Some(vec![(first.to_ascii_lowercase() as u8) & 0x1f]);
        }
        if ctrl && (first as u32) < 0x20 {
            return Some(vec![first as u8]);
        }
    }
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    match event {
        Event::Rpc { tag, result: Err(e) } if tag.starts_with("term.spawn:") => {
            let handle = &tag["term.spawn:".len()..];
            TERMS.with(|t| {
                if let Some(term) = t.borrow_mut().list.iter_mut().find(|x| x.handle == handle) {
                    term.screen.feed(format!("\x1b[31m{}\x1b[0m\r\n", e.replace('\n', " ")).as_bytes());
                    term.exited = true;
                }
            });
            render(app);
        }
        Event::Notify { method, params } if method == "process/outputDelta" => {
            let handle = params.get("processHandle").and_then(Value::as_str).unwrap_or("");
            let data = b64::decode(params.get("deltaBase64").and_then(Value::as_str).unwrap_or(""));
            let mut reply = Vec::new();
            let known = TERMS.with(|t| {
                let mut t = t.borrow_mut();
                match t.list.iter_mut().find(|x| x.handle == handle) {
                    Some(term) => {
                        term.screen.feed(&data);
                        reply = std::mem::take(&mut term.screen.replies);
                        true
                    }
                    None => false,
                }
            });
            if known {
                if !reply.is_empty() {
                    host.rpc("term.write", "process/writeStdin", json!({ "processHandle": handle, "deltaBase64": b64::encode(&reply) }));
                }
                render(app);
            }
        }
        Event::Notify { method, params } if method == "process/exited" => {
            let handle = params.get("processHandle").and_then(Value::as_str).unwrap_or("");
            let code = params.get("exitCode").and_then(Value::as_i64).unwrap_or(0);
            TERMS.with(|t| {
                if let Some(term) = t.borrow_mut().list.iter_mut().find(|x| x.handle == handle) {
                    term.exited = true;
                    term.screen.feed(format!("\r\n\x1b[90m[プロセスは終了しました (コード {code})]\x1b[0m\r\n").as_bytes());
                }
            });
            render(app);
        }
        _ => {}
    }
}

fn color(rgb: u32) -> Color {
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

fn run(r: &Run) -> TermRun {
    TermRun {
        text: SharedString::from(r.text.as_str()),
        fg: color(r.style.fg.unwrap_or(0)),
        bg: color(r.style.bg.unwrap_or(0)),
        has_fg: r.style.fg.is_some(),
        has_bg: r.style.bg.is_some(),
        inverse: r.style.inverse,
        bold: r.style.bold,
    }
}

fn render(app: &AppWindow) {
    let state = app.global::<TerminalState>();
    TERMS.with(|t| {
        let t = t.borrow();
        let tabs: Vec<TermTab> = t
            .list
            .iter()
            .enumerate()
            .map(|(i, x)| TermTab { title: x.title.as_str().into(), selected: i == t.current, exited: x.exited })
            .collect();
        state.set_tabs(ModelRc::from(Rc::new(VecModel::from(tabs))));
        let lines: Vec<TermLine> = match t.list.get(t.current) {
            Some(term) => {
                let (from, to) = term.sel.map(|(a, b)| (a.min(b), a.max(b))).unwrap_or((1, 0));
                term.screen
                    .lines_at(term.offset)
                    .iter()
                    .enumerate()
                    .map(|(i, l)| TermLine {
                        runs: ModelRc::from(Rc::new(VecModel::from(l.iter().map(run).collect::<Vec<_>>()))),
                        selected: i >= from && i <= to,
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        state.set_lines(ModelRc::from(Rc::new(VecModel::from(lines))));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(key_bytes("a", false, false, false).unwrap(), b"a");
        assert_eq!(key_bytes("c", true, false, false).unwrap(), vec![3]);
        assert_eq!(key_bytes("\n", false, false, false).unwrap(), b"\r");
        assert_eq!(key_bytes("\u{f700}", false, false, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("\u{3}", true, false, false).unwrap(), vec![3]);
        assert!(key_bytes("\u{f710}", false, false, false).is_none());
    }
}
