use crate::defs::AsBytes;

pub const IN: usize = 0x001;
pub const OUT: usize = 0x002;
pub const ERR: usize = 0x004;
pub const HUP: usize = 0x008;
pub const NVAL: usize = 0x010;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PollFd {
    pub fd: usize,
    pub events: usize,
    pub revents: usize,
}

unsafe impl AsBytes for PollFd {}
