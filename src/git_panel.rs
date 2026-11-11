//! Git of the working folder (the panel of ui/git_panel.slint): the changed files and their diff,
//! stage and unstage, commit, branches, the comparison with a branch or a commit, reverting a file
//! or a hunk (asked first), line comments, push and a pull request (git and gh, with the
//! credentials the user has set up), worktrees, and the review of the changes.
//! The git commands run in the working folder of the active thread.

use crate::state::STATE;
use crate::*;
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command as Proc, Stdio};

/// The file on the screen: its diff text and what is needed to revert it.
struct Cur {
    path: String,
    raw: String,
    untracked: bool,
    x: char,
}

/// What the confirmation question is about.
#[derive(Clone)]
enum Pending {
    RevertFile,
    RevertHunk(usize),
    RemoveWorktree(String),
}

struct Comment {
    path: String,
    line: usize,
    text: String,
    comment: String,
}

#[derive(Default)]
struct Data {
    cur: Option<Cur>,
    /// What to compare with: empty is the uncommitted changes (HEAD).
    base: String,
    /// The rows of the comparison list: (label, ref); the first is the uncommitted changes.
    bases: Vec<(String, String)>,
    pending: Option<Pending>,
    comments: Vec<Comment>,
    selected_line: Option<usize>,
    /// (path, branch) of the worktrees on the list.
    worktrees: Vec<(String, String)>,
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<client::Handle>>> = const { RefCell::new(None) };
    static ENTRIES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    static DATA: RefCell<Data> = RefCell::new(Data::default());
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
}

/// A diff or a file is read up to this size.
const MAX_BYTES: usize = 400_000;
/// And shown up to this many lines.
const MAX_LINES: usize = 3000;

// ------------------------------------------------------------------ pure helpers (tested)

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub path: String,
    /// Index (staged) status and working tree status, as in `git status --porcelain`.
    pub x: char,
    pub y: char,
}

/// `git status --porcelain=v1 -z`: entries "XY path\0"; a rename or copy has the old path after.
pub(crate) fn parse_status(out: &str) -> Vec<Entry> {
    let mut parts = out.split('\0').filter(|p| !p.is_empty());
    let mut entries = Vec::new();
    while let Some(p) = parts.next() {
        let mut chars = p.chars();
        let (Some(x), Some(y), Some(' ')) = (chars.next(), chars.next(), chars.next()) else { continue };
        let path = chars.as_str().to_string();
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            let _old = parts.next();
        }
        entries.push(Entry { path, x, y });
    }
    entries
}

/// `git diff --name-status` (the comparison with a branch or a commit): "M<TAB>path",
/// "R100<TAB>old<TAB>new". The change is shown as a working tree change (nothing is staged).
pub(crate) fn parse_name_status(out: &str) -> Vec<Entry> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let code = f.next()?.chars().next()?;
            let path = f.last()?.to_string();
            Some(Entry { path, x: ' ', y: if matches!(code, 'A' | 'D' | 'R' | 'C') { code } else { 'M' } })
        })
        .collect()
}

/// `git diff --numstat`: "added<TAB>removed<TAB>path" (a binary file has "-").
pub(crate) fn parse_numstat(out: &str) -> HashMap<String, (i64, i64)> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (a, d, path) = (f.next()?, f.next()?, f.next()?);
            Some((path.to_string(), (a.parse().unwrap_or(0), d.parse().unwrap_or(0))))
        })
        .collect()
}

/// The letter of a file in the list.
pub(crate) fn status_letter(e: &Entry) -> &'static str {
    if e.x == '?' {
        return "?";
    }
    let c = if e.y != ' ' { e.y } else { e.x };
    match c {
        'A' => "A",
        'D' => "D",
        'R' | 'C' => "R",
        _ => "M",
    }
}

/// The file has changes that are not staged (or is new): the button stages them.
pub(crate) fn can_stage(e: &Entry) -> bool {
    e.y != ' ' || e.x == '?'
}

pub(crate) fn is_staged(e: &Entry) -> bool {
    e.x != ' ' && e.x != '?'
}

/// A line of a diff: its text, kind (0 plain, 1 added, 2 removed, 4 header or hunk), the index of
/// its hunk (-1 before the first) and whether it is the "@@" line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub text: String,
    pub kind: i32,
    pub hunk: i32,
    pub head: bool,
}

