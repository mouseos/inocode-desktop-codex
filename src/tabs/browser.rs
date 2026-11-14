//! ブラウザタブ(65 第 2 段)。タブの中に inocode-browser(CEF)を子ウィンドウとして埋め込む。
//! タブを開くと `inocode-browser --parent-window=<GUI の窓> --stdin-control --parent-pid=<GUI>` を起動し、
//! 子ウィンドウの位置と大きさを、タブの表示領域(BrowserState.view-*)に合わせ続ける。別のタブでは隠す。
//! 閉じる: 標準入力に `close`。GUI の終了でも終わる。URL 欄は、表示中のページを移動する(`--url=..`)。
//! 状態: 起動していない → 起動中(スピナー)→ 表示中(標準出力の ready の行)。失敗は標準エラーの理由。

use super::Host;
use crate::client::Event;
use crate::{AppWindow, BrowserState, Tabs};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const IDLE: i32 = 0;
const STARTING: i32 = 1;
const SHOWN: i32 = 2;
const FAILED: i32 = 3;
/// ui/tabs.slint の Tabs.titles での位置。
const TAB: i32 = 4;

/// The browser's control pipe (`close`), held while it runs.
static CONTROL: Mutex<Option<ChildStdin>> = Mutex::new(None);
/// The process that owns the child window (the `pid` of the ready line).
static BROWSER_PID: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static PLACER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// The child window and where it was put last (x, y, w, h, shown).
    static PLACED: RefCell<(isize, (i32, i32, i32, i32, bool))> = const { RefCell::new((0, (0, 0, 0, 0, false))) };
    /// Until then the place is sent again on every tick: the page inside the child window is made
    /// a moment after the window, and takes the size the window has at that moment.
    static SETTLE_UNTIL: RefCell<Option<std::time::Instant>> = const { RefCell::new(None) };
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

/// The GUI's own window (Windows: its HWND), the parent of the browser.
fn gui_window(app: &AppWindow) -> Option<isize> {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    app.window().with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    })?
}

/// Start the browser in the tab, or open the URL in the one that is up.
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
    let Some(parent) = gui_window(app) else {
        st.set_error("この OS では、まだタブに埋め込めません".into());
        st.set_phase(FAILED);
        return;
    };
    #[cfg(windows)]
    clip_children(parent);
    st.set_error("".into());
    st.set_phase(STARTING);
    std::thread::spawn(move || launch(exe, url, parent, weak));
}

/// A browser in its own window (started by the AI's tools before the tab was opened) is closed
/// first: there is one browser, and it lives in the tab.
fn close_other(exe: &Path) {
    let run = |arg: &str| {
        let mut cmd = Command::new(exe);
        cmd.arg(arg).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut cmd);
        cmd.status().is_ok_and(|s| s.success())
    };
    if !run("status") {
        return;
    }
    run("close");
    for _ in 0..50 {
        if !run("status") {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn launch(exe: PathBuf, url: String, parent: isize, weak: slint::Weak<AppWindow>) {
    close_other(&exe);
    let mut cmd = Command::new(&exe);
    cmd.arg(format!("--url={url}"))
        .arg(format!("--parent-pid={}", std::process::id()))
        .arg(format!("--parent-window={parent}"))
        .arg("--stdin-control")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            set(&weak, FAILED, format!("起動できません: {e}"));
            return;
        }
    };
    *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) = child.stdin.take();
    let last_err = std::sync::Arc::new(Mutex::new(String::new()));
    let reader = child.stderr.take().map(|err| {
        let last = last_err.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    *last.lock().unwrap_or_else(|e| e.into_inner()) = line;
                }
            }
        })
    });
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if v.get("event").and_then(|e| e.as_str()) == Some("ready") {
                let pid = v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
                BROWSER_PID.store(pid, Ordering::SeqCst);
                set(&weak, SHOWN, String::new());
            }
        }
    }
    let status = child.wait();
    if let Some(r) = reader {
        let _ = r.join();
    }
    *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    BROWSER_PID.store(0, Ordering::SeqCst);
    let ok = status.as_ref().is_ok_and(|s| s.success());
    let reason = last_err.lock().map(|s| s.clone()).unwrap_or_default();
    let _ = slint::invoke_from_event_loop(move || {
        PLACED.with(|p| *p.borrow_mut() = (0, (0, 0, 0, 0, false)));
        if let Some(app) = weak.upgrade() {
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
}

/// `close` on the control pipe: the browser closes and every CEF process ends.
fn close() {
    if let Some(stdin) = CONTROL.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = stdin.write_all(b"close\n");
        let _ = stdin.flush();
    }
}

/// The GUI window paints around the browser, not over it.
#[cfg(windows)]
fn clip_children(parent: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_STYLE, WS_CLIPCHILDREN};
    let hwnd = parent as windows_sys::Win32::Foundation::HWND;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_CLIPCHILDREN as isize);
    }
}

