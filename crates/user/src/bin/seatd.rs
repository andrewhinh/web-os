#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;

use ulib::fs::{File, OpenOptions};
use ulib::io::{Read, Write};
use ulib::process::{Command, Stdio};
use ulib::sys::poll;
use ulib::{eprintln, println, seat, signal, socket, sys, thread};

const VNC_PORT_BASE: u16 = 5901;
const VNC_PORT_COUNT: usize = 8;

const WIDTH: usize = 1024;
const HEIGHT: usize = 768;

const BORDER_PADDING: usize = 2;
const LINE_SPACING: usize = 2;
const LETTER_SPACING: usize = 0;
const SCALE: usize = 2;
const CHAR_W: usize = 8 * SCALE;
const CHAR_H: usize = 8 * SCALE;

const FONT8X8: [[u8; 8]; 128] = include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/font8x8.in"));

struct ConsoleSurface {
    buf: Vec<u32>,
    width: usize,
    height: usize,
    stride: usize,
    x: usize,
    y: usize,
    fg: u32,
    bg: u32,
    esc: bool,
    csi: bool,
    csi_num: usize,
    csi_have_num: bool,
    dirty: bool,
    dirty_x0: usize,
    dirty_y0: usize,
    dirty_x1: usize,
    dirty_y1: usize,
}

impl ConsoleSurface {
    fn new() -> Self {
        let fg = pack_rgba(0, 255, 64);
        let bg = pack_rgba(0, 0, 0);
        Self {
            buf: alloc::vec![bg; WIDTH * HEIGHT],
            width: WIDTH,
            height: HEIGHT,
            stride: WIDTH,
            x: BORDER_PADDING,
            y: BORDER_PADDING,
            fg,
            bg,
            esc: false,
            csi: false,
            csi_num: 0,
            csi_have_num: false,
            dirty: false,
            dirty_x0: 0,
            dirty_y0: 0,
            dirty_x1: 0,
            dirty_y1: 0,
        }
    }

    fn clear(&mut self) {
        self.buf.fill(self.bg);
        self.x = BORDER_PADDING;
        self.y = BORDER_PADDING;
        self.esc = false;
        self.csi = false;
        self.csi_num = 0;
        self.csi_have_num = false;
        self.mark_dirty_full();
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn mark_dirty_full(&mut self) {
        self.dirty = true;
        self.dirty_x0 = 0;
        self.dirty_y0 = 0;
        self.dirty_x1 = self.width;
        self.dirty_y1 = self.height;
    }

    fn mark_dirty_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if w == 0 || h == 0 {
            return;
        }
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        if !self.dirty {
            self.dirty = true;
            self.dirty_x0 = x;
            self.dirty_y0 = y;
            self.dirty_x1 = x1;
            self.dirty_y1 = y1;
            return;
        }
        self.dirty_x0 = self.dirty_x0.min(x);
        self.dirty_y0 = self.dirty_y0.min(y);
        self.dirty_x1 = self.dirty_x1.max(x1);
        self.dirty_y1 = self.dirty_y1.max(y1);
    }

    fn clear_dirty(&mut self) {
        self.dirty = false;
        self.dirty_x0 = 0;
        self.dirty_y0 = 0;
        self.dirty_x1 = 0;
        self.dirty_y1 = 0;
    }

    fn scroll(&mut self, rows: usize) {
        if rows == 0 || rows >= self.height {
            self.clear();
            return;
        }
        let stride = self.stride;
        let total = self.height * stride;
        let scroll = rows * stride;
        self.buf.copy_within(scroll..total, 0);
        for p in &mut self.buf[(total - scroll)..total] {
            *p = self.bg;
        }
        self.mark_dirty_full();
    }