/// A diff as lines with their kind.
pub(crate) fn diff_rows(text: &str) -> Vec<Row> {
    let mut hunk = -1;
    text.lines()
        .take(MAX_LINES)
        .map(|l| {
            let head = l.starts_with("@@");
            if head {
                hunk += 1;
            }
            let kind = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff --git") || l.starts_with("index ") || head || l.starts_with("new file") || l.starts_with("deleted file") {
                4
            } else if l.starts_with('+') {
                1
            } else if l.starts_with('-') {
                2
            } else {
                0
            };
            Row { text: l.to_string(), kind, hunk, head }
        })
        .collect()
}

/// The patch of one hunk: the file header of the diff and that hunk (to apply in reverse).
pub(crate) fn hunk_patch(diff: &str, hunk: usize) -> Option<String> {
    let mut header = String::new();
    let mut current: i64 = -1;
    let mut body = String::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            current += 1;
        }
        if current < 0 {
            header.push_str(line);
            header.push('\n');
        } else if current as usize == hunk {
            body.push_str(line);
            body.push('\n');
        }
    }
    if body.is_empty() {
        None
    } else {
        Some(header + &body)
    }
}

/// A new file shown as added lines.
pub(crate) fn as_added(text: &str) -> Vec<(String, i32)> {
    text.lines().take(MAX_LINES).map(|l| (format!("+{l}"), 1)).collect()
}

pub(crate) fn branch_names(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('(')).map(str::to_string).collect()
}

/// A name that git accepts for a branch and that cannot be taken for an option.
pub(crate) fn valid_branch_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with('-')
        && !n.starts_with('/')
        && !n.ends_with('/')
        && !n.ends_with('.')
        && !n.ends_with(".lock")
        && !n.contains("..")
        && !n.contains("//")
        && !n.contains("@{")
        && !n.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
}

/// `git worktree list --porcelain`: (path, branch or "(detached)").
pub(crate) fn parse_worktrees(out: &str) -> Vec<(String, String)> {
    let mut list = Vec::new();
    for block in out.split("\n\n") {
        let mut path = None;
        let mut branch = "(detached)".to_string();
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = b.strip_prefix("refs/heads/").unwrap_or(b).to_string();
            } else if line == "bare" {
                branch = "(bare)".to_string();
            }
        }
        if let Some(p) = path {
            list.push((p, branch));
        }
    }
    list
}

/// Where the worktree of a branch goes: next to the repository, in "<repo>-worktrees".
pub(crate) fn worktree_dir(toplevel: &str, name: &str) -> String {
    let top = toplevel.trim_end_matches(['/', '\\']);
    let (parent, repo) = match top.rfind(['/', '\\']) {
        Some(i) => (&top[..i], &top[i + 1..]),
        None => (".", top),
    };
    let sep = if top.contains('\\') { '\\' } else { '/' };
    format!("{parent}{sep}{repo}-worktrees{sep}{}", name.replace('/', "-"))
}

fn stat_label(a: i64, d: i64) -> String {
    if a == 0 && d == 0 {
        String::new()
    } else {
        format!("+{a} −{d}")
    }
}

// ------------------------------------------------------------------ git

fn quiet(cmd: &mut Proc) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn finish(out: std::process::Output) -> Result<String, String> {
    if out.status.success() {
        let mut bytes = out.stdout;
        bytes.truncate(MAX_BYTES);
        Ok(String::from_utf8_lossy(&bytes).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err };
        Err(msg.lines().next().unwrap_or("コマンドが失敗しました").to_string())
    }
}

fn git(cwd: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Proc::new("git");
    cmd.arg("-C").arg(cwd).args(args).stdin(Stdio::null());
    quiet(&mut cmd);
    finish(cmd.output().map_err(|e| format!("git を起動できません: {e}"))?)
}

