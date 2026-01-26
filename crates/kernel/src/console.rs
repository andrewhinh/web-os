// Console input and output, to the uart.
// Reads are raw byte streams.
// Implements special input characters:
//   newline -- end of line
//   control-h -- backspace
//   control-u -- kill line
//   control-d -- end of line
//   control-p -- print process list

use core::num::Wrapping;

use crate::error::{Error::*, Result};
use crate::file::{DEVSW, Device, Major};
use crate::framebuffer;
use crate::proc::{Cpus, dump, either_copyin, either_copyout, kill_pgrp, sleep, wakeup};
use crate::signal::{SIGINT, SIGTSTP};
use crate::spinlock::Mutex;
use crate::uart;
use crate::vm::VirtAddr;

pub static SEATS: Mutex<Seats> = Mutex::new(Seats::new(), "seats");

const BS: u8 = 0x08;

// Control-x
const fn ctrl(x: u8) -> u8 {
    x - b'@'
}

const INPUT_BUF_SIZE: usize = 256;
const OUTPUT_BUF_SIZE: usize = 16384;
const MAX_SEATS: usize = 8;

pub struct Seats {
    seats: [SeatConsole; MAX_SEATS],
}

#[derive(Clone, Copy)]
struct SeatConsole {
    in_use: bool,
    buf: [u8; INPUT_BUF_SIZE],
    r: Wrapping<usize>, // Read index
    w: Wrapping<usize>, // Write index
    e: Wrapping<usize>, // Edit index
    out: [u8; OUTPUT_BUF_SIZE],
    out_r: usize,
    out_w: usize,
    out_full: bool,
    session: usize,
    fg_pgrp: usize,
}

impl Seats {
    const fn new() -> Self {
        Self {
            seats: [SeatConsole::new(); MAX_SEATS],
        }
    }

    fn seat_mut(&mut self, id: usize) -> Option<&mut SeatConsole> {
        if id >= MAX_SEATS {
            return None;
        }
        let seat = &mut self.seats[id];
        if seat.in_use { Some(seat) } else { None }
    }

    fn seat_mut_or_err(&mut self, id: usize) -> Result<&mut SeatConsole> {
        self.seat_mut(id).ok_or(BadFileDescriptor)
    }

    fn intr_locked(&mut self, seat_id: usize, c: u8) {
        let Some(seat) = self.seat_mut(seat_id) else {
            return;
        };
        match c {
            m if m == ctrl(b'C') => {
                let target = if seat.fg_pgrp == 0 {
                    Cpus::myproc().map(|p| p.inner.lock().pgid).unwrap_or(0)
                } else {
                    seat.fg_pgrp
                };
                if target != 0 {
                    let _ = kill_pgrp(target, SIGINT);
                }
            }
            m if m == ctrl(b'Z') => {
                let target = if seat.fg_pgrp == 0 {
                    Cpus::myproc().map(|p| p.inner.lock().pgid).unwrap_or(0)
                } else {
                    seat.fg_pgrp
                };
                if target != 0 {
                    let _ = kill_pgrp(target, SIGTSTP);
                }
            }
            // Print process list
            m if m == ctrl(b'P') => dump(),
            // Kill line
            m if m == ctrl(b'U') => {
                while seat.e != seat.w
                    && seat.buf[(seat.e - Wrapping(1)).0 % INPUT_BUF_SIZE] != b'\n'
                {
                    seat.e -= Wrapping(1);
                    output_push(seat, seat_id, ctrl(b'H'));
                }
            }
            // Backspace
            m if m == ctrl(b'H') | b'\x7f' => {
                if seat.e != seat.w {
                    seat.e -= Wrapping(1);
                    output_push(seat, seat_id, ctrl(b'H'));
                }
            }
            _ => {
                if c != 0 && (seat.e - seat.r).0 < INPUT_BUF_SIZE {
                    let c = if c == b'\r' { b'\n' } else { c };

                    // echo back to the user
                    output_push(seat, seat_id, c);

                    // store for consumption by CONS.read().
                    let e_idx = seat.e.0 % INPUT_BUF_SIZE;
                    seat.buf[e_idx] = c;
                    seat.e += Wrapping(1);

                    if c == b'\n' || c == ctrl(b'D') || (seat.e - seat.r).0 == INPUT_BUF_SIZE {
                        // wake up CONS.read() if a whole line (or end of line)
                        // has arrived
                        seat.w = seat.e;
                        wakeup(&seat.r as *const _ as usize);
                    }
                }
            }
        }
    }
}