    fn newline(&mut self) {
        let line_px = CHAR_H + LINE_SPACING;
        self.y += line_px;
        self.x = BORDER_PADDING;
        if self.y + CHAR_H + BORDER_PADDING >= self.height {
            self.scroll(line_px);
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

    fn erase_to_eol(&mut self) {
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
            for p in &mut self.buf[(off + x0)..(off + x1)] {
                *p = self.bg;
            }
        }
        self.mark_dirty_rect(x0, y0, x1 - x0, y1 - y0);
    }

    fn draw_char_at(&mut self, c: char, px: usize, py: usize) {
        let idx = if (c as u32) < 128 {
            c as usize
        } else {
            b'?' as usize
        };
        let glyph = FONT8X8[idx];

        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                let on = (bits >> (7 - col)) & 1 == 1;
                let color = if on { self.fg } else { self.bg };

                for sy in 0..SCALE {
                    for sx in 0..SCALE {
                        let x = px + col * SCALE + sx;
                        let y = py + row * SCALE + sy;
                        if x >= self.width || y >= self.height {
                            continue;
                        }
                        let off = y * self.stride + x;
                        self.buf[off] = color;
                    }
                }
            }
        }
        self.mark_dirty_rect(px, py, CHAR_W, CHAR_H);
    }

    fn handle_ansi(&mut self, b: u8) -> bool {
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
                b'?' => return true,
                b'0'..=b'9' => {
                    self.csi_have_num = true;
                    self.csi_num = self
                        .csi_num
                        .saturating_mul(10)
                        .saturating_add((b - b'0') as usize);
                    return true;
                }
                b';' => return true,
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
                        0 => self.erase_to_eol(),
                        2 => {
                            self.x = BORDER_PADDING;
                            self.erase_to_eol();
                        }
                        _ => {}
                    }
                }
                b'J' => {
                    let mode = if self.csi_have_num { self.csi_num } else { 0 };
                    if mode == 2 {
                        self.clear();
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

    fn putc(&mut self, byte: u8) {
        if self.handle_ansi(byte) {
            return;
        }
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.x = BORDER_PADDING,
            0x08 => self.cursor_left(),
            0x7f => self.cursor_left(),
            b'\t' => {
                let tab_w = 4 * (CHAR_W + LETTER_SPACING);
                let cur = self.x.saturating_sub(BORDER_PADDING);
                let next = ((cur / tab_w) + 1) * tab_w;
                let spaces = (next.saturating_sub(cur) / (CHAR_W + LETTER_SPACING)).max(1);
                for _ in 0..spaces {
                    self.putc(b' ');
                }
            }
            c => {
                if self.x + CHAR_W + BORDER_PADDING >= self.width {
                    self.newline();
                }
                if self.y + CHAR_H + BORDER_PADDING >= self.height {
                    self.newline();
                }

                let ch = if c.is_ascii_graphic() || c == b' ' {
                    c as char
                } else {
                    '?'
                };

                self.draw_char_at(ch, self.x, self.y);
                self.x += CHAR_W + LETTER_SPACING;
            }
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.putc(b);
        }
    }
}

struct RfbState {
    input: Vec<u8>,
    want_update: bool,
    cursor: Option<(u16, u16)>,
}

impl RfbState {
    fn new() -> Self {
        Self {
            input: Vec::new(),
            want_update: true,
            cursor: None,
        }
    }
}