/// git with text on its standard input (`git apply -`).
fn git_in(cwd: &str, args: &[&str], input: &str) -> Result<String, String> {
    let mut cmd = Proc::new("git");
    cmd.arg("-C").arg(cwd).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("git を起動できません: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    }
    finish(child.wait_with_output().map_err(|e| e.to_string())?)
}

/// The GitHub CLI, with the sign-in the user has set up; nothing of it is read here.
fn gh(cwd: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Proc::new("gh");
    cmd.current_dir(cwd).args(args).stdin(Stdio::null());
    quiet(&mut cmd);
    finish(cmd.output().map_err(|e| format!("gh を起動できません: {e}"))?)
}

fn cwd() -> String {
    STATE.with(|s| s.borrow().active_cwd.clone())
}

// ------------------------------------------------------------------ screen

fn set_note(app: &AppWindow, text: &str) {
    app.global::<GitState>().set_note(text.into());
}

/// Show the diff of the file on the screen (with the selected line and the marks of the comments).
fn render_diff(app: &AppWindow) {
    let g = app.global::<GitState>();
    DATA.with(|d| {
        let d = d.borrow();
        let Some(cur) = &d.cur else {
            g.set_diff_title("".into());
            g.set_diff(ModelRc::from(Rc::new(VecModel::<GitLine>::default())));
            g.set_selected_line(-1);
            return;
        };
        let rows: Vec<Row> = if cur.untracked {
            as_added(&cur.raw).into_iter().map(|(text, kind)| Row { text, kind, hunk: -1, head: false }).collect()
        } else {
            diff_rows(&cur.raw)
        };
        let lines: Vec<GitLine> = rows
            .into_iter()
            .enumerate()
            .map(|(i, r)| GitLine {
                text: r.text.into(),
                kind: r.kind,
                hunk: r.hunk,
                head: r.head,
                selected: d.selected_line == Some(i),
                marked: d.comments.iter().any(|c| c.path == cur.path && c.line == i),
            })
            .collect();
        g.set_diff_title(cur.path.as_str().into());
        g.set_diff(ModelRc::from(Rc::new(VecModel::from(lines))));
        g.set_selected_line(d.selected_line.map(|i| i as i32).unwrap_or(-1));
    });
}

fn render_comments(app: &AppWindow) {
    let rows: Vec<GitComment> = DATA.with(|d| {
        d.borrow().comments.iter().map(|c| GitComment { place: format!("{}:{}", c.path, c.line + 1).into(), text: c.comment.as_str().into() }).collect()
    });
    app.global::<GitState>().set_comments(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// The branch under the composer (empty: not a git folder).
pub(crate) fn set_branch(app: &AppWindow, branch: &str) {
    app.global::<GitState>().set_branch(branch.into());
}

fn render_files(app: &AppWindow, selected: Option<usize>, stats: &HashMap<String, (i64, i64)>) {
    let rows: Vec<GitFile> = ENTRIES.with(|e| {
        e.borrow()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (a, d) = stats.get(&f.path).copied().unwrap_or((0, 0));
                GitFile {
                    path: f.path.as_str().into(),
                    status: status_letter(f).into(),
                    can_stage: can_stage(f),
                    staged: is_staged(f),
                    stat: stat_label(a, d).into(),
                    selected: Some(i) == selected,
                }
            })
            .collect()
    });
    app.global::<GitState>().set_files(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn base() -> String {
    DATA.with(|d| d.borrow().base.clone())
}

/// The numbers of the list: against HEAD, or against what is compared with.
fn numstat(cwd: &str) -> HashMap<String, (i64, i64)> {
    let base = base();
    let against = if base.is_empty() { "HEAD" } else { base.as_str() };
    git(cwd, &["diff", against, "--numstat"])
        .or_else(|_| git(cwd, &["diff", "--numstat"]))
        .map(|o| parse_numstat(&o))
        .unwrap_or_default()
}

/// Read the state of the folder again.
fn refresh(app: &AppWindow) {
    let cwd = cwd();
    let g = app.global::<GitState>();
    if cwd.is_empty() {
        set_note(app, "作業フォルダがありません");
        return;
    }
    match git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(b) => {
            let b = b.trim().to_string();
            set_branch(app, &b);
            STATE.with(|s| s.borrow_mut().branch = b);
        }
        Err(e) => {
            set_branch(app, "");
            ENTRIES.with(|x| x.borrow_mut().clear());
            render_files(app, None, &HashMap::new());
            DATA.with(|d| d.borrow_mut().cur = None);
            render_diff(app);
            set_note(app, &format!("git のフォルダではありません: {e}"));
            return;
        }
    }
    let base = base();
    let entries = if base.is_empty() {
        git(&cwd, &["status", "--porcelain=v1", "-z"]).map(|o| parse_status(&o)).unwrap_or_default()
    } else {
        git(&cwd, &["diff", "--name-status", &base]).map(|o| parse_name_status(&o)).unwrap_or_default()
    };
    ENTRIES.with(|x| *x.borrow_mut() = entries);
    render_files(app, None, &numstat(&cwd));
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.cur = None;
        d.selected_line = None;
    });
    render_diff(app);
    g.set_comparing(!base.is_empty());
    g.set_base_label(if base.is_empty() { "HEAD".into() } else { base.as_str().into() });
    let branches = git(&cwd, &["branch", "--format=%(refname:short)"]).map(|o| branch_names(&o)).unwrap_or_default();
    g.set_branches(ModelRc::from(Rc::new(VecModel::from(branches.into_iter().map(SharedString::from).collect::<Vec<_>>()))));
}

