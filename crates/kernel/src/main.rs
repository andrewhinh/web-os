#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use kernel::{
    aplic, bio, console, framebuffer, kalloc, kmain, net, null,
    param::NCPU,
    println,
    proc::{self, Cpus, scheduler, user_init},
    task, trap, uart, virtio_disk, virtio_input, virtio_net, vm,
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
        console::init();
        println!("kernel is booting");
        null::init();
        kalloc::init();
        vm::kinit();
        vm::kinithart();
        proc::init();
        trap::inithart();
        aplic::init();
        bio::init();
        virtio_disk::init();
        virtio_input::init();
        framebuffer::init();
        virtio_net::init();
        net::init();
        for cpu in 0..NCPU {
            task::init_cpu(cpu);
        }
        uart::spawn_tasks();
        virtio_disk::spawn_tasks();
        virtio_net::spawn_tasks();
        user_init(initcode);
        STARTED.store(true, Ordering::SeqCst);
    } else {
        while !STARTED.load(Ordering::SeqCst) {
            core::hint::spin_loop()
        }
        vm::kinithart();
        trap::inithart();
    }
    scheduler()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    use kernel::printf::panic_inner;
    panic_inner(info)
}
