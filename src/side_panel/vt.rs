//! A small terminal emulator: UTF-8 text, cursor movement, erase, scroll region and SGR colors.
//! It turns the bytes of a PTY into a grid of styled cells that the terminal view draws.

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Style {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub bold: bool,
    pub inverse: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Cell {
    ch: char,
    style: Style,
    tail: bool,
}

const BLANK: Cell = Cell { ch: ' ', style: Style { fg: None, bg: None, bold: false, inverse: false }, tail: false };

#[derive(Clone, PartialEq, Debug)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug)]
enum State {
    Ground,
    Esc,
    Charset,
    Csi(String),
    Osc,
    OscEsc,
}

pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    grid: Vec<Vec<Cell>>,
    cx: usize,
    cy: usize,
    style: Style,
    saved: (usize, usize),
    top: usize,
    bottom: usize,
    wrap_pending: bool,
    state: State,
    pending: Vec<u8>,
    pub cursor_visible: bool,
    /// Bytes the emulator answers with (cursor position report, device attributes).
    pub replies: Vec<u8>,
    /// Lines that scrolled off the top, oldest first.
    scrollback: Vec<Vec<Cell>>,
}

/// How many lines that scrolled off are kept.
const SCROLLBACK: usize = 3000;

const PALETTE: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b,
    0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
];

