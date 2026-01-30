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

const CHAR_W: usize = 8 * SCALE;
const CHAR_H: usize = 8 * SCALE;

// 8x8 monochrome bitmaps:
// - 1 byte per row, MSB is leftmost pixel
// - missing chars render as '?'
const FONT8X8: [[u8; 8]; 128] = include!("../font8x8.in");
const FONT8X8_EXT_LATIN: [[u8; 8]; 96] = include!("../font8x8_ext_latin.in");
const FONT8X8_BOX: [[u8; 8]; 128] = include!("../font8x8_box.in");

pub struct FbConsole {
    inited: bool,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    stride: usize,
    fmt: PixelFormat,

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

    // UTF-8 decode state
    utf8_codepoint: u32,
    utf8_needed: u8,
    utf8_min: u32,
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

            dirty: false,
            dirty_minx: 0,
            dirty_miny: 0,
            dirty_maxx: 0,
            dirty_maxy: 0,

            esc: false,
            csi: false,
            csi_num: 0,
            csi_have_num: false,

            utf8_codepoint: 0,
            utf8_needed: 0,
            utf8_min: 0,
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
        self.esc = false;
        self.csi = false;
        self.csi_num = 0;
        self.csi_have_num = false;
        self.reset_utf8_state();
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
        self.y += line_px;
        self.x = BORDER_PADDING;
        if self.y + CHAR_H + BORDER_PADDING >= self.height {
            self.scroll_locked(g, line_px);
            self.y = self.y.saturating_sub(line_px);
        }
    }

    fn cursor_left(&mut self) {
        if self.x <= BORDER_PADDING {
            return;
        }
        self.x = self.x.saturating_sub(CHAR_W + LETTER_SPACING);
    }

    fn cursor_left_n(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let step = CHAR_W + LETTER_SPACING;
        let want = n.saturating_mul(step);
        let min_x = BORDER_PADDING;
        self.x = self.x.saturating_sub(want).max(min_x);
    }

    fn cursor_right_n(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let step = CHAR_W + LETTER_SPACING;
        let want = n.saturating_mul(step);
        let max_x = self
            .width
            .saturating_sub(BORDER_PADDING)
            .saturating_sub(CHAR_W);
        self.x = self.x.saturating_add(want).min(max_x);
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
    }

    fn glyph_for_char(c: char) -> &'static [u8; 8] {
        let cp = c as u32;
        if cp < 0x80 {
            &FONT8X8[cp as usize]
        } else if (0x00A0..=0x00FF).contains(&cp) {
            &FONT8X8_EXT_LATIN[(cp - 0x00A0) as usize]
        } else if (0x2500..=0x257F).contains(&cp) {
            &FONT8X8_BOX[(cp - 0x2500) as usize]
        } else {
            &FONT8X8[b'?' as usize]
        }
    }

    fn draw_char_at(&mut self, g: &mut Gpu, c: char, px: usize, py: usize) {
        let glyph = Self::glyph_for_char(c);

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

    fn reset_utf8_state(&mut self) {
        self.utf8_codepoint = 0;
        self.utf8_needed = 0;
        self.utf8_min = 0;
    }

    fn render_char_locked(&mut self, g: &mut Gpu, ch: char) {
        if self.x + CHAR_W + BORDER_PADDING >= self.width {
            self.newline(g);
        }
        if self.y + CHAR_H + BORDER_PADDING >= self.height {
            self.newline(g);
        }
        self.draw_char_at(g, ch, self.x, self.y);
        self.x += CHAR_W + LETTER_SPACING;
    }

    fn handle_ascii_byte_locked(&mut self, g: &mut Gpu, byte: u8) {
        if self.handle_ansi_locked(g, byte) {
            return;
        }
        match byte {
            b'\n' => self.newline(g),
            b'\r' => self.x = BORDER_PADDING,
            0x08 => self.cursor_left(),
            0x7f => self.cursor_left(),
            b'\t' => {
                let tab_w = 4 * (CHAR_W + LETTER_SPACING);
                let cur = self.x.saturating_sub(BORDER_PADDING);
                let next = ((cur / tab_w) + 1) * tab_w;
                let spaces = (next.saturating_sub(cur) / (CHAR_W + LETTER_SPACING)).max(1);
                for _ in 0..spaces {
                    self.putc_locked(g, b' ');
                }
            }
            c => {
                if c.is_ascii_graphic() || c == b' ' {
                    self.render_char_locked(g, c as char);
                }
            }
        }
    }

    fn handle_utf8_byte_locked(&mut self, g: &mut Gpu, byte: u8) {
        loop {
            if self.utf8_needed == 0 {
                match byte {
                    0xC2..=0xDF => {
                        self.utf8_needed = 1;
                        self.utf8_codepoint = u32::from(byte & 0x1F);
                        self.utf8_min = 0x80;
                        return;
                    }
                    0xE0..=0xEF => {
                        self.utf8_needed = 2;
                        self.utf8_codepoint = u32::from(byte & 0x0F);
                        self.utf8_min = 0x800;
                        return;
                    }
                    0xF0..=0xF4 => {
                        self.utf8_needed = 3;
                        self.utf8_codepoint = u32::from(byte & 0x07);
                        self.utf8_min = 0x10000;
                        return;
                    }
                    _ => {
                        self.render_char_locked(g, '\u{FFFD}');
                        return;
                    }
                }
            } else if (0x80..=0xBF).contains(&byte) {
                self.utf8_codepoint = (self.utf8_codepoint << 6) | u32::from(byte & 0x3F);
                self.utf8_needed = self.utf8_needed.saturating_sub(1);
                if self.utf8_needed == 0 {
                    let cp = self.utf8_codepoint;
                    let min = self.utf8_min;
                    self.reset_utf8_state();
                    if cp < min || cp > 0x10FFFF || (0xD800..=0xDFFF).contains(&cp) {
                        self.render_char_locked(g, '\u{FFFD}');
                    } else if let Some(ch) = core::char::from_u32(cp) {
                        self.render_char_locked(g, ch);
                    } else {
                        self.render_char_locked(g, '\u{FFFD}');
                    }
                }
                return;
            } else {
                self.reset_utf8_state();
                self.render_char_locked(g, '\u{FFFD}');
                continue;
            }
        }
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
        if byte < 0x80 {
            if self.utf8_needed != 0 {
                self.reset_utf8_state();
                self.render_char_locked(g, '\u{FFFD}');
            }
            self.handle_ascii_byte_locked(g, byte);
            return;
        }

        if self.esc || self.csi {
            self.esc = false;
            self.csi = false;
            self.csi_num = 0;
            self.csi_have_num = false;
        }

        self.handle_utf8_byte_locked(g, byte);
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
