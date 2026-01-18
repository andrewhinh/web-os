use core::arch::asm;
use core::ptr;

use crate::memlayout::{IMSIC_S, IMSIC_STRIDE, UART0_IRQ, VIRTIO0_IRQ, VIRTIO1_IRQ};

// IMSIC CSRs
const SISELECT: usize = 0x150;
const SIREG: usize = 0x151;
const STOPEI: usize = 0x15C;

// SISELECT values (register selector)
const EIDELIVERY: usize = 0x70;
const EITHRESHOLD: usize = 0x72;
const EIE0: usize = 0xC0;

#[inline]
fn siselect_write(sel: usize) {
    unsafe {
        asm!("csrw {csr}, {val}", csr = const SISELECT, val = in(reg) sel);
    }
}

#[inline]
fn sireg_read() -> usize {
    let v: usize;
    unsafe {
        asm!("csrr {out}, {csr}", out = out(reg) v, csr = const SIREG);
    }
    v
}

#[inline]
fn sireg_write(v: usize) {
    unsafe {
        asm!("csrw {csr}, {val}", csr = const SIREG, val = in(reg) v);
    }
}

#[inline]
fn enable_msg(msg: usize) {
    assert!(msg > 0 && msg < 2048);

    // EIE registers are 64-bit but occupy two CSR numbers,
    // only even-numbered EIE* selectors are valid.
    let sel = EIE0 + 2 * (msg / 64);
    let bit = msg % 64;

    siselect_write(sel);
    let cur = sireg_read();
    sireg_write(cur | (1usize << bit));
}

// Per-hart init
pub fn init_hart() {
    // Enable interrupt delivery.
    siselect_write(EIDELIVERY);
    sireg_write(1);

    // Threshold 0 => hear all priorities/messages.
    siselect_write(EITHRESHOLD);
    sireg_write(0);

    enable_msg(UART0_IRQ as usize);
    enable_msg(VIRTIO0_IRQ as usize);
    enable_msg(VIRTIO1_IRQ as usize);
}

// Pop the top pending external interrupt message for S-mode.
#[inline]
pub fn pop() -> u32 {
    let v: u32;
    unsafe {
        // csrrw atomically reads STOPEI and writes zero back, claiming the message.
        asm!("csrrw {out}, {csr}, zero", out = out(reg) v, csr = const STOPEI);
    }
    v >> 16
}

pub fn send_test(hart: usize, msg: u32) {
    let addr = (IMSIC_S + hart * IMSIC_STRIDE) as *mut u32;
    unsafe {
        ptr::write_volatile(addr, msg);
    }
}