/// The browser's child window under the GUI window: the direct child that its process owns.
#[cfg(windows)]
fn find_child(parent: isize, pid: u32) -> isize {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetParent, GetWindowThreadProcessId};
    struct Seek {
        parent: HWND,
        pid: u32,
        found: HWND,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> i32 {
        let seek = unsafe { &mut *(lparam as *mut Seek) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner == seek.pid && unsafe { GetParent(hwnd) } == seek.parent {
            seek.found = hwnd;
            return 0;
        }
        1
    }
    let mut seek = Seek { parent: parent as HWND, pid, found: std::ptr::null_mut() };
    unsafe { EnumChildWindows(parent as HWND, Some(each), &mut seek as *mut Seek as LPARAM) };
    seek.found as isize
}

/// Keep the child window on the tab's area (physical pixels), and hidden on the other tabs.
#[cfg(windows)]
fn place(app: &AppWindow) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, ShowWindow, HWND_TOP, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    };
    let pid = BROWSER_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let Some(parent) = gui_window(app) else { return };
    let mut hwnd = PLACED.with(|p| p.borrow().0);
    if hwnd == 0 {
        hwnd = find_child(parent, pid);
        if hwnd == 0 {
            return;
        }
        PLACED.with(|p| p.borrow_mut().0 = hwnd);
        SETTLE_UNTIL.with(|t| *t.borrow_mut() = Some(std::time::Instant::now() + Duration::from_secs(4)));
    }
    let st = app.global::<BrowserState>();
    let shown = app.global::<Tabs>().get_current() == TAB && st.get_phase() == SHOWN && !st.get_covered();
    let k = app.window().scale_factor();
    let rect = (
        (st.get_view_x() * k).round() as i32,
        (st.get_view_y() * k).round() as i32,
        (st.get_view_w() * k).round().max(1.0) as i32,
        (st.get_view_h() * k).round().max(1.0) as i32,
        shown,
    );
    let settling = SETTLE_UNTIL.with(|t| t.borrow().is_some_and(|u| std::time::Instant::now() < u));
    if PLACED.with(|p| p.borrow().1) == rect && !settling {
        return;
    }
    PLACED.with(|p| p.borrow_mut().1 = rect);
    let h = hwnd as windows_sys::Win32::Foundation::HWND;
    unsafe {
        if shown {
            SetWindowPos(h, HWND_TOP, rect.0, rect.1, rect.2, rect.3, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            ShowWindow(h, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn place(_app: &AppWindow) {}

pub fn install(app: &AppWindow, _host: &Host) {
    let st = app.global::<BrowserState>();
    let weak = app.as_weak();
    st.on_open(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
    st.on_close(close);
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(33), move || {
        if let Some(app) = weak.upgrade() {
            place(&app);
        }
    });
    PLACER.with(|p| *p.borrow_mut() = Some(timer));
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
