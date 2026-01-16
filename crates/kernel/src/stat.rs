#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileType {
    #[default]
    Empty = 0,
    Dir = 1,
    File = 2,
    Device = 3,
    Symlink = 4,
}

#[derive(Default, Debug, Clone, Copy)]
#[repr(C)]
pub struct Stat {
    pub dev: u32,        // File system's disk device
    pub ino: u32,        // Inode number
    pub ftype: FileType, // Type of file
    pub nlink: u16,      // Number of links to file
    pub size: usize,     // Size of file in bytes
    pub atime: u64,      // Ticks since boot
    pub mtime: u64,      // Ticks since boot
    pub ctime: u64,      // Ticks since boot
}

impl Stat {
    pub fn file_type(&self) -> FileType {
        self.ftype
    }
}
