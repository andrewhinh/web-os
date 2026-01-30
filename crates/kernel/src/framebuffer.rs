extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use crate::{
    spinlock::Mutex,
    virtio_gpu::{GPU, Gpu, PixelFormat},
};

pub static FBCON: Mutex<FbConsole> = Mutex::new(FbConsole::new(), "fbcon");
pub static EARLY: Mutex<EarlyBuf> = Mutex::new(EarlyBuf::new(), "fbearly");

const BORDER_PADDING: usize = 2;
const LINE_SPACING: usize = 2;
const LETTER_SPACING: usize = 0;
const SCALE: usize = 2;
const SCROLLBACK_LINES: usize = 200;

const CHAR_W: usize = 8 * SCALE;
const CHAR_H: usize = 8 * SCALE;

// 8x8 monochrome bitmaps:
// - 1 byte per row, MSB is leftmost pixel
// - missing chars render as '?'
const FONT8X8: [[u8; 8]; 128] = include!("../font8x8.in");

pub struct FbConsole {
    inited: bool,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    stride: usize,
    fmt: PixelFormat,
    cols: usize,
    rows: usize,
    max_lines: usize,
    line_base: usize,
    line_count: usize,
    cursor_line: usize,
    cursor_col: usize,
    view_offset: usize,
    lines: Vec<Vec<u8>>,

    // dirty rectangle tracking so we only transfer/flush changed pixels
    dirty: bool,
    dirty_minx: usize,
    dirty_miny: usize,
    dirty_maxx: usize,
    dirty_maxy: usize,

    // ANSI parser for shell UX
    esc: bool,
    csi: bool,
    csi_num: usize,
    csi_have_num: bool,
}

impl Default for FbConsole {
    fn default() -> Self {
        Self::new()
    }
}

const EARLY_BUF_SIZE: usize = 8192;
pub struct EarlyBuf {
    buf: [u8; EARLY_BUF_SIZE],
    r: usize,
    w: usize,
    full: bool,
}

impl Default for EarlyBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl EarlyBuf {
    pub const fn new() -> Self {
        Self {
            buf: [0; EARLY_BUF_SIZE],
            r: 0,
            w: 0,
            full: false,
        }
    }

    pub fn push(&mut self, b: u8) {
        self.buf[self.w] = b;
        self.w = (self.w + 1) % EARLY_BUF_SIZE;
        if self.full {
            self.r = (self.r + 1) % EARLY_BUF_SIZE;
        }
        self.full = self.w == self.r;
    }

    pub fn pop(&mut self) -> Option<u8> {
        if !self.full && self.r == self.w {
            return None;
        }
        let b = self.buf[self.r];
        self.r = (self.r + 1) % EARLY_BUF_SIZE;
        self.full = false;
        Some(b)
    }
}

impl FbConsole {
    pub const fn new() -> Self {
        Self {
            inited: false,
            x: BORDER_PADDING,
            y: BORDER_PADDING,
            width: 0,
            height: 0,
            stride: 0,
            fmt: PixelFormat::Bgrx8888,
            cols: 0,
            rows: 0,
            max_lines: 0,
            line_base: 0,
            line_count: 0,
            cursor_line: 0,
            cursor_col: 0,
            view_offset: 0,
            lines: Vec::new(),

            dirty: false,
            dirty_minx: 0,
            dirty_miny: 0,
            dirty_maxx: 0,
            dirty_maxy: 0,

            esc: false,
            csi: false,
            csi_num: 0,
            csi_have_num: false,
        }
    }

    pub fn init(&mut self) {
        if self.inited {
            return;
        }

        let mut g = GPU.lock();
        if !g.try_init() {
            return;
        }
        let Some((w, h, stride, fmt)) = g.info() else {
            return;
        };

        self.width = w;
        self.height = h;
        self.stride = stride;
        self.fmt = fmt;
        self.x = BORDER_PADDING;
        self.y = BORDER_PADDING;
        self.inited = true;
        self.cols =
            (self.width.saturating_sub(2 * BORDER_PADDING) / (CHAR_W + LETTER_SPACING)).max(1);
        self.rows =
            (self.height.saturating_sub(2 * BORDER_PADDING) / (CHAR_H + LINE_SPACING)).max(1);
        self.max_lines = self.rows.saturating_add(SCROLLBACK_LINES).max(1);
        self.lines = (0..self.max_lines).map(|_| vec![b' '; self.cols]).collect();
        self.line_base = 0;
        self.line_count = 1;
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.view_offset = 0;

        // clear screen once, then replay any early boot output captured before the
        // GPU/scanout came up
        self.clear_locked(&mut g);
        self.flush_dirty_locked(&mut g);

        drop(g);
        loop {
            let b = { EARLY.lock().pop() };
            let Some(b) = b else { break };
            self.putc(b);
        }
    }