fn select(app: &AppWindow, i: usize) {
    let Some(entry) = ENTRIES.with(|e| e.borrow().get(i).cloned()) else { return };
    let cwd = cwd();
    let base = base();
    let untracked = entry.x == '?';
    let raw = if untracked {
        let path = std::path::Path::new(&cwd).join(&entry.path);
        match std::fs::read(&path) {
            Ok(mut bytes) => {
                bytes.truncate(MAX_BYTES);
                String::from_utf8_lossy(&bytes).to_string()
            }
            Err(e) => format!("読めません: {e}"),
        }
    } else if base.is_empty() {
        git(&cwd, &["diff", "HEAD", "--", &entry.path]).or_else(|_| git(&cwd, &["diff", "--", &entry.path])).unwrap_or_else(|e| e)
    } else {
        git(&cwd, &["diff", &base, "--", &entry.path]).unwrap_or_else(|e| e)
    };
    DATA.with(|d| {
        let mut d = d.borrow_mut();
        d.cur = Some(Cur { path: entry.path.clone(), raw, untracked, x: entry.x });
        d.selected_line = None;
    });
    render_files(app, Some(i), &numstat(&cwd));
    render_diff(app);
}

/// Run a git command that changes something, then show the new state.
fn change(app: &AppWindow, args: &[&str], done: &str) -> bool {
    let cwd = cwd();
    match git(&cwd, args) {
        Ok(out) => {
            refresh(app);
            let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            set_note(app, &if first.is_empty() { done.to_string() } else { format!("{done}: {first}") });
            true
        }
        Err(e) => {
            set_note(app, &e);
            false
        }
    }
}

fn set_mode(app: &AppWindow, mode: i32) {
    let g = app.global::<GitState>();
    g.set_mode(mode);
    let cwd = cwd();
    match mode {
        2 => {
            let current = g.get_branch().to_string();
            let mut bases = vec![("未コミットの変更 (HEAD)".to_string(), String::new())];
            for b in git(&cwd, &["branch", "--format=%(refname:short)"]).map(|o| branch_names(&o)).unwrap_or_default() {
                if b != current {
                    bases.push((b.clone(), b));
                }
            }
            for l in git(&cwd, &["log", "-n", "30", "--format=%h\t%s"]).unwrap_or_default().lines() {
                if let Some((h, s)) = l.split_once('\t') {
                    bases.push((format!("{h}  {s}"), h.to_string()));
                }
            }
            let labels: Vec<SharedString> = bases.iter().map(|b| b.0.as_str().into()).collect();
            DATA.with(|d| d.borrow_mut().bases = bases);
            g.set_bases(ModelRc::from(Rc::new(VecModel::from(labels))));
        }
        3 => refresh_worktrees(app),
        _ => {}
    }
}

fn refresh_worktrees(app: &AppWindow) {
    let list = git(&cwd(), &["worktree", "list", "--porcelain"]).map(|o| parse_worktrees(&o)).unwrap_or_default();
    let labels: Vec<SharedString> = list.iter().map(|(p, b)| format!("{b}  —  {p}").into()).collect();
    DATA.with(|d| d.borrow_mut().worktrees = list);
    app.global::<GitState>().set_worktrees(ModelRc::from(Rc::new(VecModel::from(labels))));
}

pub(crate) fn open(app: &AppWindow) {
    let g = app.global::<GitState>();
    g.set_open(true);
    g.set_mode(0);
    set_note(app, "");
    refresh(app);
}

fn ask(app: &AppWindow, text: &str, label: &str, p: Pending) {
    DATA.with(|d| d.borrow_mut().pending = Some(p));
    let g = app.global::<GitState>();
    g.set_confirm_label(label.into());
    g.set_confirm_text(text.into());
}

