/*
 * Copyright (C) 2016 Salvatore Sanfilippo <antirez at gmail dot com>
 *               2026, anoter43 <verretpoŝtadreso@tralalero.tralala>
 *
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are
 * met:
 *
 *  *  Redistributions of source code must retain the above copyright
 *     notice, this list of conditions and the following disclaimer.
 *
 *  *  Redistributions in binary form must reproduce the above copyright
 *     notice, this list of conditions and the following disclaimer in the
 *     documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
 * "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
 * LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
 * A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
 * HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
 * SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
 * LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */
use std::fs;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use libc::{BRKINT, CS8, ECHO, ICANON, ICRNL, IEXTEN, INPCK, ISIG, ISTRIP, IXON, OPOST, TCSAFLUSH, VMIN, VTIME};
use termios::{tcgetattr, tcsetattr};

const KILO_VERSION: &str = "0.0.1";
const TAB_STOP: usize = 8;
const KILO_QUIT_TIMES: u32 = 3;
const KILO_QUERY_LEN: usize = 256;

const HL_NORMAL: u8 = 0;
const HL_NONPRINT: u8 = 1;
const HL_COMMENT: u8 = 2;
const HL_MLCOMMENT: u8 = 3;
const HL_KEYWORD1: u8 = 4;
const HL_KEYWORD2: u8 = 5;
const HL_STRING: u8 = 6;
const HL_NUMBER: u8 = 7;
const HL_MATCH: u8 = 8;

struct Syntax {
    filematch: &'static [&'static str],
    keywords: &'static [&'static str],
    singleline_comment_start: &'static str,
    multiline_comment_start: &'static str,
    multiline_comment_end: &'static str,
}

static C_SYNTAX: Syntax = Syntax {
    filematch: &[".c", ".h", ".cpp", ".hpp", ".cc"],
    keywords: &[
        "auto", "break", "case", "continue", "default", "do", "else", "enum", "extern", "for",
        "goto", "if", "register", "return", "sizeof", "static", "struct", "switch", "typedef",
        "union", "volatile", "while", "NULL", "alignas", "alignof", "and", "and_eq", "asm",
        "bitand", "bitor", "class", "compl", "constexpr", "const_cast", "deltype", "delete",
        "dynamic_cast", "explicit", "export", "false", "friend", "inline", "mutable", "namespace",
        "new", "noexcept", "not", "not_eq", "nullptr", "operator", "or", "or_eq", "private",
        "protected", "public", "reinterpret_cast", "static_assert", "static_cast", "template",
        "this", "thread_local", "throw", "true", "try", "typeid", "typename", "virtual", "xor",
        "xor_eq", "int|", "long|", "double|", "float|", "char|", "unsigned|", "signed|", "void|",
        "short|", "auto|", "const|", "bool|",
    ],
    singleline_comment_start: "//",
    multiline_comment_start: "/*",
    multiline_comment_end: "*/",
};

static HLDB: &[Syntax] = std::slice::from_ref(&C_SYNTAX);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Byte(u8),
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Del,
    Home,
    End,
    PageUp,
    PageDown,
    Esc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

struct Row {
    chars: Vec<u8>,
    render: Vec<u8>,
    hl: Vec<u8>,
    hl_oc: bool,
}

impl Row {
    fn has_open_comment(&self) -> bool {
        self.hl.last() == Some(&HL_MLCOMMENT)
            && (self.render.len() < 2
                || !(self.render[self.render.len() - 2] == b'*'
                    && self.render[self.render.len() - 1] == b'/'))
    }
}

struct Editor {
    cx: isize,
    cy: isize,
    rowoff: isize,
    coloff: isize,
    screenrows: usize,
    screencols: usize,
    rows: Vec<Row>,
    dirty: bool,
    filename: String,
    statusmsg: String,
    statusmsg_time: Instant,
    syntax: Option<&'static Syntax>,
    rawmode: bool,
    orig_termios: Option<termios::Termios>,
    quit_times: u32,
}

