//! Text that streams in from the server, brought to the screen at a fixed interval.
//!
//! An answer arrives as many small pieces. Putting each piece on the screen lays out the whole
//! conversation again (the chat is one list whose height every message adds to), so a long answer
//! made the window slower and slower. The pieces are collected here and written at most every
//! `STEP`; the reader sees the answer grow at about 12 times a second, which reads as live.
//!
//! The timer stops when nothing is pending: a quiet window does no work.

use crate::view::{messages_with, push};
use crate::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How often streamed text is written to the screen.
const STEP: Duration = Duration::from_millis(80);

thread_local! {
    /// The text of the answer being written, not yet on the screen.
    static ANSWER: RefCell<String> = const { RefCell::new(String::new()) };
    /// The body of each tool row that is still streaming in.
    static TOOLS: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
    static TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static LAST: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

fn pending() -> bool {
    ANSWER.with(|a| !a.borrow().is_empty()) || TOOLS.with(|t| !t.borrow().is_empty())
}

/// A piece of the answer. It is put on the screen by the next tick.
pub(crate) fn delta(text: &str) {
    ANSWER.with(|a| a.borrow_mut().push_str(text));
}

/// A piece of a tool row's body.
pub(crate) fn tool_delta(id: &str, text: &str) {
    TOOLS.with(|t| t.borrow_mut().entry(id.to_string()).or_default().push_str(text));
}

/// Start the tick if it is not running.
pub(crate) fn start(app: &AppWindow) {
    TIMER.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, STEP, move || {
            let Some(app) = weak.upgrade() else { return };
            flush(&app);
        });
        *slot.borrow_mut() = Some(timer);
    });
}

/// Write what is pending now (the end of an answer, a tool row, the end of the turn).
pub(crate) fn flush(app: &AppWindow) {
    if !pending() {
        stop();
        return;
    }
    LAST.with(|l| l.set(Some(Instant::now())));
    let tools: Vec<(String, String)> = TOOLS.with(|t| std::mem::take(&mut *t.borrow_mut()).into_iter().collect());
    let answer = ANSWER.with(|a| a.borrow_mut().drain(..).collect::<String>());
    if !answer.is_empty() {
        messages_with(app, |model| {
            let n = model.row_count();
            match model.row_data(n.wrapping_sub(1)) {
                Some(mut last) if last.role == "assistant" => {
                    last.text.push_str(&answer);
                    model.set_row_data(n - 1, last);
                }
                _ => push(model, "", "assistant", &answer),
            }
        });
        crate::chat_md::sync_last(app, true);
    }
    let had_tools = !tools.is_empty();
    for (id, text) in tools {
        apply_tool_delta(app, &id, &text);
    }
    if answer.is_empty() && !had_tools {
        return;
    }
    // The reader at the end follows it; one who scrolled up stays where they are.
    app.invoke_scroll_bottom(false);
    if !pending() {
        stop();
    }
}

fn apply_tool_delta(app: &AppWindow, id: &str, delta: &str) {
    messages_with(app, |model| {
        // The streaming row is one of the last ones: look from the end.
        for i in (0..model.row_count()).rev() {
            let Some(mut row) = model.row_data(i) else { continue };
            if row.role != "tool" || row.id != id {
                continue;
            }
            // The body of a diff is the diff; the output of the patch is not added to it.
            if row.diff.row_count() > 0 {
                return;
            }
            let joined = format!("{}{}", row.detail, delta);
            row.detail = crate::view::clip_detail(&joined).into();
            model.set_row_data(i, row);
            return;
        }
    });
}

fn stop() {
    TIMER.with(|slot| {
        if let Some(timer) = slot.borrow().as_ref() {
            timer.stop();
        }
        *slot.borrow_mut() = None;
    });
}

/// Nothing is pending any more (the conversation was replaced).
pub(crate) fn reset() {
    ANSWER.with(|a| a.borrow_mut().clear());
    TOOLS.with(|t| t.borrow_mut().clear());
    stop();
    LAST.with(|l| l.set(None));
}