fn confirmed(app: &AppWindow) {
    let g = app.global::<GitState>();
    g.set_confirm_text("".into());
    let Some(p) = DATA.with(|d| d.borrow_mut().pending.take()) else { return };
    let cwd = cwd();
    match p {
        Pending::RevertFile => {
            let Some((path, untracked, x)) = DATA.with(|d| d.borrow().cur.as_ref().map(|c| (c.path.clone(), c.untracked, c.x))) else { return };
            let result = if untracked {
                std::fs::remove_file(std::path::Path::new(&cwd).join(&path)).map_err(|e| e.to_string())
            } else if x == 'A' {
                git(&cwd, &["rm", "-f", "--", &path]).map(|_| ())
            } else {
                git(&cwd, &["restore", "--source=HEAD", "--staged", "--worktree", "--", &path]).map(|_| ())
            };
            match result {
                Ok(()) => {
                    refresh(app);
                    set_note(app, &format!("元に戻しました: {path}"));
                }
                Err(e) => set_note(app, &e),
            }
        }
        Pending::RevertHunk(h) => {
            let Some((path, raw)) = DATA.with(|d| d.borrow().cur.as_ref().map(|c| (c.path.clone(), c.raw.clone()))) else { return };
            let Some(patch) = hunk_patch(&raw, h) else { return };
            match git_in(&cwd, &["apply", "-R", "--recount", "-"], &patch) {
                Ok(_) => {
                    refresh(app);
                    set_note(app, &format!("hunk を元に戻しました: {path}"));
                }
                Err(e) => set_note(app, &e),
            }
        }
        Pending::RemoveWorktree(path) => match git(&cwd, &["worktree", "remove", &path]) {
            Ok(_) => {
                refresh_worktrees(app);
                set_note(app, &format!("ワークツリーを削除しました: {path}"));
            }
            Err(e) => set_note(app, &e),
        },
    }
}

/// Run a slow command (push, a pull request) off the UI thread and show what it says.
fn background(app: &AppWindow, label: &str, job: impl FnOnce(String) -> Result<String, String> + Send + 'static) {
    let g = app.global::<GitState>();
    if g.get_busy() {
        return;
    }
    g.set_busy(true);
    set_note(app, &format!("{label}…"));
    let weak = app.as_weak();
    let cwd = cwd();
    std::thread::spawn(move || {
        let result = job(cwd);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(app) = weak.upgrade() else { return };
            app.global::<GitState>().set_busy(false);
            match result {
                Ok(text) => set_note(&app, &text),
                Err(e) => set_note(&app, &e),
            }
            refresh(&app);
        });
    });
}

