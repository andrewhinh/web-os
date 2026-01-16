#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigDefaultAction {
    Ignore,
    Terminate,
}

pub const NSIG: usize = 32;
pub const SIG_DFL: usize = usize::MAX;
pub const SIG_IGN: usize = usize::MAX - 1;

pub const SIGINT: usize = 2;
pub const SIGKILL: usize = 9;
pub const SIGUSR1: usize = 10;
pub const SIGUSR2: usize = 12;
pub const SIGALRM: usize = 14;
pub const SIGTERM: usize = 15;

pub const WNOHANG: usize = 0x1;

#[inline]
pub fn sig_mask(sig: usize) -> u32 {
    if sig == 0 || sig > NSIG {
        0
    } else {
        1u32 << (sig - 1)
    }
}

#[inline]
pub fn default_action(sig: usize) -> SigDefaultAction {
    match sig {
        SIGKILL | SIGTERM | SIGINT | SIGALRM | SIGUSR1 | SIGUSR2 => SigDefaultAction::Terminate,
        _ => SigDefaultAction::Ignore,
    }
}
