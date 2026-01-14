#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use kernel::{
    aplic, bio, console, kalloc, kmain, null,
    param::NCPU,
    println,
    proc::{self, Cpus, scheduler, user_init},
    task, trap, uart, virtio_disk, vm,
};

static STARTED: AtomicBool = AtomicBool::new(false);

kmain!(main);

extern "C" fn main() -> ! {
    let cpuid = unsafe { Cpus::cpu_id() };
    if cpuid == 0 {
        #[cfg(target_os = "none")]
        let initcode: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bin/_initcode"));
        #[cfg(not(target_os = "none"))]
        let initcode: &'static [u8] = &[];
        console::init(); // console init
        println!("kernel is booting");
        null::init(); // null device init
        kalloc::init(); // physical memory allocator
        vm::kinit(); // create kernel page table
        vm::kinithart(); // turn on paging
        proc::init(); // process table
        trap::inithart(); // install kernel trap vector
        aplic::init(); // set up interrupt controller (APLIC -> IMSIC MSIs)
        bio::init(); // buffer cache
        virtio_disk::init(); // emulated hard disk
        for cpu in 0..NCPU {
            task::init_cpu(cpu);
        }
        uart::spawn_tasks();
        virtio_disk::spawn_tasks();
        user_init(initcode);
        STARTED.store(true, Ordering::SeqCst);
    } else {
        while !STARTED.load(Ordering::SeqCst) {
            core::hint::spin_loop()
        }
        vm::kinithart(); // turn on paging
        trap::inithart(); // install kernel trap vector
    }
    scheduler()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    use kernel::printf::panic_inner;
    panic_inner(info)
}
