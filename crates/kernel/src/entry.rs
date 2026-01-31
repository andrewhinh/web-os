use core::arch::asm;

use crate::memlayout::STACK_PAGE_NUM;
use crate::start::start;

#[unsafe(link_section = ".entry")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _entry() -> ! {
    // set up stack for Rust.
    // stack0 is declared in kernel/src/start.rs
    // with 4096 * STACK_PAGE_NUM bytes stack per CPU.
    // sp = stack0 + (hartid * 4096 * STACK_PAGE_NUM)
    unsafe {
        asm!(
            "la sp, STACK0",
            "li a0, 4096 * {ssz}",
            "csrr a1, mhartid",
            "addi a1, a1, 1",
            "mul a0, a0, a1",
            "add sp, sp, a0",
            ssz = const STACK_PAGE_NUM,
        );

        start()
    }
}