fn is_separator(c: u8) -> bool {
    c == 0 || c.is_ascii_whitespace() || b",.()+-/*=~%[];".contains(&c)
}

fn is_print(c: u8) -> bool {
    (32..=126).contains(&c)
}

fn syntax_to_color(hl: u8) -> i32 {
    match hl {
        HL_COMMENT | HL_MLCOMMENT => 36,
        HL_KEYWORD1 => 33,
        HL_KEYWORD2 => 32,
        HL_STRING => 35,
        HL_NUMBER => 31,
        HL_MATCH => 34,
        _ => 37,
    }
}

fn read_byte_timeout() -> Option<u8> {
    let mut c = 0u8;
    loop {
        let n = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                &mut c as *mut u8 as *mut libc::c_void,
                1,
            )
        };
        if n == 1 {
            return Some(c);
        }
        if n == 0 {
            return None;
        }
        let err = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::EINTR {
            continue;
        }
        std::process::exit(1);
    }
}

fn read_byte_blocking() -> u8 {
    loop {
        if let Some(c) = read_byte_timeout() {
            return c;
        }
    }
}

fn read_key() -> Key {
    let c = read_byte_blocking();
    if c != 27 {
        return Key::Byte(c);
    }
    let Some(s0) = read_byte_timeout() else {
        return Key::Esc;
    };
    let Some(s1) = read_byte_timeout() else {
        return Key::Esc;
    };
    if s0 == b'[' {
        if s1.is_ascii_digit() {
            if let Some(s2) = read_byte_timeout() {
                if s2 == b'~' {
                    return match s1 {
                        b'3' => Key::Del,
                        b'5' => Key::PageUp,
                        b'6' => Key::PageDown,
                        _ => Key::Esc,
                    };
                }
            }
            return Key::Esc;
        }
        return match s1 {
            b'A' => Key::ArrowUp,
            b'B' => Key::ArrowDown,
            b'C' => Key::ArrowRight,
            b'D' => Key::ArrowLeft,
            b'H' => Key::Home,
            b'F' => Key::End,
            _ => Key::Esc,
        };
    } else if s0 == b'O' {
        return match s1 {
            b'H' => Key::Home,
            b'F' => Key::End,
            _ => Key::Esc,
        };
    }
    Key::Esc
}

fn write_stdout(bytes: &[u8]) -> Option<()> {
    let mut out = io::stdout();
    out.write_all(bytes).ok()?;
    out.flush().ok()?;
    Some(())
}

fn get_cursor_position() -> Option<(isize, isize)> {
    write_stdout(b"\x1b[6n")?;
    let mut buf = Vec::new();
    for _ in 0..31 {
        match read_byte_timeout() {
            Some(b) if b != b'R' => buf.push(b),
            _ => break,
        }
    }
    if buf.first() != Some(&27) || buf.get(1) != Some(&b'[') {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[2..]);
    let mut it = s.split(';');
    let rows: isize = it.next()?.trim().parse().ok()?;
    let cols: isize = it.next()?.trim().parse().ok()?;
    Some((rows, cols))
}

fn get_window_size() -> Option<(usize, usize)> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col != 0 {
            return Some((ws.ws_row as usize, ws.ws_col as usize));
        }
    }
    let (orig_row, orig_col) = get_cursor_position()?;
    write_stdout(b"\x1b[999C\x1b[999B")?;
    let (rows, cols) = get_cursor_position()?;
    write_stdout(format!("\x1b[{};{}H", orig_row, orig_col).as_bytes())?;
    Some((rows as usize, cols as usize))
}

impl Editor {
    fn new() -> Self {
        let mut e = Editor {
            cx: 0,
            cy: 0,
            rowoff: 0,
            coloff: 0,
            screenrows: 0,
            screencols: 0,
            rows: Vec::new(),
            dirty: false,
            filename: String::new(),
            statusmsg: String::new(),
            statusmsg_time: Instant::now(),
            syntax: None,
            rawmode: false,
            orig_termios: None,
            quit_times: KILO_QUIT_TIMES,
        };
        e.update_window_size();
        e
    }