impl SeatConsole {
    const fn new() -> Self {
        Self {
            in_use: false,
            buf: [0; INPUT_BUF_SIZE],
            r: Wrapping(0),
            w: Wrapping(0),
            e: Wrapping(0),
            out: [0; OUTPUT_BUF_SIZE],
            out_r: 0,
            out_w: 0,
            out_full: false,
            session: 0,
            fg_pgrp: 0,
        }
    }

    fn reset(&mut self) {
        self.buf = [0; INPUT_BUF_SIZE];
        self.r = Wrapping(0);
        self.w = Wrapping(0);
        self.e = Wrapping(0);
        self.out = [0; OUTPUT_BUF_SIZE];
        self.out_r = 0;
        self.out_w = 0;
        self.out_full = false;
        self.session = 0;
        self.fg_pgrp = 0;
    }

    fn output_push(&mut self, b: u8) {
        self.out[self.out_w] = b;
        self.out_w = (self.out_w + 1) % OUTPUT_BUF_SIZE;
        if self.out_full {
            self.out_r = (self.out_r + 1) % OUTPUT_BUF_SIZE;
        } else if self.out_w == self.out_r {
            self.out_full = true;
        }
    }

    fn output_pop(&mut self) -> Option<u8> {
        if !self.out_full && self.out_r == self.out_w {
            return None;
        }
        let b = self.out[self.out_r];
        self.out_r = (self.out_r + 1) % OUTPUT_BUF_SIZE;
        self.out_full = false;
        Some(b)
    }
}

impl Device for Mutex<Seats> {
    // user read()s from the console go here.
    // copy (up to) a whole input line to dst.
    //
    fn read(&self, mut dst: VirtAddr, mut n: usize, _offset: usize) -> Result<usize> {
        let seat_id = current_seat_id();
        let p = Cpus::myproc().unwrap();
        let mut seats = self.lock();
        let mut seat = seats.seat_mut_or_err(seat_id)?;

        let target = n;
        while n > 0 {
            // wait until interrupt handler has put some
            // input into seat buf
            while seat.r == seat.w {
                if p.inner.lock().killed {
                    return Err(Interrupted);
                }
                let addr = &seat.r as *const _ as usize;
                seats = sleep(addr, seats);
                seat = seats.seat_mut_or_err(seat_id)?;
            }
            let c = seat.buf[seat.r.0 % INPUT_BUF_SIZE];
            seat.r += Wrapping(1);

            if c == ctrl(b'D') {
                // end of line
                if n < target {
                    // Save ^D for next time, to make sure
                    // caller gets a 0-bytes result.
                    seat.r -= Wrapping(1);
                }
                break;
            }

            // copy the input byte to the user-space buffer.
            either_copyout(dst, &c)?;

            dst += 1;
            n -= 1;

            if c == b'\n' {
                // a whole line has arrived, return to
                // the user-level read().
                break;
            }
        }

        Ok(target - n)
    }

    // user write()s to the console go here.
    //
    fn write(&self, src: VirtAddr, n: usize, _offset: usize) -> Result<usize> {
        if n == 0 {
            return Ok(0);
        }
        let seat_id = current_seat_id();

        let mut buf = [0u8; 512];
        let mut written = 0usize;
        let mut src = src;
        while written < n {
            let m = core::cmp::min(buf.len(), n - written);
            either_copyin(&mut buf[..m], src)?;

            {
                let mut seats = SEATS.lock();
                let seat = seats.seat_mut_or_err(seat_id)?;
                for &b in &buf[..m] {
                    seat.output_push(b);
                }
            }
            if seat_id == 0 {
                for &b in &buf[..m] {
                    uart::UART.putc(b);
                }
                framebuffer::write(&buf[..m]);
            }

            written += m;
            src += m;
        }
        Ok(written)
    }

    fn major(&self) -> Major {
        Major::Console
    }
}

pub fn init() {
    unsafe { uart::init() }
    {
        let mut seats = SEATS.lock();
        seats.seats[0].in_use = true;
        seats.seats[0].reset();
    }
    DEVSW.set(Major::Console, &SEATS).unwrap();
}

pub fn readable() -> bool {
    let seat_id = current_seat_id();
    let mut seats = SEATS.lock();
    let Some(seat) = seats.seat_mut(seat_id) else {
        return false;
    };
    seat.r != seat.w
}