    fn mark_dirty_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if !self.inited || self.width == 0 || self.height == 0 {
            return;
        }
        if w == 0 || h == 0 {
            return;
        }

        let x0 = x.min(self.width - 1);
        let y0 = y.min(self.height - 1);
        let x1 = (x.saturating_add(w).saturating_sub(1)).min(self.width - 1);
        let y1 = (y.saturating_add(h).saturating_sub(1)).min(self.height - 1);
        if x1 < x0 || y1 < y0 {
            return;
        }

        if !self.dirty {
            self.dirty = true;
            self.dirty_minx = x0;
            self.dirty_miny = y0;
            self.dirty_maxx = x1;
            self.dirty_maxy = y1;
        } else {
            self.dirty_minx = self.dirty_minx.min(x0);
            self.dirty_miny = self.dirty_miny.min(y0);
            self.dirty_maxx = self.dirty_maxx.max(x1);
            self.dirty_maxy = self.dirty_maxy.max(y1);
        }
    }

    fn line_index(&self, abs_line: usize) -> usize {
        abs_line % self.max_lines
    }

    fn clear_line_buf(&mut self, abs_line: usize) {
        let idx = self.line_index(abs_line);
        for b in &mut self.lines[idx] {
            *b = b' ';
        }
    }

    fn visible_start_line(&self) -> usize {
        let end_line = self.line_base + self.line_count - 1;
        let mut start = if end_line + 1 > self.rows {
            end_line + 1 - self.rows
        } else {
            self.line_base
        };
        let max_offset = start.saturating_sub(self.line_base);
        let offset = self.view_offset.min(max_offset);
        start = start.saturating_sub(offset);
        start
    }

    fn update_cursor_xy(&mut self) {
        let cell_w = CHAR_W + LETTER_SPACING;
        let cell_h = CHAR_H + LINE_SPACING;
        let start = self.visible_start_line();
        let row = self
            .cursor_line
            .saturating_sub(start)
            .min(self.rows.saturating_sub(1));
        let col = self.cursor_col.min(self.cols.saturating_sub(1));
        self.x = BORDER_PADDING + col.saturating_mul(cell_w);
        self.y = BORDER_PADDING + row.saturating_mul(cell_h);
    }

    fn redraw_from_buffer(&mut self, g: &mut Gpu) {
        if let Some(fb) = g.fb_mut() {
            let bg = self.fmt.pack_rgba(0, 0, 0, 0xff);
            fb.fill(bg);
            self.mark_dirty_rect(0, 0, self.width, self.height);
        }

        let cell_w = CHAR_W + LETTER_SPACING;
        let cell_h = CHAR_H + LINE_SPACING;
        let start_line = self.visible_start_line();
        let end_line = self.line_base + self.line_count;

        for row in 0..self.rows {
            let abs = start_line + row;
            if abs >= end_line {
                continue;
            }
            let idx = self.line_index(abs);
            for col in 0..self.cols {
                let ch = self.lines[idx][col];
                if ch == b' ' {
                    continue;
                }
                let px = BORDER_PADDING + col * cell_w;
                let py = BORDER_PADDING + row * cell_h;
                self.draw_char_at(g, ch as char, px, py);
            }
        }

        self.update_cursor_xy();
    }

    fn clear_row_area(&mut self, g: &mut Gpu, row: usize) {
        let Some(fb) = g.fb_mut() else {
            return;
        };
        let bg = self.fmt.pack_rgba(0, 0, 0, 0xff);
        let cell_h = CHAR_H + LINE_SPACING;
        let y0 = BORDER_PADDING + row.saturating_mul(cell_h);
        let y1 = (y0 + cell_h).min(self.height);
        for y in y0..y1 {
            let off = y * self.stride;
            for p in &mut fb[off..(off + self.width)] {
                *p = bg;
            }
        }
        self.mark_dirty_rect(0, y0, self.width, y1 - y0);
    }

    fn draw_line_row(&mut self, g: &mut Gpu, row: usize, abs_line: usize) {
        self.clear_row_area(g, row);
        let end_line = self.line_base + self.line_count;
        if abs_line >= end_line {
            return;
        }
        let cell_w = CHAR_W + LETTER_SPACING;
        let cell_h = CHAR_H + LINE_SPACING;
        let idx = self.line_index(abs_line);
        for col in 0..self.cols {
            let ch = self.lines[idx][col];
            if ch == b' ' {
                continue;
            }
            let px = BORDER_PADDING + col * cell_w;
            let py = BORDER_PADDING + row * cell_h;
            self.draw_char_at(g, ch as char, px, py);
        }
    }

    fn scroll_view(&mut self, g: &mut Gpu, delta_lines: isize) {
        let lines = if delta_lines < 0 {
            (-delta_lines) as usize
        } else {
            delta_lines as usize
        };
        let line_px = CHAR_H + LINE_SPACING;
        let shift_px = lines.saturating_mul(line_px);
        if shift_px >= self.height {
            self.redraw_from_buffer(g);
            return;
        }
        let Some(fb) = g.fb_mut() else {
            return;
        };
        let total = self.height * self.stride;
        let scroll = shift_px * self.stride;
        let bg = self.fmt.pack_rgba(0, 0, 0, 0xff);

        if delta_lines > 0 {
            fb.copy_within(scroll..total, 0);
            for p in &mut fb[(total - scroll)..total] {
                *p = bg;
            }
        } else {
            fb.copy_within(0..(total - scroll), scroll);
            for p in &mut fb[0..scroll] {
                *p = bg;
            }
        }
        self.mark_dirty_rect(0, 0, self.width, self.height);

        let start_line = self.visible_start_line();
        if delta_lines > 0 {
            for i in 0..lines {
                let row = self.rows - lines + i;
                let abs_line = start_line + row;
                self.draw_line_row(g, row, abs_line);
            }
        } else {
            for i in 0..lines {
                let row = i;
                let abs_line = start_line + row;
                self.draw_line_row(g, row, abs_line);
            }
        }
    }

    fn ensure_bottom(&mut self, g: &mut Gpu) {
        if self.view_offset != 0 {
            self.view_offset = 0;
            self.redraw_from_buffer(g);
        }
    }

    fn adjust_scrollback(&mut self, g: &mut Gpu, delta: isize) {
        let max_offset = self.line_count.saturating_sub(self.rows);
        let cur = self.view_offset as isize;
        let mut next = cur.saturating_add(delta);
        if next < 0 {
            next = 0;
        }
        let max_off = max_offset as isize;
        if next > max_off {
            next = max_off;
        }
        let next = next as usize;
        if next != self.view_offset {
            let old_start = self.visible_start_line();
            self.view_offset = next;
            let new_start = self.visible_start_line();
            let diff = new_start as isize - old_start as isize;
            let diff_abs = if diff < 0 {
                (-diff) as usize
            } else {
                diff as usize
            };
            if diff_abs >= self.rows {
                self.redraw_from_buffer(g);
            } else if diff != 0 {
                self.scroll_view(g, diff);
                self.update_cursor_xy();
            } else {
                self.update_cursor_xy();
            }
        }
    }

    fn flush_dirty_locked(&mut self, g: &mut Gpu) {
        if !self.dirty {
            return;
        }
        let minx = self.dirty_minx;
        let miny = self.dirty_miny;
        let maxx = self.dirty_maxx;
        let maxy = self.dirty_maxy;
        self.dirty = false;
        self.dirty_minx = 0;
        self.dirty_miny = 0;
        self.dirty_maxx = 0;
        self.dirty_maxy = 0;
        g.flush_rect(minx, miny, maxx - minx + 1, maxy - miny + 1);
    }

    fn clear_locked(&mut self, g: &mut Gpu) {
        if let Some(fb) = g.fb_mut() {
            fb.fill(self.fmt.pack_rgba(0, 0, 0, 0xff));
            self.mark_dirty_rect(0, 0, self.width, self.height);
        }
        self.x = BORDER_PADDING;
        self.y = BORDER_PADDING;
        self.line_base = 0;
        self.line_count = 1;
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.view_offset = 0;
        for line in &mut self.lines {
            for b in line {
                *b = b' ';
            }
        }
        self.esc = false;
        self.csi = false;
        self.csi_num = 0;
        self.csi_have_num = false;
    }

    fn scroll_locked(&mut self, g: &mut Gpu, rows: usize) {
        let Some(fb) = g.fb_mut() else {
            return;
        };
        if rows == 0 || rows >= self.height {
            self.clear_locked(g);
            return;
        }

        let stride = self.stride;
        let bg = self.fmt.pack_rgba(0, 0, 0, 0xff);

        // framebuffer is row-major, so memmove whole pixel rows for speed
        let total = self.height * stride;
        let scroll = rows * stride;
        fb.copy_within(scroll..total, 0);
        for p in &mut fb[(total - scroll)..total] {
            *p = bg;
        }

        self.mark_dirty_rect(0, 0, self.width, self.height);
    }

    fn newline(&mut self, g: &mut Gpu) {
        let line_px = CHAR_H + LINE_SPACING;
        self.cursor_line = self.cursor_line.saturating_add(1);
        self.cursor_col = 0;
        if self.cursor_line >= self.line_base + self.line_count {
            if self.line_count < self.max_lines {
                self.line_count += 1;
            } else {
                self.line_base = self.line_base.saturating_add(1);
            }
            self.clear_line_buf(self.cursor_line);
        } else {
            self.clear_line_buf(self.cursor_line);
        }
        self.y += line_px;
        self.x = BORDER_PADDING;
        if self.y + CHAR_H + BORDER_PADDING >= self.height {
            self.scroll_locked(g, line_px);
            self.y = self.y.saturating_sub(line_px);
        }
    }

    fn cursor_left(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        self.cursor_col -= 1;
        self.x = BORDER_PADDING + self.cursor_col * (CHAR_W + LETTER_SPACING);
    }

    fn cursor_left_n(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.cursor_col = self.cursor_col.saturating_sub(n);
        self.x = BORDER_PADDING + self.cursor_col * (CHAR_W + LETTER_SPACING);
    }

    fn cursor_right_n(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let max_col = self.cols.saturating_sub(1);
        self.cursor_col = (self.cursor_col.saturating_add(n)).min(max_col);
        self.x = BORDER_PADDING + self.cursor_col * (CHAR_W + LETTER_SPACING);
    }

    fn erase_to_eol(&mut self, g: &mut Gpu) {
        let Some(fb) = g.fb_mut() else {
            return;
        };
        let bg = self.fmt.pack_rgba(0, 0, 0, 0xff);
        let x0 = self.x.min(self.width);
        let x1 = self.width.saturating_sub(BORDER_PADDING).min(self.width);
        if x1 <= x0 {
            return;
        }
        let y0 = self.y.min(self.height);
        let y1 = (self.y + CHAR_H + LINE_SPACING).min(self.height);
        if y1 <= y0 {
            return;
        }
        for y in y0..y1 {
            let off = y * self.stride;
            for p in &mut fb[(off + x0)..(off + x1)] {
                *p = bg;
            }
        }
        self.mark_dirty_rect(x0, y0, x1 - x0, y1 - y0);
        let idx = self.line_index(self.cursor_line);
        let start = self.cursor_col.min(self.cols);
        for b in self.lines[idx][start..].iter_mut() {
            *b = b' ';
        }
    }

    fn draw_char_at(&mut self, g: &mut Gpu, c: char, px: usize, py: usize) {
        let idx = if (c as u32) < 128 {
            c as usize
        } else {
            b'?' as usize
        };
        let glyph = FONT8X8[idx];

        let fg = self.fmt.pack_rgba(0, 255, 64, 0xff);
        let bg = self.fmt.pack_rgba(0, 0, 0, 0xff);

        let Some(buf) = g.fb_mut() else {
            return;
        };

        let mut minx = px;
        let mut miny = py;
        let mut maxx = px;
        let mut maxy = py;

        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                let on = (bits >> (7 - col)) & 1 == 1;
                let color = if on { fg } else { bg };

                for sy in 0..SCALE {
                    for sx in 0..SCALE {
                        let x = px + col * SCALE + sx;
                        let y = py + row * SCALE + sy;
                        if x >= self.width || y >= self.height {
                            continue;
                        }
                        let off = y * self.stride + x;
                        buf[off] = color;
                        minx = minx.min(x);
                        miny = miny.min(y);
                        maxx = maxx.max(x);
                        maxy = maxy.max(y);
                    }
                }
            }
        }

        self.mark_dirty_rect(minx, miny, maxx - minx + 1, maxy - miny + 1);
    }

    fn handle_ansi_locked(&mut self, g: &mut Gpu, b: u8) -> bool {
        if self.esc {
            self.esc = false;
            if b == b'[' {
                self.csi = true;
                self.csi_num = 0;
                self.csi_have_num = false;
            }
            return true;
        }

        if self.csi {
            match b {
                b'?' => {
                    return true;
                }
                b'0'..=b'9' => {
                    self.csi_have_num = true;
                    self.csi_num = self
                        .csi_num
                        .saturating_mul(10)
                        .saturating_add((b - b'0') as usize);
                    return true;
                }
                b';' => {
                    return true;
                }
                b'C' => {
                    let n = if self.csi_have_num { self.csi_num } else { 1 };
                    self.cursor_right_n(n);
                }
                b'D' => {
                    let n = if self.csi_have_num { self.csi_num } else { 1 };
                    self.cursor_left_n(n);
                }
                b'K' => {
                    let mode = if self.csi_have_num { self.csi_num } else { 0 };
                    match mode {
                        0 => self.erase_to_eol(g),
                        2 => {
                            self.x = BORDER_PADDING;
                            self.cursor_col = 0;
                            self.erase_to_eol(g);
                        }
                        _ => {}
                    }
                }
                b'J' => {
                    let mode = if self.csi_have_num { self.csi_num } else { 0 };
                    if mode == 2 {
                        self.clear_locked(g);
                    }
                }
                b'h' | b'l' => {}
                _ => {}
            }
            self.csi = false;
            self.csi_num = 0;
            self.csi_have_num = false;
            return true;
        }

        if b == 0x1b {
            self.esc = true;
            return true;
        }

        false
    }

    fn putc_locked(&mut self, g: &mut Gpu, byte: u8) {
        self.ensure_bottom(g);
        if self.handle_ansi_locked(g, byte) {
            return;
        }
        match byte {
            b'\n' => self.newline(g),
            b'\r' => {
                self.cursor_col = 0;
                self.x = BORDER_PADDING;
            }
            0x08 => self.cursor_left(),
            0x7f => self.cursor_left(),
            b'\t' => {
                let tab_w = 4;
                let cur = self.cursor_col;
                let next = ((cur / tab_w) + 1) * tab_w;
                let spaces = next.saturating_sub(cur).max(1);
                for _ in 0..spaces {
                    self.putc_locked(g, b' ');
                }
            }
            c => {
                if self.cursor_col >= self.cols {
                    self.newline(g);
                }
                if self.x + CHAR_W + BORDER_PADDING >= self.width {
                    self.newline(g);
                }
                if self.y + CHAR_H + BORDER_PADDING >= self.height {
                    self.newline(g);
                }

                let ch = if c.is_ascii_graphic() || c == b' ' {
                    c
                } else {
                    b'?'
                };
                let idx = self.line_index(self.cursor_line);
                let col = self.cursor_col.min(self.cols.saturating_sub(1));
                self.lines[idx][col] = ch;

                self.draw_char_at(g, ch as char, self.x, self.y);
                self.cursor_col = self.cursor_col.saturating_add(1);
                self.x = BORDER_PADDING + self.cursor_col * (CHAR_W + LETTER_SPACING);
            }
        }
    }

    pub fn putc(&mut self, byte: u8) {
        if !self.inited {
            EARLY.lock().push(byte);
            return;
        }

        let mut g = GPU.lock();
        if !g.is_inited() {
            return;
        }
        let g = &mut *g;

        self.putc_locked(g, byte);
        self.flush_dirty_locked(g);
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if !self.inited {
            let mut early = EARLY.lock();
            for &b in bytes {
                early.push(b);
            }
            return;
        }

        let mut g = GPU.lock();
        if !g.is_inited() {
            return;
        }
        let g = &mut *g;

        for &b in bytes {
            self.putc_locked(g, b);
        }
        self.flush_dirty_locked(g);
    }

    pub fn scrollback(&mut self, delta: isize) {
        if !self.inited {
            return;
        }
        let mut g = GPU.lock();
        if !g.is_inited() {
            return;
        }
        let g = &mut *g;
        self.adjust_scrollback(g, delta);
        self.flush_dirty_locked(g);
    }
}

pub fn init() {
    FBCON.lock().init()
}

pub fn putc(c: u8) {
    FBCON.lock().putc(c)
}

pub fn write(bytes: &[u8]) {
    FBCON.lock().write(bytes)
}

pub fn scrollback(delta: isize) {
    FBCON.lock().scrollback(delta)
}