    fn update_window_size(&mut self) {
        match get_window_size() {
            Some((rows, cols)) => {
                self.screenrows = rows.saturating_sub(2);
                self.screencols = cols;
            }
            None => {
                eprintln!("Unable to query the screen for size (columns / rows)");
                std::process::exit(1);
            }
        }
    }

    fn clamp_cursor(&mut self) {
        if self.cy > self.screenrows as isize {
            self.cy = self.screenrows as isize - 1;
        }
        if self.cx > self.screencols as isize {
            self.cx = self.screencols as isize - 1;
        }
    }

    fn enable_raw_mode(&mut self) -> io::Result<()> {
        if self.rawmode {
            return Ok(());
        }
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return Err(io::Error::from_raw_os_error(libc::ENOTTY));
        }
        let mut orig: termios::Termios = unsafe { std::mem::zeroed() };
        tcgetattr(libc::STDIN_FILENO, &mut orig)?;
        let mut raw: termios::Termios = unsafe { std::mem::zeroed() };
        tcgetattr(libc::STDIN_FILENO, &mut raw)?;
        raw.c_iflag &= !(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
        raw.c_oflag &= !OPOST;
        raw.c_cflag |= CS8;
        raw.c_lflag &= !(ECHO | ICANON | IEXTEN | ISIG);
        raw.c_cc[VMIN as usize] = 0;
        raw.c_cc[VTIME as usize] = 1;
        tcsetattr(libc::STDIN_FILENO, TCSAFLUSH, &raw)?;
        self.orig_termios = Some(orig);
        self.rawmode = true;
        Ok(())
    }

    fn disable_raw_mode(&mut self) {
        if self.rawmode {
            if let Some(orig) = &self.orig_termios {
                let _ = tcsetattr(libc::STDIN_FILENO, TCSAFLUSH, orig);
            }
            self.rawmode = false;
        }
    }

    fn select_syntax(&mut self, filename: &str) {
        for syn in HLDB {
            for pat in syn.filematch {
                if let Some(pos) = filename.find(pat) {
                    if !pat.starts_with('.') || pos + pat.len() == filename.len() {
                        self.syntax = Some(syn);
                        return;
                    }
                }
            }
        }
    }

    fn update_row(&mut self, idx: usize) {
        let chars = std::mem::take(&mut self.rows[idx].chars);
        let mut render = Vec::with_capacity(chars.len());
        for &c in &chars {
            if c == b'\t' {
                render.push(b' ');
                while (render.len() + 1) % TAB_STOP != 0 {
                    render.push(b' ');
                }
            } else {
                render.push(c);
            }
        }
        let r = &mut self.rows[idx];
        r.chars = chars;
        r.render = render;
        self.update_syntax(idx);
    }

