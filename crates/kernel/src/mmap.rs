// mmap constants shared with userland

pub const PROT_READ: usize = 0x1;
pub const PROT_WRITE: usize = 0x2;
pub const PROT_EXEC: usize = 0x4;

pub const MAP_SHARED: usize = 0x1;
pub const MAP_PRIVATE: usize = 0x2;
pub const MAP_ANON: usize = 0x4;
