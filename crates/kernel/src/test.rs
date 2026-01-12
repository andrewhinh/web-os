use core::ptr::write_volatile;

use crate::memlayout::SIFIVE_TEST;
use crate::printf::_print;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x5555, // FINISHER_PASS
    Failed = 0x3333,  // FINISHER_FAIL
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    unsafe {
        write_volatile(SIFIVE_TEST as *mut u32, exit_code as u32);
    }
}

pub trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        _print(format_args!("{}...\t", core::any::type_name::<T>()));
        self();
        _print(format_args!("[ok]\n"));
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    _print(format_args!("Running {} tests\n", tests.len()));
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &core::panic::PanicInfo) -> ! {
    _print(format_args!("[failed]\n"));
    _print(format_args!("Error: {}\n", info));
    exit_qemu(QemuExitCode::Failed);
    #[allow(clippy::empty_loop)]
    loop {}
}