fn color256(n: u32) -> u32 {
    match n {
        0..=15 => PALETTE[n as usize],
        16..=231 => {
            let n = n - 16;
            let level = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
            (level(n / 36) << 16) | (level(n / 6 % 6) << 8) | level(n % 6)
        }
        _ => {
            let v = 8 + (n.min(255) - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

fn width(ch: char) -> usize {
    let c = ch as u32;
    if (0x1100..=0x115f).contains(&c)
        || (0x2e80..=0xa4cf).contains(&c)
        || (0xac00..=0xd7a3).contains(&c)
        || (0xf900..=0xfaff).contains(&c)
        || (0xfe30..=0xfe6f).contains(&c)
        || (0xff00..=0xff60).contains(&c)
        || (0xffe0..=0xffe6).contains(&c)
        || (0x1f300..=0x1faff).contains(&c)
    {
        2
    } else {
        1
    }
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Screen {
        let (cols, rows) = (cols.max(2), rows.max(2));
        Screen {
            cols,
            rows,
            grid: vec![vec![BLANK; cols]; rows],
            cx: 0,
            cy: 0,
            style: Style::default(),
            saved: (0, 0),
            top: 0,
            bottom: rows - 1,
            wrap_pending: false,
            state: State::Ground,
            pending: Vec::new(),
            cursor_visible: true,
            replies: Vec::new(),
            scrollback: Vec::new(),
        }
    }

    /// How far the view can be scrolled back, in lines.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(2), rows.max(2));
        if cols == self.cols && rows == self.rows {
            return;
        }
        // Keep the lines nearest the cursor when the screen gets shorter.
        if rows < self.rows && self.cy >= rows {
            let drop = self.cy + 1 - rows;
            let gone: Vec<Vec<Cell>> = self.grid.drain(0..drop).collect();
            self.scrollback.extend(gone);
            self.cy -= drop;
        }
        self.grid.resize(rows, vec![BLANK; self.cols]);
        for line in &mut self.grid {
            line.resize(cols, BLANK);
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.wrap_pending = false;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let (text, rest) = match std::str::from_utf8(&data) {
            Ok(s) => (s.to_string(), Vec::new()),
            Err(e) => {
                let valid = e.valid_up_to();
                let head = String::from_utf8_lossy(&data[..valid]).into_owned();
                match e.error_len() {
                    // An incomplete sequence at the end: wait for the next chunk.
                    None => (head, data[valid..].to_vec()),
                    Some(n) => {
                        let mut s = head;
                        s.push('\u{fffd}');
                        let tail = String::from_utf8_lossy(&data[valid + n..]).into_owned();
                        s.push_str(&tail);
                        (s, Vec::new())
                    }
                }
            }
        };
        self.pending = rest;
        for ch in text.chars() {
            self.step(ch);
        }
    }

    fn step(&mut self, ch: char) {
        match std::mem::replace(&mut self.state, State::Ground) {
            State::Ground => match ch {
                '\x1b' => self.state = State::Esc,
                '\r' => {
                    self.cx = 0;
                    self.wrap_pending = false;
                }
                '\n' | '\x0b' | '\x0c' => self.linefeed(),
                '\x08' => {
                    self.cx = self.cx.saturating_sub(1);
                    self.wrap_pending = false;
                }
                '\t' => {
                    self.cx = ((self.cx / 8 + 1) * 8).min(self.cols - 1);
                    self.wrap_pending = false;
                }
                c if (c as u32) < 0x20 || c == '\x7f' => {}
                c => self.put(c),
            },
            State::Esc => match ch {
                '[' => self.state = State::Csi(String::new()),
                ']' => self.state = State::Osc,
                '(' | ')' | '*' | '+' => self.state = State::Charset,
                '7' => self.saved = (self.cx, self.cy),
                '8' => {
                    self.cx = self.saved.0.min(self.cols - 1);
                    self.cy = self.saved.1.min(self.rows - 1);
                }
                'M' => self.reverse_index(),
                'D' => self.linefeed(),
                'E' => {
                    self.cx = 0;
                    self.linefeed();
                }
                'c' => {
                    let (c, r) = (self.cols, self.rows);
                    *self = Screen::new(c, r);
                }
                _ => {}
            },
            State::Charset => {}
            State::Osc => match ch {
                '\x07' => {}
                '\x1b' => self.state = State::OscEsc,
                _ => self.state = State::Osc,
            },
            State::OscEsc => {
                // ESC \ ends the string; anything else is ignored.
                if ch != '\\' {
                    self.state = State::Osc;
                }
            }
            State::Csi(mut params) => {
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    self.csi(&params, ch);
                } else if params.len() < 64 {
                    params.push(ch);
                    self.state = State::Csi(params);
                }
            }
        }
    }

    fn put(&mut self, ch: char) {
        let w = width(ch);
        if self.wrap_pending || self.cx + w > self.cols {
            self.cx = 0;
            self.linefeed();
        }
        self.wrap_pending = false;
        let style = self.style;
        self.grid[self.cy][self.cx] = Cell { ch, style, tail: false };
        if w == 2 && self.cx + 1 < self.cols {
            self.grid[self.cy][self.cx + 1] = Cell { ch: ' ', style, tail: true };
        }
        self.cx += w;
        if self.cx >= self.cols {
            self.cx = self.cols - 1;
            self.wrap_pending = true;
        }
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.bottom {
            self.scroll_up(1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cy == self.top {
            self.scroll_down(1);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            let gone = self.grid.remove(self.top);
            if self.top == 0 {
                self.scrollback.push(gone);
                if self.scrollback.len() > SCROLLBACK {
                    self.scrollback.remove(0);
                }
            }
            self.grid.insert(self.bottom, vec![BLANK; self.cols]);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            self.grid.remove(self.bottom);
            self.grid.insert(self.top, vec![BLANK; self.cols]);
        }
    }

    fn erase(&mut self, y: usize, from: usize, to: usize) {
        let cols = self.cols;
        let blank = Cell { style: Style { bg: self.style.bg, ..Style::default() }, ..BLANK };
        for x in from..to.min(cols) {
            self.grid[y][x] = blank;
        }
    }

    fn csi(&mut self, raw: &str, fin: char) {
        let private = raw.starts_with(['?', '>', '<', '=']);
        let body = raw.trim_start_matches(['?', '>', '<', '=']);
        let args: Vec<u32> = body.split(';').map(|p| p.split(':').next().unwrap_or("").parse().unwrap_or(0)).collect();
        let arg = |i: usize, default: u32| match args.get(i) {
            Some(&v) if v > 0 => v as usize,
            _ => default as usize,
        };
        match fin {
            'A' => self.cy = self.cy.saturating_sub(arg(0, 1)).max(if self.cy >= self.top { self.top } else { 0 }),
            'B' => self.cy = (self.cy + arg(0, 1)).min(if self.cy <= self.bottom { self.bottom } else { self.rows - 1 }),
            'C' => self.cx = (self.cx + arg(0, 1)).min(self.cols - 1),
            'D' => self.cx = self.cx.saturating_sub(arg(0, 1)),
            'E' => {
                self.cx = 0;
                self.cy = (self.cy + arg(0, 1)).min(self.rows - 1);
            }
            'F' => {
                self.cx = 0;
                self.cy = self.cy.saturating_sub(arg(0, 1));
            }
            'G' | '`' => self.cx = (arg(0, 1) - 1).min(self.cols - 1),
            'd' => self.cy = (arg(0, 1) - 1).min(self.rows - 1),
            'H' | 'f' => {
                self.cy = (arg(0, 1) - 1).min(self.rows - 1);
                self.cx = (arg(1, 1) - 1).min(self.cols - 1);
            }
            'J' => match args.first().copied().unwrap_or(0) {
                0 => {
                    self.erase(self.cy, self.cx, self.cols);
                    for y in self.cy + 1..self.rows {
                        self.erase(y, 0, self.cols);
                    }
                }
                1 => {
                    for y in 0..self.cy {
                        self.erase(y, 0, self.cols);
                    }
                    self.erase(self.cy, 0, self.cx + 1);
                }
                _ => {
                    for y in 0..self.rows {
                        self.erase(y, 0, self.cols);
                    }
                }
            },
            'K' => match args.first().copied().unwrap_or(0) {
                0 => self.erase(self.cy, self.cx, self.cols),
                1 => self.erase(self.cy, 0, self.cx + 1),
                _ => self.erase(self.cy, 0, self.cols),
            },
            'L' => {
                if self.cy >= self.top && self.cy <= self.bottom {
                    for _ in 0..arg(0, 1).min(self.bottom - self.cy + 1) {
                        self.grid.remove(self.bottom);
                        self.grid.insert(self.cy, vec![BLANK; self.cols]);
                    }
                }
            }
            'M' => {
                if self.cy >= self.top && self.cy <= self.bottom {
                    for _ in 0..arg(0, 1).min(self.bottom - self.cy + 1) {
                        self.grid.remove(self.cy);
                        self.grid.insert(self.bottom, vec![BLANK; self.cols]);
                    }
                }
            }
            'P' => {
                let n = arg(0, 1).min(self.cols - self.cx);
                let row = &mut self.grid[self.cy];
                row.drain(self.cx..self.cx + n);
                row.extend(std::iter::repeat(BLANK).take(n));
            }
            '@' => {
                let n = arg(0, 1).min(self.cols - self.cx);
                let row = &mut self.grid[self.cy];
                for _ in 0..n {
                    row.insert(self.cx, BLANK);
                }
                row.truncate(self.cols);
            }
            'X' => self.erase(self.cy, self.cx, self.cx + arg(0, 1)),
            'S' => self.scroll_up(arg(0, 1)),
            'T' => self.scroll_down(arg(0, 1)),
            'r' => {
                let top = arg(0, 1) - 1;
                let bottom = (if args.get(1).copied().unwrap_or(0) == 0 { self.rows } else { arg(1, 1) }) - 1;
                if top < bottom && bottom < self.rows {
                    self.top = top;
                    self.bottom = bottom;
                } else {
                    self.top = 0;
                    self.bottom = self.rows - 1;
                }
                self.cx = 0;
                self.cy = 0;
            }
            's' => self.saved = (self.cx, self.cy),
            'u' => {
                self.cx = self.saved.0.min(self.cols - 1);
                self.cy = self.saved.1.min(self.rows - 1);
            }
            'm' if !private => self.sgr(&args),
            'n' if !private => {
                if args.first() == Some(&6) {
                    self.replies.extend(format!("\x1b[{};{}R", self.cy + 1, self.cx + 1).bytes());
                } else if args.first() == Some(&5) {
                    self.replies.extend(b"\x1b[0n");
                }
            }
            'c' => {
                if raw.starts_with('>') {
                    self.replies.extend(b"\x1b[>0;0;0c");
                } else if !private {
                    self.replies.extend(b"\x1b[?1;0c");
                }
            }
            'h' | 'l' if private => {
                if args.contains(&25) {
                    self.cursor_visible = fin == 'h';
                }
            }
            _ => {}
        }
        self.wrap_pending = false;
    }

    fn sgr(&mut self, args: &[u32]) {
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            match a {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                22 => self.style.bold = false,
                7 => self.style.inverse = true,
                27 => self.style.inverse = false,
                30..=37 => self.style.fg = Some(PALETTE[(a - 30) as usize]),
                39 => self.style.fg = None,
                40..=47 => self.style.bg = Some(PALETTE[(a - 40) as usize]),
                49 => self.style.bg = None,
                90..=97 => self.style.fg = Some(PALETTE[(a - 90 + 8) as usize]),
                100..=107 => self.style.bg = Some(PALETTE[(a - 100 + 8) as usize]),
                38 | 48 => {
                    let color = match args.get(i + 1) {
                        Some(5) => {
                            let c = args.get(i + 2).map(|&n| color256(n));
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let g = |k: usize| args.get(i + k).copied().unwrap_or(0).min(255);
                            let c = (g(2) << 16) | (g(3) << 8) | g(4);
                            i += 4;
                            Some(c)
                        }
                        _ => None,
                    };
                    if let Some(c) = color {
                        if a == 38 {
                            self.style.fg = Some(c);
                        } else {
                            self.style.bg = Some(c);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// The screen as lines of same-style runs. The cursor cell is drawn as an inverse run.
    pub fn lines(&self) -> Vec<Vec<Run>> {
        self.lines_at(0)
    }

    /// The rows on view when the view is scrolled `offset` lines back (0 is the live screen).
    fn view_rows(&self, offset: usize) -> Vec<&Vec<Cell>> {
        let all = self.scrollback.len() + self.grid.len();
        let offset = offset.min(self.scrollback.len());
        let start = all - self.rows - offset;
        self.scrollback.iter().chain(self.grid.iter()).skip(start).take(self.rows).collect()
    }

    /// The same, as lines of runs; the cursor shows only on the live screen.
    pub fn lines_at(&self, offset: usize) -> Vec<Vec<Run>> {
        let live = offset == 0;
        let mut out = Vec::with_capacity(self.rows);
        for (y, row) in self.view_rows(offset).into_iter().enumerate() {
            let last = row.iter().rposition(|c| c.ch != ' ' || c.style.bg.is_some() || c.style.inverse);
            let mut end = last.map(|l| l + 1).unwrap_or(0);
            let show_cursor = live && self.cursor_visible && y == self.cy;
            if show_cursor {
                end = end.max(self.cx + 1);
            }
            let mut runs: Vec<Run> = Vec::new();
            for (x, cell) in row.iter().enumerate().take(end) {
                if cell.tail {
                    continue;
                }
                let mut style = cell.style;
                if show_cursor && x == self.cx {
                    style.inverse = !style.inverse;
                    runs.push(Run { text: cell.ch.to_string(), style });
                    continue;
                }
                match runs.last_mut() {
                    Some(r) if r.style == style => r.text.push(cell.ch),
                    _ => runs.push(Run { text: cell.ch.to_string(), style }),
                }
            }
            out.push(runs);
        }
        out
    }

    /// The rows on view as plain text, for copying.
    pub fn text_at(&self, offset: usize) -> Vec<String> {
        self.view_rows(offset)
            .into_iter()
            .map(|r| r.iter().filter(|c| !c.tail).map(|c| c.ch).collect::<String>().trim_end().to_string())
            .collect()
    }

    #[cfg(test)]
    pub fn text(&self) -> Vec<String> {
        self.grid.iter().map(|r| r.iter().filter(|c| !c.tail).map(|c| c.ch).collect::<String>().trim_end().to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_wraps_and_scrolls() {
        let mut s = Screen::new(5, 2);
        s.feed(b"abcdefg\r\nxy");
        assert_eq!(s.text(), vec!["fg", "xy"]);
    }

    #[test]
    fn cursor_movement_and_erase() {
        let mut s = Screen::new(10, 3);
        s.feed(b"hello\x1b[2;3Hab\x1b[1;1H\x1b[K");
        assert_eq!(s.text(), vec!["", "  ab", ""]);
    }

    #[test]
    fn colors_split_runs() {
        let mut s = Screen::new(10, 1);
        s.feed(b"a\x1b[31mb\x1b[0mc");
        s.cursor_visible = false;
        let runs = &s.lines()[0];
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].style.fg, Some(0xcd3131));
    }

    #[test]
    fn scrollback_keeps_what_scrolled_off() {
        let mut s = Screen::new(5, 2);
        s.feed(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(s.text(), vec!["three", "four"]);
        assert_eq!(s.scrollback_len(), 2);
        assert_eq!(s.text_at(1), vec!["two", "three"]);
        assert_eq!(s.text_at(2), vec!["one", "two"]);
        assert_eq!(s.text_at(9), vec!["one", "two"]);
    }

    #[test]
    fn cursor_report_and_utf8_split() {
        let mut s = Screen::new(10, 2);
        s.feed("あ".as_bytes()[..2].as_ref());
        s.feed("あ".as_bytes()[2..].as_ref());
        s.feed(b"\x1b[6n");
        assert_eq!(String::from_utf8(s.replies.clone()).unwrap(), "\x1b[1;3R");
    }
}