fn pack_rgba(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

#[derive(Default, Clone, Copy)]
struct WriteStats {
    bytes: usize,
    writes: usize,
    would_block: usize,
}

fn write_all(file: &mut File, buf: &[u8]) -> sys::Result<()> {
    let _ = write_all_stats(file, buf)?;
    Ok(())
}

fn write_all_stats(file: &mut File, buf: &[u8]) -> sys::Result<WriteStats> {
    let mut stats = WriteStats::default();
    let fd = file.get_fd();
    let mut fds = [poll::PollFd {
        fd,
        events: poll::OUT,
        revents: 0,
    }];
    let mut off = 0usize;
    while off < buf.len() {
        match file.write(&buf[off..]) {
            Ok(0) => return Err(sys::Error::WriteZero),
            Ok(n) => {
                off += n;
                stats.bytes += n;
                stats.writes += 1;
            }
            Err(sys::Error::WouldBlock) => {
                stats.would_block += 1;
                let _ = sys::poll(&mut fds, 1);
            }
            Err(sys::Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(stats)
}

fn read_exact(file: &mut File, buf: &mut [u8]) -> sys::Result<bool> {
    let mut off = 0usize;
    while off < buf.len() {
        match file.read(&mut buf[off..]) {
            Ok(0) => return Ok(false),
            Ok(n) => off += n,
            Err(sys::Error::WouldBlock) => {
                let _ = sys::sleep(1);
            }
            Err(sys::Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn handshake(conn: &mut File) -> sys::Result<()> {
    write_all(conn, b"RFB 003.008\n")?;
    let mut ver = [0u8; 12];
    if !read_exact(conn, &mut ver)? {
        return Err(sys::Error::NotConnected);
    }
    write_all(conn, &[1u8, 1u8])?;
    let mut sec = [0u8; 1];
    if !read_exact(conn, &mut sec)? {
        return Err(sys::Error::NotConnected);
    }
    write_all(conn, &0u32.to_be_bytes())?;
    let mut init = [0u8; 1];
    if !read_exact(conn, &mut init)? {
        return Err(sys::Error::NotConnected);
    }

    let mut resp = Vec::new();
    resp.extend_from_slice(&(WIDTH as u16).to_be_bytes());
    resp.extend_from_slice(&(HEIGHT as u16).to_be_bytes());
    resp.push(32);
    resp.push(24);
    resp.push(0);
    resp.push(1);
    resp.extend_from_slice(&255u16.to_be_bytes());
    resp.extend_from_slice(&255u16.to_be_bytes());
    resp.extend_from_slice(&255u16.to_be_bytes());
    resp.push(16);
    resp.push(8);
    resp.push(0);
    resp.extend_from_slice(&[0u8; 3]);
    let name = b"web-os seat";
    resp.extend_from_slice(&(name.len() as u32).to_be_bytes());
    resp.extend_from_slice(name);
    write_all(conn, &resp)?;
    Ok(())
}

fn handle_key(seat_id: usize, key: u32, down: bool) -> sys::Result<()> {
    if !down {
        return Ok(());
    }
    let mut buf = [0u8; 8];
    let n = match key {
        0x20..=0x7e => {
            buf[0] = key as u8;
            1
        }
        0xff08 => {
            buf[0] = 0x08;
            1
        }
        0xff09 => {
            buf[0] = b'\t';
            1
        }
        0xff0d => {
            buf[0] = b'\n';
            1
        }
        0xff1b => {
            buf[0] = 0x1b;
            1
        }
        0xff52 => {
            buf[..3].copy_from_slice(b"\x1b[A");
            3
        }
        0xff54 => {
            buf[..3].copy_from_slice(b"\x1b[B");
            3
        }
        0xff53 => {
            buf[..3].copy_from_slice(b"\x1b[C");
            3
        }
        0xff51 => {
            buf[..3].copy_from_slice(b"\x1b[D");
            3
        }
        0xff50 => {
            buf[..3].copy_from_slice(b"\x1b[H");
            3
        }
        0xff57 => {
            buf[..3].copy_from_slice(b"\x1b[F");
            3
        }
        0xff55 => {
            buf[..4].copy_from_slice(b"\x1b[5~");
            4
        }
        0xff56 => {
            buf[..4].copy_from_slice(b"\x1b[6~");
            4
        }
        0xff63 => {
            buf[..4].copy_from_slice(b"\x1b[2~");
            4
        }
        0xffff => {
            buf[..4].copy_from_slice(b"\x1b[3~");
            4
        }
        0xffbe => {
            buf[..3].copy_from_slice(b"\x1bOP");
            3
        }
        0xffbf => {
            buf[..3].copy_from_slice(b"\x1bOQ");
            3
        }
        0xffc0 => {
            buf[..3].copy_from_slice(b"\x1bOR");
            3
        }
        0xffc1 => {
            buf[..3].copy_from_slice(b"\x1bOS");
            3
        }
        0xffc2 => {
            buf[..5].copy_from_slice(b"\x1b[15~");
            5
        }
        0xffc3 => {
            buf[..5].copy_from_slice(b"\x1b[17~");
            5
        }
        0xffc4 => {
            buf[..5].copy_from_slice(b"\x1b[18~");
            5
        }
        0xffc5 => {
            buf[..5].copy_from_slice(b"\x1b[19~");
            5
        }
        0xffc6 => {
            buf[..5].copy_from_slice(b"\x1b[20~");
            5
        }
        0xffc7 => {
            buf[..5].copy_from_slice(b"\x1b[21~");
            5
        }
        0xffc8 => {
            buf[..5].copy_from_slice(b"\x1b[23~");
            5
        }
        0xffc9 => {
            buf[..5].copy_from_slice(b"\x1b[24~");
            5
        }
        _ => 0,
    };
    if n > 0 {
        let _ = seat::write_input(seat_id, &buf[..n])?;
    }
    Ok(())
}

fn parse_messages(state: &mut RfbState, seat_id: usize) -> sys::Result<()> {
    let mut idx = 0usize;
    while idx < state.input.len() {
        let msg_type = state.input[idx];
        match msg_type {
            0 => {
                let need = 1 + 3 + 16;
                if state.input.len() - idx < need {
                    break;
                }
                idx += need;
            }
            2 => {
                if state.input.len() - idx < 4 {
                    break;
                }
                let count =
                    u16::from_be_bytes([state.input[idx + 2], state.input[idx + 3]]) as usize;
                let need = 4 + count * 4;
                if state.input.len() - idx < need {
                    break;
                }
                idx += need;
            }
            3 => {
                let need = 1 + 1 + 2 + 2 + 2 + 2;
                if state.input.len() - idx < need {
                    break;
                }
                let incremental = state.input[idx + 1] != 0;
                let x = u16::from_be_bytes([state.input[idx + 2], state.input[idx + 3]]);
                let y = u16::from_be_bytes([state.input[idx + 4], state.input[idx + 5]]);
                let w = u16::from_be_bytes([state.input[idx + 6], state.input[idx + 7]]);
                let h = u16::from_be_bytes([state.input[idx + 8], state.input[idx + 9]]);
                // #region agent log
                println!(
                    "SEATDLOG|fbreq seat={} incr={} x={} y={} w={} h={}",
                    seat_id, incremental as u8, x, y, w, h
                );
                // #endregion
                state.want_update = true;
                idx += need;
            }
            4 => {
                let need = 1 + 1 + 2 + 4;
                if state.input.len() - idx < need {
                    break;
                }
                let down = state.input[idx + 1] != 0;
                let key = u32::from_be_bytes([
                    state.input[idx + 4],
                    state.input[idx + 5],
                    state.input[idx + 6],
                    state.input[idx + 7],
                ]);
                handle_key(seat_id, key, down)?;
                idx += need;
            }
            5 => {
                let need = 1 + 1 + 2 + 2;
                if state.input.len() - idx < need {
                    break;
                }
                let x = u16::from_be_bytes([state.input[idx + 2], state.input[idx + 3]]);
                let y = u16::from_be_bytes([state.input[idx + 4], state.input[idx + 5]]);
                state.cursor = Some((x, y));
                idx += need;
            }
            6 => {
                let need = 1 + 3 + 4;
                if state.input.len() - idx < need {
                    break;
                }
                let len = u32::from_be_bytes([
                    state.input[idx + 4],
                    state.input[idx + 5],
                    state.input[idx + 6],
                    state.input[idx + 7],
                ]) as usize;
                if state.input.len() - idx < need + len {
                    break;
                }
                idx += need + len;
            }
            _ => break,
        }
    }
    if idx > 0 {
        state.input.drain(0..idx);
    }
    Ok(())
}

fn draw_cursor(frame: &mut [u32], width: usize, height: usize, x: u16, y: u16) {
    let x = x as usize;
    let y = y as usize;
    let color = pack_rgba(0, 255, 64);
    for dy in 0..8 {
        for dx in 0..8 {
            let px = x + dx;
            let py = y + dy;
            if px >= width || py >= height {
                continue;
            }
            let off = py * width + px;
            frame[off] = color;
        }
    }
}

fn send_frame(
    conn: &mut File,
    surface: &ConsoleSurface,
    cursor: Option<(u16, u16)>,
    seat_id: usize,
) -> sys::Result<WriteStats> {
    let mut frame = surface.buf.clone();
    if let Some((x, y)) = cursor {
        draw_cursor(&mut frame, surface.width, surface.height, x, y);
    }

    let x0 = surface.dirty_x0.min(surface.width);
    let y0 = surface.dirty_y0.min(surface.height);
    let x1 = surface.dirty_x1.min(surface.width);
    let y1 = surface.dirty_y1.min(surface.height);
    let region_w = x1.saturating_sub(x0);
    let region_h = y1.saturating_sub(y0);
    if region_w == 0 || region_h == 0 {
        return Ok(WriteStats::default());
    }

    let tile_w = 64usize;
    let tile_h = 64usize;
    let tiles_x = (region_w + tile_w - 1) / tile_w;
    let tiles_y = (region_h + tile_h - 1) / tile_h;
    let tiles = tiles_x * tiles_y;
    let total_bytes = region_w * region_h * 4 + tiles * 16;
    // #region agent log
    println!(
        "SEATDLOG|send_frame_start seat={} tiles={} tile={}x{} region={}x{}@{},{} total={}",
        seat_id, tiles, tile_w, tile_h, region_w, region_h, x0, y0, total_bytes
    );
    // #endregion

    let mut stats = WriteStats::default();
    for ty in 0..tiles_y {
        let ry = y0 + ty * tile_h;
        let h = (y1 - ry).min(tile_h);
        for tx in 0..tiles_x {
            let rx = x0 + tx * tile_w;
            let w = (x1 - rx).min(tile_w);
            let mut msg = Vec::with_capacity(16 + w * h * 4);
            msg.push(0);
            msg.push(0);
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&(rx as u16).to_be_bytes());
            msg.extend_from_slice(&(ry as u16).to_be_bytes());
            msg.extend_from_slice(&(w as u16).to_be_bytes());
            msg.extend_from_slice(&(h as u16).to_be_bytes());
            msg.extend_from_slice(&0u32.to_be_bytes());
            for row in 0..h {
                let base = (ry + row) * surface.width + rx;
                for col in 0..w {
                    let pix = frame[base + col];
                    msg.extend_from_slice(&pix.to_le_bytes());
                }
            }
            let part = write_all_stats(conn, &msg)?;
            stats.bytes += part.bytes;
            stats.writes += part.writes;
            stats.would_block += part.would_block;
        }
    }

    // #region agent log
    println!("SEATDLOG|send_frame_done seat={} tiles={}", seat_id, tiles);
    // #endregion
    Ok(stats)
}

fn spawn_shell(seat_id: usize) -> sys::Result<usize> {
    let console = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/console")?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-i");
    cmd.stdin(Stdio::Fd(console.try_clone()?))
        .stdout(Stdio::Fd(console.try_clone()?))
        .stderr(Stdio::Fd(console))
        .seat(seat_id)
        .foreground(true);
    cmd.env("PS1", "web-os$ ");
    cmd.env("TERM", "xterm");
    let child = cmd.spawn()?;
    Ok(child.pid())
}

fn handle_client(mut conn: File, port: u16) -> sys::Result<()> {
    // #region agent log
    println!("SEATDLOG|client_start port={}", port);
    // #endregion
    let seat_id = seat::create()?;
    // #region agent log
    println!("SEATDLOG|seat_created port={} seat_id={}", port, seat_id);
    // #endregion
    let child_pid = match spawn_shell(seat_id) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = seat::destroy(seat_id);
            return Err(e);
        }
    };

    // #region agent log
    println!("SEATDLOG|handshake_start port={} seat_id={}", port, seat_id);
    // #endregion
    if let Err(e) = handshake(&mut conn) {
        // #region agent log
        eprintln!(
            "SEATDLOG|handshake_err port={} seat_id={} err={}",
            port, seat_id, e
        );
        // #endregion
        let _ = sys::kill(child_pid, signal::SIGTERM);
        let _ = seat::destroy(seat_id);
        return Err(e);
    }
    // #region agent log
    println!("SEATDLOG|handshake_ok port={} seat_id={}", port, seat_id);
    // #endregion
    let _ = conn.set_nonblock();

    let mut surface = ConsoleSurface::new();
    surface.clear();
    surface.clear_dirty();
    let mut rfb = RfbState::new();
    let mut out_buf = [0u8; 256];

    let mut exit_reason = "eof";
    let mut logged_frame = false;
    let mut logged_reads = 0usize;
    let mut logged_out = 0usize;
    let mut logged_stats = 0usize;
    loop {
        let mut progressed = false;

        match conn.read(&mut out_buf) {
            Ok(0) => {
                exit_reason = "conn_eof";
                break;
            }
            Ok(n) => {
                if logged_reads < 5 {
                    let first = out_buf[0];
                    // #region agent log
                    println!(
                        "SEATDLOG|conn_read seat={} n={} first={}",
                        seat_id, n, first
                    );
                    // #endregion
                    logged_reads += 1;
                }
                rfb.input.extend_from_slice(&out_buf[..n]);
                parse_messages(&mut rfb, seat_id)?;
                progressed = true;
            }
            Err(sys::Error::WouldBlock) => {}
            Err(sys::Error::Interrupted) => {}
            Err(_) => {
                exit_reason = "conn_err";
                break;
            }
        }

        match seat::read_output(seat_id, &mut out_buf) {
            Ok(0) => {}
            Ok(n) => {
                if logged_out < 5 {
                    let first = out_buf[0];
                    // #region agent log
                    println!("SEATDLOG|seat_out seat={} n={} first={}", seat_id, n, first);
                    // #endregion
                    logged_out += 1;
                }
                surface.write(&out_buf[..n]);
                progressed = true;
            }
            Err(_) => {
                exit_reason = "seat_read_err";
                break;
            }
        }

        if surface.dirty && rfb.want_update {
            if !logged_frame {
                // #region agent log
                println!(
                    "SEATDLOG|send_frame seat={} dirty={} want_update={} size={}x{}",
                    seat_id,
                    surface.dirty as u8,
                    rfb.want_update as u8,
                    surface.width,
                    surface.height
                );
                // #endregion
                logged_frame = true;
            }
            match send_frame(&mut conn, &surface, rfb.cursor, seat_id) {
                Ok(stats) => {
                    if logged_stats < 5 {
                        // #region agent log
                        println!(
                            "SEATDLOG|send_frame_stats seat={} bytes={} writes={} would_block={}",
                            seat_id, stats.bytes, stats.writes, stats.would_block
                        );
                        // #endregion
                        logged_stats += 1;
                    }
                }
                Err(_) => {
                    // #region agent log
                    eprintln!("SEATDLOG|send_frame_err seat={}", seat_id);
                    // #endregion
                    exit_reason = "send_frame_err";
                    break;
                }
            }
            surface.clear_dirty();
            rfb.want_update = false;
            progressed = true;
        }

        if !progressed {
            let _ = sys::sleep(1);
        }
    }

    let _ = sys::kill(child_pid, signal::SIGTERM);
    let _ = seat::destroy(seat_id);
    // #region agent log
    println!(
        "SEATDLOG|client_exit port={} seat_id={} reason={}",
        port, seat_id, exit_reason
    );
    // #endregion
    Ok(())
}

struct ListenerArgs {
    port: u16,
}

extern "C" fn listener_entry(arg1: usize, _arg2: usize) {
    let args = unsafe { Box::from_raw(arg1 as *mut ListenerArgs) };
    if let Err(e) = listen_loop(args.port) {
        eprintln!("seatd: port {} err={}", args.port, e);
    }
    sys::exit(0);
}

struct ClientArgs {
    conn: File,
    port: u16,
}

extern "C" fn client_entry(arg1: usize, _arg2: usize) {
    let args = unsafe { Box::from_raw(arg1 as *mut ClientArgs) };
    if let Err(e) = handle_client(args.conn, args.port) {
        eprintln!("seatd: client err={}", e);
    }
    sys::exit(0);
}

fn listen_loop(port: u16) -> sys::Result<()> {
    let addr = alloc::format!("0.0.0.0:{port}");
    let mut server = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0)?;
    socket::bind(&server, &addr)?;
    socket::listen(&server, 16)?;

    loop {
        let conn = socket::accept(&server)?;
        // #region agent log
        println!("SEATDLOG|accept port={}", port);
        // #endregion
        let args = Box::new(ClientArgs { conn, port });
        let _ = thread::thread_create(client_entry, Box::into_raw(args) as usize, 0);
    }
}

fn main() {
    println!(
        "seatd: listen ports {}..{}",
        VNC_PORT_BASE,
        VNC_PORT_BASE + VNC_PORT_COUNT as u16 - 1
    );
    for i in 0..VNC_PORT_COUNT {
        let port = VNC_PORT_BASE + i as u16;
        let args = Box::new(ListenerArgs { port });
        if let Err(e) = thread::thread_create(listener_entry, Box::into_raw(args) as usize, 0) {
            eprintln!("seatd: spawn listener {} err={}", port, e);
        }
    }
    loop {
        let _ = sys::sleep(1000);
    }
}