    fn update_syntax(&mut self, idx: usize) {
        let rsize = self.rows[idx].render.len();
        if rsize == 0 {
            self.rows[idx].hl.clear();
            return;
        }
        let mut hl = vec![HL_NORMAL; rsize];
        let Some(syn) = self.syntax else {
            self.rows[idx].hl = hl;
            return;
        };
        let render = self.rows[idx].render.clone();
        let scs = syn.singleline_comment_start.as_bytes();
        let mcs = syn.multiline_comment_start.as_bytes();
        let mce = syn.multiline_comment_end.as_bytes();

        let mut i = 0usize;
        while i < render.len() && render[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut prev_sep = true;
        let mut in_string: Option<u8> = None;
        let mut in_comment = idx > 0 && self.rows[idx - 1].has_open_comment();

        while i < render.len() {
            let c = render[i];
            let next = render.get(i + 1).copied();

            if prev_sep && c == scs[0] && next == Some(scs[1]) {
                for b in hl[i..].iter_mut() {
                    *b = HL_COMMENT;
                }
                break;
            }

            if in_comment {
                hl[i] = HL_MLCOMMENT;
                if c == mce[0] && next == Some(mce[1]) {
                    hl[i + 1] = HL_MLCOMMENT;
                    i += 2;
                    in_comment = false;
                    prev_sep = true;
                    continue;
                }
                prev_sep = false;
                i += 1;
                continue;
            } else if c == mcs[0] && next == Some(mcs[1]) {
                hl[i] = HL_MLCOMMENT;
                hl[i + 1] = HL_MLCOMMENT;
                i += 2;
                in_comment = true;
                prev_sep = false;
                continue;
            }

            if let Some(q) = in_string {
                hl[i] = HL_STRING;
                if c == b'\\' {
                    if i + 1 < hl.len() {
                        hl[i + 1] = HL_STRING;
                    }
                    i += 2;
                    prev_sep = false;
                    continue;
                }
                if c == q {
                    in_string = None;
                }
                i += 1;
                continue;
            } else if c == b'"' || c == b'\'' {
                in_string = Some(c);
                hl[i] = HL_STRING;
                i += 1;
                prev_sep = false;
                continue;
            }

            if !is_print(c) {
                hl[i] = HL_NONPRINT;
                i += 1;
                prev_sep = false;
                continue;
            }

            let prev_hl = if i > 0 { hl[i - 1] } else { HL_NORMAL };
            if (c.is_ascii_digit() && (prev_sep || prev_hl == HL_NUMBER))
                || (c == b'.' && i > 0 && prev_hl == HL_NUMBER)
            {
                hl[i] = HL_NUMBER;
                i += 1;
                prev_sep = false;
                continue;
            }

            if prev_sep {
                let mut matched = false;
                for kw in syn.keywords.iter() {
                    let (base, kw2): (&str, bool) = match kw.strip_suffix('|') {
                        Some(b) => (b, true),
                        None => (*kw, false),
                    };
                    let kb = base.as_bytes();
                    let klen = kb.len();
                    if i + klen <= render.len()
                        && &render[i..i + klen] == kb
                        && is_separator(render.get(i + klen).copied().unwrap_or(0))
                    {
                        let h = if kw2 { HL_KEYWORD2 } else { HL_KEYWORD1 };
                        for b in hl[i..i + klen].iter_mut() {
                            *b = h;
                        }
                        i += klen;
                        matched = true;
                        break;
                    }
                }
                if matched {
                    prev_sep = false;
                    continue;
                }
            }

            prev_sep = is_separator(c);
            i += 1;
        }

        self.rows[idx].hl = hl;
        let oc = self.rows[idx].has_open_comment();
        if self.rows[idx].hl_oc != oc && idx + 1 < self.rows.len() {
            self.update_syntax(idx + 1);
        }
        self.rows[idx].hl_oc = oc;
    }

    fn insert_row(&mut self, at: usize, s: &[u8]) {
        if at > self.rows.len() {
            return;
        }
        self.rows.insert(
            at,
            Row {
                chars: s.to_vec(),
                render: Vec::new(),
                hl: Vec::new(),
                hl_oc: false,
            },
        );
        self.update_row(at);
        self.dirty = true;
    }

    fn del_row(&mut self, at: usize) {
        if at >= self.rows.len() {
            return;
        }
        self.rows.remove(at);
        self.dirty = true;
    }

    fn row_insert_char(&mut self, filerow: usize, at: usize, c: u8) {
        let row = &mut self.rows[filerow];
        let len = row.chars.len();
        if at > len {
            row.chars.resize(at, b' ');
            row.chars.push(c);
        } else {
            row.chars.insert(at, c);
        }
        self.update_row(filerow);
        self.dirty = true;
    }

    fn row_append_string(&mut self, filerow: usize, s: &[u8]) {
        self.rows[filerow].chars.extend_from_slice(s);
        self.update_row(filerow);
        self.dirty = true;
    }

    fn row_del_char(&mut self, filerow: usize, at: usize) {
        let row = &mut self.rows[filerow];
        if row.chars.len() <= at {
            return;
        }
        row.chars.remove(at);
        self.update_row(filerow);
        self.dirty = true;
    }

    fn insert_char(&mut self, c: u8) {
        let filerow = self.rowoff + self.cy;
        let filecol = self.coloff + self.cx;
        if filerow >= self.rows.len() as isize {
            while (self.rows.len() as isize) <= filerow {
                self.insert_row(self.rows.len(), b"");
            }
        }
        let fr = filerow as usize;
        self.row_insert_char(fr, filecol as usize, c);
        if self.cx == self.screencols as isize - 1 {
            self.coloff += 1;
        } else {
            self.cx += 1;
        }
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        let filerow = self.rowoff + self.cy;
        let filecol = self.coloff + self.cx;
        if filerow >= self.rows.len() as isize {
            if filerow == self.rows.len() as isize {
                self.insert_row(self.rows.len(), b"");
            } else {
                return;
            }
        } else {
            let fr = filerow as usize;
            let fc = (filecol as usize).min(self.rows[fr].chars.len());
            if fc == 0 {
                self.insert_row(fr, b"");
            } else {
                let tail: Vec<u8> = self.rows[fr].chars[fc..].to_vec();
                self.insert_row(fr + 1, &tail);
                self.rows[fr].chars.truncate(fc);
                self.update_row(fr);
            }
        }
        if self.cy == self.screenrows as isize - 1 {
            self.rowoff += 1;
        } else {
            self.cy += 1;
        }
        self.cx = 0;
        self.coloff = 0;
    }

    fn del_char(&mut self) {
        let filerow = self.rowoff + self.cy;
        let filecol = self.coloff + self.cx;
        if filerow >= self.rows.len() as isize {
            return;
        }
        let fr = filerow as usize;
        if filecol == 0 && fr == 0 {
            return;
        }
        if filecol == 0 {
            let fc = self.rows[fr - 1].chars.len();
            let cur: Vec<u8> = self.rows[fr].chars.clone();
            self.row_append_string(fr - 1, &cur);
            self.del_row(fr);
            if self.cy == 0 {
                self.rowoff -= 1;
            } else {
                self.cy -= 1;
            }
            self.cx = fc as isize;
            if self.cx >= self.screencols as isize {
                let shift = (self.screencols as isize - self.cx) + 1;
                self.cx -= shift;
                self.coloff += shift;
            }
        } else {
            self.row_del_char(fr, (filecol - 1) as usize);
            if self.cx == 0 && self.coloff != 0 {
                self.coloff -= 1;
            } else {
                self.cx -= 1;
            }
        }
        self.dirty = true;
    }

    fn rows_to_string(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        for r in &self.rows {
            buf.extend_from_slice(&r.chars);
            buf.push(b'\n');
        }
        buf
    }

    fn open(&mut self, filename: &str) {
        self.dirty = false;
        self.filename = filename.to_string();
        let data = match fs::read(filename) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("Opening file: {}", e);
                std::process::exit(1);
            }
        };
        for chunk in data.split_inclusive(|&b| b == b'\n') {
            let mut line = chunk.to_vec();
            if line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            self.insert_row(self.rows.len(), &line);
        }
        self.dirty = false;
    }

    fn save(&mut self) {
        let buf = self.rows_to_string();
        let result = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.filename)
            .and_then(|mut f| {
                f.set_len(buf.len() as u64)?;
                f.write_all(&buf)?;
                Ok(buf.len())
            });
        match result {
            Ok(len) => {
                self.dirty = false;
                self.set_status(format!("{} bytes written on disk", len));
            }
            Err(e) => self.set_status(format!("Can't save! I/O error: {}", e)),
        }
    }

    fn set_status<S: Into<String>>(&mut self, msg: S) {
        self.statusmsg = msg.into();
        self.statusmsg_time = Instant::now();
    }

    fn refresh_screen(&mut self) {
        let mut out: Vec<u8> = Vec::new();
        out.extend(b"\x1b[?25l");
        out.extend(b"\x1b[H");

        for y in 0..self.screenrows {
            let filerow = self.rowoff as usize + y;
            if filerow >= self.rows.len() {
                if self.rows.is_empty() && y == self.screenrows / 3 {
                    let welcome = format!("Kilo editor -- verison {}\x1b[0K\r\n", KILO_VERSION);
                    let wl = welcome.len();
                    if wl < self.screencols {
                        let mut padding = (self.screencols - wl) / 2;
                        if padding > 0 {
                            out.push(b'~');
                            padding -= 1;
                        }
                        for _ in 0..padding {
                            out.push(b' ');
                        }
                    }
                    out.extend(welcome.as_bytes());
                } else {
                    out.extend(b"~\x1b[0K\r\n");
                }
                continue;
            }

            let row = &self.rows[filerow];
            let start = self.coloff as usize;
            let mut len = row.render.len().saturating_sub(start);
            if len > self.screencols {
                len = self.screencols;
            }
            let mut current_color: i32 = -1;
            for j in 0..len {
                let c = row.render[start + j];
                let h = row.hl[start + j];
                if h == HL_NONPRINT {
                    out.extend(b"\x1b[7m");
                    out.push(if c <= 26 { b'@' + c } else { b'?' });
                    out.extend(b"\x1b[0m");
                } else if h == HL_NORMAL {
                    if current_color != -1 {
                        out.extend(b"\x1b[39m");
                        current_color = -1;
                    }
                    out.push(c);
                } else {
                    let color = syntax_to_color(h);
                    if color != current_color {
                        out.extend(format!("\x1b[{}m", color).as_bytes());
                        current_color = color;
                    }
                    out.push(c);
                }
            }
            out.extend(b"\x1b[39m\x1b[0K\r\n");
        }

        out.extend(b"\x1b[0K\x1b[7m");
        let status = format!(
            "{:.20} - {} lines {}",
            self.filename,
            self.rows.len(),
            if self.dirty { "(modified)" } else { "" }
        );
        let rstatus = format!("{}/{}", self.rowoff + self.cy + 1, self.rows.len());
        let mut slen = status.chars().count().min(self.screencols);
        for ch in status.chars().take(slen) {
            let mut b = [0u8; 4];
            out.extend(ch.encode_utf8(&mut b).as_bytes());
        }
        let rlen = rstatus.chars().count();
        while slen < self.screencols {
            if self.screencols - slen == rlen {
                out.extend(rstatus.as_bytes());
                break;
            }
            out.push(b' ');
            slen += 1;
        }
        out.extend(b"\x1b[0m\r\n\x1b[0K");
        if !self.statusmsg.is_empty()
            && self.statusmsg_time.elapsed() < Duration::from_secs(5)
        {
            for ch in self.statusmsg.chars().take(self.screencols) {
                let mut b = [0u8; 4];
                out.extend(ch.encode_utf8(&mut b).as_bytes());
            }
        }

        let mut cx: isize = 1;
        let filerow = self.rowoff + self.cy;
        if filerow < self.rows.len() as isize {
            let row = &self.rows[filerow as usize];
            let end = self.coloff + self.cx;
            let mut j = self.coloff;
            while j < end {
                if j < row.chars.len() as isize && row.chars[j as usize] == b'\t' {
                    cx += 7 - cx % 8;
                }
                cx += 1;
                j += 1;
            }
        }
        out.extend(format!("\x1b[{};{}H", self.cy + 1, cx).as_bytes());
        out.extend(b"\x1b[?25h");

        let mut so = io::stdout();
        let _ = so.write_all(&out);
        let _ = so.flush();
    }

    fn restore_hl(&mut self, saved: &mut Option<(usize, Vec<u8>)>) {
        if let Some((line, hl)) = saved.take() {
            let n = hl.len().min(self.rows[line].render.len());
            self.rows[line].hl[..n].copy_from_slice(&hl[..n]);
        }
    }

    fn find(&mut self) {
        let mut query: Vec<u8> = Vec::new();
        let mut last_match: isize = -1;
        let mut find_next: isize = 0;
        let mut saved_hl: Option<(usize, Vec<u8>)> = None;

        let saved_cx = self.cx;
        let saved_cy = self.cy;
        let saved_coloff = self.coloff;
        let saved_rowoff = self.rowoff;

        loop {
            self.set_status(format!(
                "Search: {} (Use ESC/Arrows/Enter)",
                String::from_utf8_lossy(&query)
            ));
            self.refresh_screen();

            match read_key() {
                Key::Del | Key::Byte(127) | Key::Byte(8) => {
                    query.pop();
                    last_match = -1;
                }
                Key::Esc => {
                    self.cx = saved_cx;
                    self.cy = saved_cy;
                    self.coloff = saved_coloff;
                    self.rowoff = saved_rowoff;
                    self.restore_hl(&mut saved_hl);
                    self.set_status("");
                    return;
                }
                Key::Byte(13) => {
                    self.restore_hl(&mut saved_hl);
                    self.set_status("");
                    return;
                }
                Key::ArrowRight | Key::ArrowDown => find_next = 1,
                Key::ArrowLeft | Key::ArrowUp => find_next = -1,
                Key::Byte(c) if is_print(c) => {
                    if query.len() < KILO_QUERY_LEN {
                        query.push(c);
                        last_match = -1;
                    }
                }
                _ => {}
            }

            if last_match == -1 {
                find_next = 1;
            }
            if find_next != 0 {
                let num = self.rows.len() as isize;
                let mut match_row: Option<(usize, usize)> = None;
                let mut current = last_match;
                if num > 0 {
                    for _ in 0..num {
                        current += find_next;
                        if current == -1 {
                            current = num - 1;
                        } else if current == num {
                            current = 0;
                        }
                        if let Some(off) = find_in_row(&self.rows[current as usize].render, &query) {
                            match_row = Some((current as usize, off));
                            break;
                        }
                    }
                }
                find_next = 0;

                self.restore_hl(&mut saved_hl);

                if let Some((row_idx, off)) = match_row {
                    last_match = row_idx as isize;
                    let rsize = self.rows[row_idx].render.len();
                    if rsize > 0 {
                        let qlen = query.len().min(rsize - off);
                        saved_hl = Some((row_idx, self.rows[row_idx].hl.clone()));
                        for h in self.rows[row_idx].hl[off..off + qlen].iter_mut() {
                            *h = HL_MATCH;
                        }
                    }
                    self.cy = 0;
                    self.cx = off as isize;
                    self.rowoff = row_idx as isize;
                    self.coloff = 0;
                    if self.cx > self.screencols as isize {
                        let diff = self.cx - self.screencols as isize;
                        self.cx -= diff;
                        self.coloff += diff;
                    }
                }
            }
        }
    }

    fn move_cursor(&mut self, dir: Dir) {
        let filerow = self.rowoff + self.cy;
        let filecol = self.coloff + self.cx;
        let rowlen = if filerow < self.rows.len() as isize {
            self.rows[filerow as usize].chars.len() as isize
        } else {
            0
        };

        match dir {
            Dir::Left => {
                if self.cx == 0 {
                    if self.coloff != 0 {
                        self.coloff -= 1;
                    } else if filerow > 0 {
                        self.cy -= 1;
                        self.cx = self.rows[(filerow - 1) as usize].chars.len() as isize;
                        if self.cx > self.screencols as isize - 1 {
                            self.coloff = self.cx - self.screencols as isize + 1;
                            self.cx = self.screencols as isize - 1;
                        }
                    }
                } else {
                    self.cx -= 1;
                }
            }
            Dir::Right => {
                if filerow < self.rows.len() as isize && filecol < rowlen {
                    if self.cx == self.screencols as isize - 1 {
                        self.coloff += 1;
                    } else {
                        self.cx += 1;
                    }
                } else if filerow < self.rows.len() as isize && filecol == rowlen {
                    self.cx = 0;
                    self.coloff = 0;
                    if self.cy == self.screenrows as isize - 1 {
                        self.rowoff += 1;
                    } else {
                        self.cy += 1;
                    }
                }
            }
            Dir::Up => {
                if self.cy == 0 {
                    if self.rowoff != 0 {
                        self.rowoff -= 1;
                    }
                } else {
                    self.cy -= 1;
                }
            }
            Dir::Down => {
                if filerow < self.rows.len() as isize {
                    if self.cy == self.screenrows as isize - 1 {
                        self.rowoff += 1;
                    } else {
                        self.cy += 1;
                    }
                }
            }
        }

        let filerow = self.rowoff + self.cy;
        let filecol = self.coloff + self.cx;
        let rowlen = if filerow < self.rows.len() as isize {
            self.rows[filerow as usize].chars.len() as isize
        } else {
            0
        };
        if filecol > rowlen {
            self.cx -= filecol - rowlen;
            if self.cx < 0 {
                self.coloff += self.cx;
                self.cx = 0;
            }
        }
    }

    fn process_keypress(&mut self, key: Key) -> bool {
        match key {
            Key::Byte(13) => self.insert_newline(),
            Key::Byte(3) => {}
            Key::Byte(17) => {
                if self.dirty && self.quit_times > 0 {
                    self.set_status(format!(
                        "WARNING!!! File has unsaved changes. Press Ctrl-Q {} more times to quit.",
                        self.quit_times
                    ));
                    self.quit_times -= 1;
                    return true;
                }
                return false;
            }
            Key::Byte(19) => self.save(),
            Key::Byte(6) => self.find(),
            Key::Byte(127) | Key::Byte(8) | Key::Del => self.del_char(),
            Key::PageUp | Key::PageDown => {
                if key == Key::PageUp && self.cy != 0 {
                    self.cy = 0;
                } else if key == Key::PageDown && self.cy != self.screenrows as isize - 1 {
                    self.cy = self.screenrows as isize - 1;
                }
                let times = self.screenrows;
                for _ in 0..times {
                    self.move_cursor(if key == Key::PageUp { Dir::Up } else { Dir::Down });
                }
            }
            Key::ArrowUp => self.move_cursor(Dir::Up),
            Key::ArrowDown => self.move_cursor(Dir::Down),
            Key::ArrowLeft => self.move_cursor(Dir::Left),
            Key::ArrowRight => self.move_cursor(Dir::Right),
            Key::Byte(12) => {}
            Key::Esc | Key::Home | Key::End => {}
            Key::Byte(c) => self.insert_char(c),
        }
        self.quit_times = KILO_QUIT_TIMES;
        true
    }
}

fn find_in_row(render: &[u8], q: &[u8]) -> Option<usize> {
    if q.is_empty() {
        return Some(0);
    }
    if render.len() < q.len() {
        return None;
    }
    render.windows(q.len()).position(|w| w == q)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: kilo <filename>");
        std::process::exit(1);
    }

    let mut editor = Editor::new();
    editor.select_syntax(&args[1]);
    editor.open(&args[1]);
    if let Err(e) = editor.enable_raw_mode() {
        eprintln!("Enabling raw mode: {}", e);
        std::process::exit(1);
    }
    editor.set_status("HELP: Ctrl-S = save | Ctrl-Q = quit | Ctrl-F = find");
    loop {
        if let Some((rows, cols)) = get_window_size() {
            let srows = rows.saturating_sub(2);
            if srows != editor.screenrows || cols != editor.screencols {
                editor.screenrows = srows;
                editor.screencols = cols;
                editor.clamp_cursor();
            }
        }
        editor.refresh_screen();
        let key = read_key();
        if !editor.process_keypress(key) {
            break;
        }
    }
    editor.disable_raw_mode();
}
