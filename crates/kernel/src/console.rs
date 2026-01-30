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

pub static CONS: Mutex<Cons> = Mutex::new(Cons::new(), "cons");

const BS: u8 = 0x08;

// Control-x
const fn ctrl(x: u8) -> u8 {
    x - b'@'
}

const INPUT_BUF_SIZE: usize = 128;
pub struct Cons {
    buf: [u8; INPUT_BUF_SIZE],
    r: Wrapping<usize>, // Read index
    w: Wrapping<usize>, // Write index
    e: Wrapping<usize>, // Edit index
    session: usize,
    fg_pgrp: usize,
}

impl Cons {
    const fn new() -> Cons {
        Cons {
            buf: [0; INPUT_BUF_SIZE],
            r: Wrapping(0),
            w: Wrapping(0),
            e: Wrapping(0),
            session: 0,
            fg_pgrp: 0,
        }
    }
}

impl Device for Mutex<Cons> {
    // user read()s from the console go here.
    // copy (up to) a whole input line to dst.
    //
    fn read(&self, mut dst: VirtAddr, mut n: usize, _offset: usize) -> Result<usize> {
        let mut cons_guard = self.lock();
        let p = Cpus::myproc().unwrap();

        let target = n;
        while n > 0 {
            // wait until interrupt handler has put some
            // input into CONS.buf
            while cons_guard.r == cons_guard.w {
                if p.inner.lock().killed {
                    return Err(Interrupted);
                }
                cons_guard = sleep(&cons_guard.r as *const _ as usize, cons_guard);
            }
            let c = cons_guard.buf[cons_guard.r.0 % INPUT_BUF_SIZE];
            cons_guard.r += Wrapping(1);

            if c == ctrl(b'D') {
                // end of line
                if n < target {
                    // Save ^D for next time, to make sure
                    // caller gets a 0-bytes result.
                    cons_guard.r -= Wrapping(1);
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

        let mut buf = [0u8; 512];
        let mut written = 0usize;
        let mut src = src;
        while written < n {
            let m = core::cmp::min(buf.len(), n - written);
            either_copyin(&mut buf[..m], src)?;

            for &b in &buf[..m] {
                uart::UART.putc(b);
            }
            framebuffer::write(&buf[..m]);

            written += m;
            src += m;
        }
        Ok(written)
    }

    fn major(&self) -> Major {
        Major::Console
    }
}

impl Mutex<Cons> {
    // the console input interrupt handler.
    // CONS.intr() calls this for input character.
    // do erase/kill processing, append to cons.buf,
    // wake up CONS.read() if a whole line has arrived.
    //
    pub fn intr(&self, c: u8) {
        let mut cons_guard = self.lock();
        match c {
            m if m == ctrl(b'C') => {
                let target = if cons_guard.fg_pgrp == 0 {
                    Cpus::myproc().map(|p| p.inner.lock().pgid).unwrap_or(0)
                } else {
                    cons_guard.fg_pgrp
                };
                if target != 0 {
                    let _ = kill_pgrp(target, SIGINT);
                }
            }
            m if m == ctrl(b'Z') => {
                let target = if cons_guard.fg_pgrp == 0 {
                    Cpus::myproc().map(|p| p.inner.lock().pgid).unwrap_or(0)
                } else {
                    cons_guard.fg_pgrp
                };
                if target != 0 {
                    let _ = kill_pgrp(target, SIGTSTP);
                }
            }
            // Print process list
            m if m == ctrl(b'P') => dump(),
            // Kill line
            m if m == ctrl(b'U') => {
                while cons_guard.e != cons_guard.w
                    && cons_guard.buf[(cons_guard.e - Wrapping(1)).0 % INPUT_BUF_SIZE] != b'\n'
                {
                    cons_guard.e -= Wrapping(1);
                    putc(ctrl(b'H'));
                }
            }
            // Backspace
            m if m == ctrl(b'H') | b'\x7f' => {
                if cons_guard.e != cons_guard.w {
                    cons_guard.e -= Wrapping(1);
                    putc(ctrl(b'H'));
                }
            }
            _ => {
                if c != 0 && (cons_guard.e - cons_guard.r).0 < INPUT_BUF_SIZE {
                    let c = if c == b'\r' { b'\n' } else { c };

                    // echo back to the user
                    putc(c);

                    // store for consumption by CONS.read().
                    let e_idx = cons_guard.e.0 % INPUT_BUF_SIZE;
                    cons_guard.buf[e_idx] = c;
                    cons_guard.e += Wrapping(1);

                    if c == b'\n'
                        || c == ctrl(b'D')
                        || (cons_guard.e - cons_guard.r).0 == INPUT_BUF_SIZE
                    {
                        // wake up CONS.read() if a whole line (or end of line)
                        // has arrived
                        cons_guard.w = cons_guard.e;
                        wakeup(&cons_guard.r as *const _ as usize);
                    }
                }
            }
        }
    }
}

pub fn init() {
    unsafe { uart::init() }
    DEVSW.set(Major::Console, &CONS).unwrap();
}

pub fn readable() -> bool {
    let guard = CONS.lock();
    guard.r != guard.w
}

pub fn session() -> usize {
    let guard = CONS.lock();
    guard.session
}

pub fn fg_pgrp() -> usize {
    let guard = CONS.lock();
    guard.fg_pgrp
}

pub fn is_foreground(pgid: usize) -> bool {
    let guard = CONS.lock();
    guard.fg_pgrp == 0 || guard.fg_pgrp == pgid
}

pub fn set_session(sid: usize) -> Result<()> {
    let mut guard = CONS.lock();
    if guard.session != 0 && guard.session != sid {
        return Err(PermissionDenied);
    }
    if guard.session == 0 {
        guard.session = sid;
    }
    Ok(())
}

pub fn set_fg_pgrp(sid: usize, pgid: usize) -> Result<()> {
    let mut guard = CONS.lock();
    if guard.session != 0 && guard.session != sid {
        return Err(PermissionDenied);
    }
    if guard.session == 0 {
        guard.session = sid;
    }
    guard.fg_pgrp = pgid;
    Ok(())
}

// send one character to the uart + framebuffer.
// called by printf, and to echo input characters,
// but not from write().
//
pub fn putc_char(c: char) {
    let mut buf = [0u8; 4];
    let encoded = c.encode_utf8(&mut buf);
    for &b in encoded.as_bytes() {
        putc(b);
    }
}

pub fn putc(c: u8) {
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
