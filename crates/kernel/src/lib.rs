#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(all(target_os = "none", test), no_main)]
#![cfg_attr(
    all(target_os = "none", feature = "kernel"),
    feature(alloc_error_handler)
)]
#![cfg_attr(all(target_os = "none", feature = "kernel"), feature(allocator_api))]
#![feature(negative_impls)]
#![feature(fn_align)]
#![feature(variant_count)]
#![cfg_attr(all(target_os = "none", test), feature(custom_test_frameworks))]
#![cfg_attr(all(target_os = "none", test), test_runner(crate::test::test_runner))]
#![cfg_attr(
    all(target_os = "none", test),
    reexport_test_harness_main = "test_main"
)]
#![allow(clippy::missing_safety_doc)]

#[cfg(all(target_os = "none", feature = "kernel"))]
extern crate alloc;

#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod condvar;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod console;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod entry;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod kernelvec;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod memlayout;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod mpmc;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod net;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod null;
pub mod param;
pub mod poll;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod proc;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod semaphore;
pub mod signal;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod sleeplock;
pub mod socket;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod spinlock;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod start;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod uart;
#[cfg(all(target_os = "none", feature = "kernel"))]
#[macro_use]
pub mod printf;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod aplic;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod bio;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod buddy;
pub mod defs;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod elf;
pub mod error;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod exec;
#[cfg(target_os = "none")]
pub mod fcntl;
pub mod file;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod framebuffer;
pub mod fs;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod imsic;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod ipc;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod kalloc;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod list;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod log;
pub mod mmap;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod pipe;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod riscv;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod runq;
pub mod stat;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod swtch;
#[cfg(target_os = "none")]
pub mod sync;
pub mod syscall;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod task;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod test;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod trampoline;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod trap;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod virtio_disk;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod virtio_gpu;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod virtio_input;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod virtio_net;
#[cfg(all(target_os = "none", feature = "kernel"))]
pub mod vm;

#[macro_export]
macro_rules! kmain {
    ($path:path) => {
        #[unsafe(export_name = "main")]
        pub extern "C" fn __main() -> ! {
            // type check the given path
            let f: extern "C" fn() -> ! = $path;

            f()
        }
    };
}

#[cfg(all(target_os = "none", test))]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(target_os = "none", test))]
static TEST_STARTED: AtomicBool = AtomicBool::new(false);

#[cfg(all(target_os = "none", test))]
#[unsafe(export_name = "main")]
pub extern "C" fn test_main_entry() -> ! {
    let cpuid = unsafe { proc::Cpus::cpu_id() };

    if cpuid == 0 {
        console::init();
        println!("");
        println!("Running kernel tests...");
        println!("");
        null::init();
        kalloc::init();
        vm::kinit();
        vm::kinithart();
        proc::init();
        trap::inithart();
        aplic::init();
        bio::init();
        virtio_disk::init();
        virtio_net::init();
        net::init();

        TEST_STARTED.store(true, Ordering::SeqCst);

        test_main();
    } else {
        while !TEST_STARTED.load(Ordering::SeqCst) {
            core::hint::spin_loop();
        }
        vm::kinithart();
        trap::inithart();
    }

    #[allow(clippy::empty_loop)]
    loop {}
}

#[cfg(all(target_os = "none", test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    test::test_panic_handler(info)
}