pub(crate) fn register(app: &AppWindow, handle: &Rc<client::Handle>) {
    HANDLE.with(|c| *c.borrow_mut() = Some(handle.clone()));
    let g = app.global::<GitState>();
    let weak = app.as_weak();
    g.on_show(move || {
        if let Some(app) = weak.upgrade() {
            open(&app);
        }
    });
    let weak = app.as_weak();
    g.on_refresh(move || {
        if let Some(app) = weak.upgrade() {
            set_note(&app, "");
            refresh(&app);
            if app.global::<GitState>().get_mode() == 3 {
                refresh_worktrees(&app);
            }
        }
    });
    let weak = app.as_weak();
    g.on_set_mode(move |m| {
        if let Some(app) = weak.upgrade() {
            set_mode(&app, m);
        }
    });
    let weak = app.as_weak();
    g.on_select(move |i| {
        if let Some(app) = weak.upgrade() {
            select(&app, i.max(0) as usize);
        }
    });
    let weak = app.as_weak();
    g.on_toggle_stage(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some(e) = ENTRIES.with(|x| x.borrow().get(i.max(0) as usize).cloned()) else { return };
        if can_stage(&e) {
            change(&app, &["add", "--", &e.path], "ステージしました");
        } else {
            change(&app, &["restore", "--staged", "--", &e.path], "ステージを解除しました");
        }
    });
    let weak = app.as_weak();
    g.on_stage_all(move || {
        if let Some(app) = weak.upgrade() {
            change(&app, &["add", "-A"], "すべてステージしました");
        }
    });
    let weak = app.as_weak();
    g.on_commit(move || {
        let Some(app) = weak.upgrade() else { return };
        let g = app.global::<GitState>();
        let message = g.get_commit_message().trim().to_string();
        if message.is_empty() {
            set_note(&app, "コミットのメッセージを入力してください");
            return;
        }
        if !ENTRIES.with(|x| x.borrow().iter().any(is_staged)) {
            set_note(&app, "ステージした変更がありません");
            return;
        }
        if change(&app, &["commit", "-m", &message], "コミットしました") {
            g.set_commit_message("".into());
        }
    });
    let weak = app.as_weak();
    g.on_switch_branch(move |name| {
        let Some(app) = weak.upgrade() else { return };
        if name.as_str() == app.global::<GitState>().get_branch().as_str() {
            return;
        }
        if change(&app, &["switch", &name], "ブランチを切り替えました") {
            app.global::<GitState>().set_mode(0);
        }
    });
    let weak = app.as_weak();
    g.on_create_branch(move || {
        let Some(app) = weak.upgrade() else { return };
        let g = app.global::<GitState>();
        let name = g.get_new_branch().trim().to_string();
        if !valid_branch_name(&name) {
            set_note(&app, "ブランチの名前に使えない文字があります");
            return;
        }
        if change(&app, &["switch", "-c", &name], "ブランチを作りました") {
            g.set_new_branch("".into());
            g.set_mode(0);
        }
    });
    let weak = app.as_weak();
    g.on_review(move || {
        let Some(app) = weak.upgrade() else { return };
        let thread = STATE.with(|s| Some(s.borrow().active.clone()).filter(|a| !a.is_empty()));
        let Some(thread) = thread else { return };
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            h.send(Command::Rpc {
                tag: "slash.review".into(),
                method: "review/start".into(),
                params: json!({ "threadId": thread, "target": { "type": "uncommittedChanges" } }),
            });
        }
        app.global::<GitState>().set_open(false);
    });

    // What to compare with.
    let weak = app.as_weak();
    g.on_pick_base(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some((_, r)) = DATA.with(|d| d.borrow().bases.get(i.max(0) as usize).cloned()) else { return };
        DATA.with(|d| d.borrow_mut().base = r);
        app.global::<GitState>().set_mode(0);
        refresh(&app);
    });

    // Reverting is asked first: it cannot be undone.
    let weak = app.as_weak();
    g.on_revert_file(move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = DATA.with(|d| d.borrow().cur.as_ref().map(|c| c.path.clone())) else { return };
        ask(&app, &format!("「{path}」の変更をすべて捨てて、HEAD の状態に戻します。この操作は元に戻せません。"), "元に戻す", Pending::RevertFile);
    });
    let weak = app.as_weak();
    g.on_revert_hunk(move |h| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = DATA.with(|d| d.borrow().cur.as_ref().map(|c| c.path.clone())) else { return };
        if h < 0 {
            return;
        }
        ask(&app, &format!("「{path}」のこの hunk の変更を捨てます。この操作は元に戻せません。"), "元に戻す", Pending::RevertHunk(h as usize));
    });
    let weak = app.as_weak();
    g.on_confirm(move || {
        if let Some(app) = weak.upgrade() {
            confirmed(&app);
        }
    });
    let weak = app.as_weak();
    g.on_cancel_confirm(move || {
        if let Some(app) = weak.upgrade() {
            DATA.with(|d| d.borrow_mut().pending = None);
            app.global::<GitState>().set_confirm_text("".into());
        }
    });

    // Line comments: kept here and sent to the chat as one message.
    let weak = app.as_weak();
    g.on_select_line(move |i| {
        let Some(app) = weak.upgrade() else { return };
        DATA.with(|d| d.borrow_mut().selected_line = Some(i.max(0) as usize));
        render_diff(&app);
    });
    let weak = app.as_weak();
    g.on_add_comment(move || {
        let Some(app) = weak.upgrade() else { return };
        let g = app.global::<GitState>();
        let comment = g.get_comment_text().trim().to_string();
        if comment.is_empty() {
            return;
        }
        DATA.with(|d| {
            let mut d = d.borrow_mut();
            let (Some(line), Some(cur)) = (d.selected_line, d.cur.as_ref()) else { return };
            let text = cur.raw.lines().nth(line).unwrap_or("").to_string();
            let path = cur.path.clone();
            d.comments.push(Comment { path, line, text, comment });
        });
        g.set_comment_text("".into());
        render_comments(&app);
        render_diff(&app);
    });
    let weak = app.as_weak();
    g.on_remove_comment(move |i| {
        let Some(app) = weak.upgrade() else { return };
        DATA.with(|d| {
            let mut d = d.borrow_mut();
            if (i as usize) < d.comments.len() {
                d.comments.remove(i as usize);
            }
        });
        render_comments(&app);
        render_diff(&app);
    });
    let weak = app.as_weak();
    g.on_send_comments(move || {
        let Some(app) = weak.upgrade() else { return };
        let text = DATA.with(|d| {
            let d = d.borrow();
            if d.comments.is_empty() {
                return String::new();
            }
            let mut t = String::from("次の行へのコメントに対応してください。\n");
            for c in &d.comments {
                t.push_str(&format!("\n- {}:{}\n  `{}`\n  {}\n", c.path, c.line + 1, c.text.trim(), c.comment));
            }
            t
        });
        if text.is_empty() {
            return;
        }
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            crate::prompts::submit(&app, &h, text);
        }
        DATA.with(|d| d.borrow_mut().comments.clear());
        render_comments(&app);
        render_diff(&app);
        app.global::<GitState>().set_open(false);
    });

    // Push and a pull request: only when the button is pressed, with the user's own sign-in.
    let weak = app.as_weak();
    g.on_push(move || {
        let Some(app) = weak.upgrade() else { return };
        let branch = app.global::<GitState>().get_branch().to_string();
        if branch.is_empty() || branch == "HEAD" {
            set_note(&app, "ブランチがありません");
            return;
        }
        background(&app, "プッシュしています", move |cwd| {
            let has_upstream = git(&cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok();
            if has_upstream {
                git(&cwd, &["push"])?;
            } else {
                git(&cwd, &["push", "-u", "origin", &branch])?;
            }
            Ok(format!("プッシュしました: {branch}"))
        });
    });
    let weak = app.as_weak();
    g.on_create_pr(move |draft| {
        let Some(app) = weak.upgrade() else { return };
        background(&app, "PR を作成しています", move |cwd| {
            let mut args = vec!["pr", "create", "--fill"];
            if draft {
                args.push("--draft");
            }
            let out = gh(&cwd, &args)?;
            let url = out.lines().rev().find(|l| l.trim().starts_with("http")).unwrap_or("").trim().to_string();
            Ok(if url.is_empty() { "PR を作成しました".to_string() } else { format!("PR を作成しました: {url}") })
        });
    });

    // Worktrees.
    let weak = app.as_weak();
    g.on_new_worktree(move || {
        let Some(app) = weak.upgrade() else { return };
        let g = app.global::<GitState>();
        let name = g.get_new_worktree_name().trim().to_string();
        if !valid_branch_name(&name) {
            set_note(&app, "ブランチの名前に使えない文字があります");
            return;
        }
        let cwd = cwd();
        let top = match git(&cwd, &["rev-parse", "--show-toplevel"]) {
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                set_note(&app, &e);
                return;
            }
        };
        let dir = worktree_dir(&top, &name);
        if let Some(parent) = std::path::Path::new(&dir).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let exists = git(&cwd, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok();
        let result = if exists { git(&cwd, &["worktree", "add", &dir, &name]) } else { git(&cwd, &["worktree", "add", "-b", &name, &dir]) };
        match result {
            Ok(_) => {
                g.set_new_worktree_name("".into());
                refresh_worktrees(&app);
                set_note(&app, &format!("ワークツリーを作りました: {dir}"));
            }
            Err(e) => set_note(&app, &e),
        }
    });
    let weak = app.as_weak();
    g.on_thread_in_worktree(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some((path, _)) = DATA.with(|d| d.borrow().worktrees.get(i.max(0) as usize).cloned()) else { return };
        if let Some(h) = HANDLE.with(|c| c.borrow().clone()) {
            h.send(Command::NewThreadIn(path));
        }
        app.global::<GitState>().set_open(false);
    });
    let weak = app.as_weak();
    g.on_remove_worktree(move |i| {
        let Some(app) = weak.upgrade() else { return };
        let Some((path, _)) = DATA.with(|d| d.borrow().worktrees.get(i.max(0) as usize).cloned()) else { return };
        if i == 0 {
            set_note(&app, "元のフォルダは削除できません");
            return;
        }
        ask(&app, &format!("ワークツリー「{path}」を削除します。コミットしていない変更がある場合、git は削除を拒否します。"), "削除", Pending::RemoveWorktree(path));
    });

    demo(app);
}

