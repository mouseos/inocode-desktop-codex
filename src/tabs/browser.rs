//! ブラウザタブ(65 第 1 段)。タブを開くと inocode-browser(CEF の別窓)を起動し、閉じる・GUI の終了で
//! 終わる(`--parent-pid`)。URL 欄は、起動中のブラウザでそのページを開く(`inocode-browser --url=..`)。
//! 状態: 起動していない → 起動中(スピナー)→ 表示中(標準出力の ready の行)。失敗は標準エラーの理由。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, BrowserState};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const IDLE: i32 = 0;
const STARTING: i32 = 1;
const SHOWN: i32 = 2;
const FAILED: i32 = 3;

thread_local! {
    /// The launcher we started (`inocode-browser`); it lives as long as the window.
    static RUNNING: RefCell<Option<u32>> = const { RefCell::new(None) };
}

/// INOCODE_BROWSER, next to this exe, on PATH, or the build output of the repository.
fn find_exe() -> Option<PathBuf> {
    let name = if cfg!(windows) { "inocode-browser.exe" } else { "inocode-browser" };
    if let Some(p) = std::env::var_os("INOCODE_BROWSER").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    if let Some(p) = exe_dir.as_ref().map(|d| d.join(name)).filter(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        if let Some(p) = std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()) {
            return Some(p);
        }
    }
    let dev = Path::new("cli").join("inocode").join(".build").join("browser-target").join("release").join(name);
    let mut bases: Vec<PathBuf> = exe_dir.iter().flat_map(|d| d.ancestors().map(Path::to_path_buf)).collect();
    if let Ok(cwd) = std::env::current_dir() {
        bases.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    bases.into_iter().map(|b| b.join(&dev)).find(|p| p.is_file())
}

fn url_of(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        "about:blank".into()
    } else if t.contains("://") || t.starts_with("about:") || t.starts_with("data:") {
        t.into()
    } else {
        format!("https://{t}")
    }
}

fn set(weak: &slint::Weak<AppWindow>, phase: i32, error: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            let st = app.global::<BrowserState>();
            st.set_error(error.into());
            st.set_phase(phase);
        }
    });
}

fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Start the browser window, or open the URL in the one that is up.
fn open(app: &AppWindow) {
    let st = app.global::<BrowserState>();
    if st.get_phase() == STARTING {
        return;
    }
    let Some(exe) = find_exe() else {
        st.set_error("inocode-browser が見つかりません。INOCODE_BROWSER を指定してください".into());
        st.set_phase(FAILED);
        return;
    };
    let url = url_of(&st.get_url());
    let weak = app.as_weak();
    if st.get_phase() == SHOWN {
        // The running one: navigate it, and wait in a thread so the UI does not.
        let mut cmd = Command::new(&exe);
        cmd.arg(format!("--url={url}")).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        no_window(&mut cmd);
        std::thread::spawn(move || {
            if let Ok(out) = cmd.output() {
                if !out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    set(&weak, SHOWN, msg);
                }
            }
        });
        return;
    }
    let mut cmd = Command::new(&exe);
    cmd.arg(format!("--url={url}"))
        .arg(format!("--parent-pid={}", std::process::id()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            st.set_error(format!("起動できません: {e}").into());
            st.set_phase(FAILED);
            return;
        }
    };
    st.set_error("".into());
    st.set_phase(STARTING);
    RUNNING.with(|r| *r.borrow_mut() = Some(child.id()));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let last_err = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let reader = stderr.map(|err| {
        let last = last_err.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    *last.lock().unwrap_or_else(|e| e.into_inner()) = line;
                }
            }
        })
    });
    std::thread::spawn(move || {
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if line.contains("\"event\":\"ready\"") {
                    set(&weak, SHOWN, String::new());
                }
            }
        }
        let status = child.wait();
        if let Some(r) = reader {
            let _ = r.join();
        }
        let ok = status.as_ref().is_ok_and(|s| s.success());
        let reason = last_err.lock().map(|s| s.clone()).unwrap_or_default();
        let weak2 = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            RUNNING.with(|r| *r.borrow_mut() = None);
            if let Some(app) = weak2.upgrade() {
                let st = app.global::<BrowserState>();
                if ok {
                    st.set_error("".into());
                    st.set_phase(IDLE);
                } else {
                    let why = if reason.is_empty() { "ブラウザが終了しました".to_string() } else { reason };
                    st.set_error(why.into());
                    st.set_phase(FAILED);
                }
            }
        });
    });
}

/// Close the window the way its close button does (`inocode-browser close`: WM_CLOSE), so every
/// CEF process ends and the launcher returns.
fn close() {
    if RUNNING.with(|r| r.borrow().is_none()) {
        return;
    }
    let Some(exe) = find_exe() else { return };
    let mut cmd = Command::new(exe);
    cmd.arg("close").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    std::thread::spawn(move || {
        let _ = cmd.status();
    });
}

pub fn install(app: &AppWindow, _host: &Host) {
    let st = app.global::<BrowserState>();
    let weak = app.as_weak();
    st.on_open(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
    st.on_close(close);
}

/// The tab was opened: start the browser if it is not up.
pub fn opened(app: &AppWindow, _host: &Host) {
    let phase = app.global::<BrowserState>().get_phase();
    if phase == IDLE {
        open(app);
    }
}

pub fn event(_app: &AppWindow, _host: &Host, _event: &Event) {}

#[cfg(test)]
mod tests {
    use super::url_of;

    #[test]
    fn a_bare_host_gets_https_and_an_empty_field_is_a_blank_page() {
        assert_eq!(url_of("example.com"), "https://example.com");
        assert_eq!(url_of(" http://a/b "), "http://a/b");
        assert_eq!(url_of(""), "about:blank");
        assert_eq!(url_of("about:blank"), "about:blank");
    }
}