pub fn session() -> usize {
    let seat_id = current_seat_id();
    let mut seats = SEATS.lock();
    let Some(seat) = seats.seat_mut(seat_id) else {
        return 0;
    };
    seat.session
}

pub fn fg_pgrp() -> usize {
    let seat_id = current_seat_id();
    let mut seats = SEATS.lock();
    let Some(seat) = seats.seat_mut(seat_id) else {
        return 0;
    };
    seat.fg_pgrp
}

pub fn is_foreground(pgid: usize) -> bool {
    let seat_id = current_seat_id();
    let mut seats = SEATS.lock();
    let Some(seat) = seats.seat_mut(seat_id) else {
        return true;
    };
    seat.fg_pgrp == 0 || seat.fg_pgrp == pgid
}

pub fn set_session(sid: usize) -> Result<()> {
    let seat_id = current_seat_id();
    let mut seats = SEATS.lock();
    let seat = seats.seat_mut_or_err(seat_id)?;
    if seat.session != 0 && seat.session != sid {
        return Err(PermissionDenied);
    }
    if seat.session == 0 {
        seat.session = sid;
    }
    Ok(())
}

pub fn set_fg_pgrp(sid: usize, pgid: usize) -> Result<()> {
    let seat_id = current_seat_id();
    let mut seats = SEATS.lock();
    let seat = seats.seat_mut_or_err(seat_id)?;
    if seat.session != 0 && seat.session != sid {
        return Err(PermissionDenied);
    }
    if seat.session == 0 {
        seat.session = sid;
    }
    seat.fg_pgrp = pgid;
    Ok(())
}

// send one character to the uart + framebuffer.
// called by printf, and to echo input characters,
// but not from write().
//
pub fn putc(c: u8) {
    emit_output(0, c);
}

pub fn seat_intr(seat_id: usize, c: u8) {
    let mut seats = SEATS.lock();
    seats.intr_locked(seat_id, c);
}

pub fn seat_create() -> Result<usize> {
    let mut seats = SEATS.lock();
    for (idx, seat) in seats.seats.iter_mut().enumerate().skip(1) {
        if !seat.in_use {
            seat.in_use = true;
            seat.reset();
            return Ok(idx);
        }
    }
    Err(NoBufferSpace)
}

pub fn seat_destroy(seat_id: usize) -> Result<()> {
    if seat_id == 0 {
        return Err(PermissionDenied);
    }
    let mut seats = SEATS.lock();
    let seat = seats.seat_mut_or_err(seat_id)?;
    seat.reset();
    seat.in_use = false;
    Ok(())
}

pub fn seat_bind(seat_id: usize) -> Result<()> {
    let mut seats = SEATS.lock();
    if seats.seat_mut(seat_id).is_none() {
        return Err(InvalidArgument);
    }
    let p = Cpus::myproc().unwrap();
    p.inner.lock().seat_id = seat_id;
    Ok(())
}

pub fn seat_read_output(seat_id: usize, dst: &mut [u8]) -> Result<usize> {
    let mut seats = SEATS.lock();
    let seat = seats.seat_mut_or_err(seat_id)?;
    let mut n = 0usize;
    while n < dst.len() {
        let Some(b) = seat.output_pop() else { break };
        dst[n] = b;
        n += 1;
    }
    Ok(n)
}

pub fn seat_write_input(seat_id: usize, src: &[u8]) -> Result<usize> {
    let mut seats = SEATS.lock();
    if seats.seat_mut(seat_id).is_none() {
        return Err(InvalidArgument);
    }
    for &b in src {
        seats.intr_locked(seat_id, b);
    }
    Ok(src.len())
}

fn emit_output(seat_id: usize, c: u8) {
    let mut seats = SEATS.lock();
    if let Some(seat) = seats.seat_mut(seat_id) {
        output_push(seat, seat_id, c);
    }
}

fn output_push(seat: &mut SeatConsole, seat_id: usize, c: u8) {
    seat.output_push(c);
    if seat_id == 0 {
        emit_hw(c);
    }
}

fn emit_hw(c: u8) {
    if c == ctrl(b'H') {
        uart::putc_sync(BS);
        uart::putc_sync(b' ');
        uart::putc_sync(BS);
        framebuffer::putc(BS);
        framebuffer::putc(b' ');
        framebuffer::putc(BS);
    } else {
        uart::putc_sync(c);
        framebuffer::putc(c);
    }
}

fn current_seat_id() -> usize {
    Cpus::myproc().map(|p| p.inner.lock().seat_id).unwrap_or(0)
}