/// Without the mouse (for the snapshot): INOCODE_GUI_GIT="open;select:0;mode:2;..." does the steps
/// one every 1.5 seconds from the 3rd second, through the same callbacks the buttons call.
fn demo(app: &AppWindow) {
    let Ok(script) = std::env::var("INOCODE_GUI_GIT") else { return };
    for (i, step) in script.split(';').enumerate() {
        let (weak, step) = (app.as_weak(), step.to_string());
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(3000 + 1500 * i as u64), move || {
            let Some(app) = weak.upgrade() else { return };
            let g = app.global::<GitState>();
            let (name, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
            let n = arg.parse::<i32>().unwrap_or(0);
            match name {
                "open" => g.invoke_show(),
                "select" => g.invoke_select(n),
                "mode" => g.invoke_set_mode(n),
                "pickbase" => g.invoke_pick_base(n),
                "line" => g.invoke_select_line(n),
                "comment" => {
                    g.set_comment_text(arg.into());
                    g.invoke_add_comment();
                }
                "sendcomments" => g.invoke_send_comments(),
                "revertfile" => g.invoke_revert_file(),
                "reverthunk" => g.invoke_revert_hunk(n),
                "confirm" => g.invoke_confirm(),
                "cancel" => g.invoke_cancel_confirm(),
                "newwt" => {
                    g.set_new_worktree_name(arg.into());
                    g.invoke_new_worktree();
                }
                "rmwt" => g.invoke_remove_worktree(n),
                "push" => g.invoke_push(),
                "pr" => g.invoke_create_pr(false),
                "stageall" => g.invoke_stage_all(),
                "commit" => {
                    g.set_commit_message(arg.into());
                    g.invoke_commit();
                }
                _ => {}
            }
        });
        TIMERS.with(|ts| ts.borrow_mut().push(t));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_entries_with_renames_and_new_files() {
        let out = " M src/a.rs\0A  new.rs\0?? notes.txt\0R  moved.rs\0old.rs\0MM both.rs\0 D gone.rs\0";
        let e = parse_status(out);
        let paths: Vec<&str> = e.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "new.rs", "notes.txt", "moved.rs", "both.rs", "gone.rs"]);
        let letters: Vec<&str> = e.iter().map(status_letter).collect();
        assert_eq!(letters, vec!["M", "A", "?", "R", "M", "D"]);
        // a change that is not staged can be staged; a staged one only unstaged
        assert!(can_stage(&e[0]) && !is_staged(&e[0]));
        assert!(!can_stage(&e[1]) && is_staged(&e[1]));
        assert!(can_stage(&e[2]) && !is_staged(&e[2]));
        assert!(can_stage(&e[4]) && is_staged(&e[4]));
        assert!(parse_status("").is_empty());
    }

    #[test]
    fn numstat_and_diff_rows() {
        let n = parse_numstat("3\t1\tsrc/a.rs\n-\t-\timg.png\n");
        assert_eq!(n["src/a.rs"], (3, 1));
        assert_eq!(n["img.png"], (0, 0));
        assert_eq!(stat_label(3, 1), "+3 −1");
        assert_eq!(stat_label(0, 0), "");
        let rows = diff_rows("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same\n@@ -9 +9 @@\n-a\n+b");
        let kinds: Vec<i32> = rows.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![4, 4, 4, 4, 2, 1, 0, 4, 2, 1]);
        let hunks: Vec<i32> = rows.iter().map(|l| l.hunk).collect();
        assert_eq!(hunks, vec![-1, -1, -1, 0, 0, 0, 0, 1, 1, 1]);
        assert!(rows[3].head && rows[7].head && !rows[4].head);
        assert_eq!(as_added("a\nb"), vec![("+a".to_string(), 1), ("+b".to_string(), 1)]);
    }

    #[test]
    fn hunk_patch_has_the_header_and_only_that_hunk() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n@@ -9 +9 @@\n-a\n+b\n";
        assert_eq!(hunk_patch(diff, 1).unwrap(), "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -9 +9 @@\n-a\n+b\n");
        assert_eq!(hunk_patch(diff, 0).unwrap(), "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n");
        assert!(hunk_patch(diff, 2).is_none());
    }

    #[test]
    fn name_status_of_a_comparison() {
        let e = parse_name_status("M\tsrc/a.rs\nA\tnew.rs\nR100\told.rs\tmoved.rs\nD\tgone.rs\n");
        let paths: Vec<&str> = e.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "new.rs", "moved.rs", "gone.rs"]);
        let letters: Vec<&str> = e.iter().map(status_letter).collect();
        assert_eq!(letters, vec!["M", "A", "R", "D"]);
        assert!(e.iter().all(|x| !is_staged(x)));
    }

    #[test]
    fn branch_names_and_validation() {
        assert_eq!(branch_names("main\nfeat/x\n(HEAD detached at abc)\n"), vec!["main", "feat/x"]);
        assert!(valid_branch_name("feat/new-thing"));
        for bad in ["", "-x", "a b", "a..b", "a~1", "x.lock", "/a", "a/", "a//b", "a:b", "a@{1}", "a\\b"] {
            assert!(!valid_branch_name(bad), "{bad}");
        }
    }

    #[test]
    fn worktrees() {
        let out = "worktree D:/w/repo\nHEAD abc\nbranch refs/heads/main\n\nworktree D:/w/repo-worktrees/x\nHEAD def\nbranch refs/heads/feat/x\n\nworktree D:/w/other\nHEAD 123\ndetached\n";
        assert_eq!(
            parse_worktrees(out),
            vec![
                ("D:/w/repo".to_string(), "main".to_string()),
                ("D:/w/repo-worktrees/x".to_string(), "feat/x".to_string()),
                ("D:/w/other".to_string(), "(detached)".to_string()),
            ]
        );
        assert_eq!(worktree_dir("D:\\w\\repo", "feat/x"), "D:\\w\\repo-worktrees\\feat-x");
        assert_eq!(worktree_dir("/home/u/repo/", "x"), "/home/u/repo-worktrees/x");
    }
}
