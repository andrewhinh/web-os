use core::arch::asm;

pub use kernel::signal::{
    NSIG, SIG_DFL, SIG_IGN, SIGALRM, SIGCONT, SIGINT, SIGKILL, SIGTERM, SIGTSTP, SIGTTIN, SIGTTOU,
    SIGUSR1, SIGUSR2, WCONTINUED, WNOHANG, WUNTRACED,
};
use kernel::syscall::SysCalls;

use crate::sys;

extern "C" fn sigrestorer() -> ! {
    unsafe {
        asm!(
            "li a7, {sysno}",
            "ecall",
            "j .",
            sysno = const SysCalls::Sigreturn as usize,
            options(noreturn),
        );
    }
}

pub fn signal(signum: usize, handler: usize) -> sys::Result<usize> {
    let prev = sys::sigaction(signum, handler, sigrestorer as *const () as usize)?;
    let prev = match prev {
        0 => SIG_DFL,
        1 => SIG_IGN,
        _ => prev,
    };
    Ok(prev)
}

pub fn setitimer(initial: usize, interval: usize) -> sys::Result<usize> {
    sys::setitimer(initial, interval)
}
