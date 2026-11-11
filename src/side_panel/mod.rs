//! Right-hand work area: tabs for the session overview, a terminal, the files and the changes.
//! UI: ui/side_panel.slint and ui/side/*.slint. Called from src/tabs/mod.rs (install, event).

mod b64;
mod files;
mod terminal;
mod vt;

use crate::client::Event;
use crate::tabs::Host;
use crate::{AppWindow, SidePanelState};
use slint::ComponentHandle;

pub fn install(app: &AppWindow, host: &Host) {
    terminal::install(app, host);
    files::install(app, host);
    demo(app, host);
    let weak = app.as_weak();
    let host = host.clone();
    app.global::<SidePanelState>().on_selected(move |tab| {
        let Some(app) = weak.upgrade() else { return };
        match tab {
            1 => terminal::opened(&app, &host),
            2 => files::opened(&app, &host),
            _ => {}
        }
    });
}

thread_local! {
    static TIMERS: std::cell::RefCell<Vec<slint::Timer>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Without the mouse or the keyboard (for the snapshot): INOCODE_GUI_SIDE=<tab> shows that tab of
/// the right-hand area after 3 seconds; INOCODE_GUI_TERM_INPUT=<text> types that text into the
/// terminal after 7 seconds (\n is Enter).
fn demo(app: &AppWindow, host: &Host) {
    use std::time::Duration;
    let Some(tab) = std::env::var("INOCODE_GUI_SIDE").ok().and_then(|v| v.parse::<i32>().ok()) else { return };
    let weak = app.as_weak();
    let show = slint::Timer::default();
    show.start(slint::TimerMode::SingleShot, Duration::from_secs(3), move || {
        if let Some(app) = weak.upgrade() {
            app.set_panel_open(true);
            app.global::<SidePanelState>().invoke_select(tab);
        }
    });
    let mut timers = vec![show];
    if let Ok(text) = std::env::var("INOCODE_GUI_TERM_INPUT") {
        let host = host.clone();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(7), move || {
            terminal::send_text(&host, &text.replace("\\n", "\r"));
        });
        timers.push(t);
    }
    // INOCODE_GUI_TERM_KEYS="type:echo a;enter;backspace;up;ctrl+c": keys through the same path as
    // the terminal view's key handler, one every half second from the 7th second.
    if let Ok(keys) = std::env::var("INOCODE_GUI_TERM_KEYS") {
        for (i, token) in keys.split(';').enumerate() {
            let (host, token) = (host.clone(), token.to_string());
            let t = slint::Timer::default();
            t.start(slint::TimerMode::SingleShot, Duration::from_millis(7000 + 500 * i as u64), move || {
                if let Some(text) = token.strip_prefix("type:") {
                    for ch in text.chars() {
                        terminal::send_key(&host, &ch.to_string(), false);
                    }
                    return;
                }
                let (text, ctrl) = match token.as_str() {
                    "enter" => ("\n", false),
                    "backspace" => ("\u{8}", false),
                    "tab" => ("\t", false),
                    "up" => ("\u{f700}", false),
                    "down" => ("\u{f701}", false),
                    "left" => ("\u{f702}", false),
                    "right" => ("\u{f703}", false),
                    "ctrl+c" => ("c", true),
                    _ => return,
                };
                terminal::send_key(&host, text, ctrl);
            });
            timers.push(t);
        }
    }
    TIMERS.with(|t| t.borrow_mut().extend(timers));
}

pub fn event(app: &AppWindow, host: &Host, event: &Event) {
    terminal::event(app, host, event);
    files::event(app, host, event);
}
