#[cfg(all(target_os = "none", feature = "kernel"))]
use alloc::vec::Vec;

use crate::defs::AsBytes;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::error::{Error::InvalidArgument, Error::ResourceBusy, Result};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::spinlock::Mutex;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::sync::LazyLock;

pub mod omode {
    pub const RDONLY: usize = 0x000;
    pub const WRONLY: usize = 0x001;
    pub const RDWR: usize = 0x002;
    pub const CREATE: usize = 0x200;
    pub const TRUNC: usize = 0x400;
    pub const APPEND: usize = 0x800;
    pub const CLOEXEC: usize = 0x1000;
    pub const NONBLOCK: usize = 0x2000;
}

pub mod fd {
    pub const CLOEXEC: usize = 0x1;
}

pub mod flock {
    pub const UNLCK: usize = 0;
    pub const RDLCK: usize = 1;
    pub const WRLCK: usize = 2;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Flock {
    pub l_type: usize,
    pub l_whence: usize,
    pub l_start: usize,
    pub l_len: usize,
    pub l_pid: usize,
}

unsafe impl AsBytes for Flock {}

pub struct OMode {
    read: bool,
    write: bool,
    truncate: bool,
    create: bool,
    append: bool,
    cloexec: bool,
    nonblock: bool,
}

impl Default for OMode {
    fn default() -> Self {
        Self::new()
    }
}

impl OMode {
    pub fn new() -> Self {
        Self {
            read: false,
            write: false,
            truncate: false,
            create: false,
            append: false,
            cloexec: false,
            nonblock: false,
        }
    }

    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    pub fn cloexec(&mut self, cloexec: bool) -> &mut Self {
        self.cloexec = cloexec;
        self
    }

    pub fn nonblock(&mut self, nonblock: bool) -> &mut Self {
        self.nonblock = nonblock;
        self
    }

    fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    pub fn from_usize(bits: usize) -> Self {
        let mut mode = Self::new();
        mode.read(bits & omode::WRONLY == 0)
            .write(bits & omode::WRONLY != 0 || bits & omode::RDWR != 0)
            .create(bits & omode::CREATE != 0)
            .truncate(bits & omode::TRUNC != 0)
            .append(bits & omode::APPEND != 0)
            .cloexec(bits & omode::CLOEXEC != 0)
            .nonblock(bits & omode::NONBLOCK != 0);
        mode
    }

    pub fn is_read(&self) -> bool {
        self.read
    }

    pub fn is_write(&self) -> bool {
        self.write
    }

    pub fn is_create(&self) -> bool {
        self.create
    }

    pub fn is_trunc(&self) -> bool {
        self.truncate
    }

    pub fn is_rdonly(&self) -> bool {
        self.read && !self.write
    }

    pub fn is_cloexec(&self) -> bool {
        self.cloexec
    }

    pub fn is_append(&self) -> bool {
        self.append
    }

    pub fn is_nonblock(&self) -> bool {
        self.nonblock
    }
}

#[repr(usize)]
pub enum FcntlCmd {
    GetFl = 1,
    SetFl = 2,
    GetFd = 3,
    SetFd = 4,
    SetCloexec = 5,
    SetNonblock = 6,
    ClearNonblock = 7,
    GetLk = 8,
    SetLk = 9,
    Invalid,
}

impl FcntlCmd {
    pub fn from_usize(bits: usize) -> Self {
        match bits {
            1 => Self::GetFl,
            2 => Self::SetFl,
            3 => Self::GetFd,
            4 => Self::SetFd,
            5 => Self::SetCloexec,
            6 => Self::SetNonblock,
            7 => Self::ClearNonblock,
            8 => Self::GetLk,
            9 => Self::SetLk,
            _ => Self::Invalid,
        }
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockKind {
    Read,
    Write,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Clone, Copy, Debug)]
struct LockEntry {
    dev: u32,
    inum: u32,
    kind: LockKind,
    pid: usize,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug, Default)]
struct LockTable {
    entries: Vec<LockEntry>,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl LockTable {
    fn conflict(&self, dev: u32, inum: u32, pid: usize, kind: LockKind) -> Option<LockEntry> {
        for entry in &self.entries {
            if entry.dev != dev || entry.inum != inum || entry.pid == pid {
                continue;
            }
            match (kind, entry.kind) {
                (LockKind::Read, LockKind::Write) | (LockKind::Write, _) => return Some(*entry),
                _ => {}
            }
        }
        None
    }

    fn clear_pid(&mut self, dev: u32, inum: u32, pid: usize) {
        self.entries
            .retain(|entry| !(entry.dev == dev && entry.inum == inum && entry.pid == pid));
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
static FILE_LOCKS: LazyLock<Mutex<LockTable>> =
    LazyLock::new(|| Mutex::new(LockTable::default(), "file_locks"));

#[cfg(all(target_os = "none", feature = "kernel"))]
fn lock_kind(lock_type: usize) -> Result<Option<LockKind>> {
    match lock_type {
        flock::UNLCK => Ok(None),
        flock::RDLCK => Ok(Some(LockKind::Read)),
        flock::WRLCK => Ok(Some(LockKind::Write)),
        _ => Err(InvalidArgument),
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn validate_flock(lock: &Flock) -> Result<()> {
    if lock.l_whence != 0 || lock.l_start != 0 || lock.l_len != 0 {
        return Err(InvalidArgument);
    }
    let _ = lock_kind(lock.l_type)?;
    Ok(())
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub fn set_lock(dev: u32, inum: u32, pid: usize, lock: &Flock) -> Result<()> {
    validate_flock(lock)?;
    let kind = lock_kind(lock.l_type)?;
    let mut guard = FILE_LOCKS.lock();
    guard.clear_pid(dev, inum, pid);
    let Some(kind) = kind else {
        return Ok(());
    };
    if guard.conflict(dev, inum, pid, kind).is_some() {
        return Err(ResourceBusy);
    }
    guard.entries.push(LockEntry {
        dev,
        inum,
        kind,
        pid,
    });
    Ok(())
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub fn get_lock(dev: u32, inum: u32, pid: usize, lock: &mut Flock) -> Result<()> {
    validate_flock(lock)?;
    let Some(kind) = lock_kind(lock.l_type)? else {
        lock.l_type = flock::UNLCK;
        lock.l_pid = 0;
        return Ok(());
    };
    let guard = FILE_LOCKS.lock();
    if let Some(entry) = guard.conflict(dev, inum, pid, kind) {
        lock.l_type = match entry.kind {
            LockKind::Read => flock::RDLCK,
            LockKind::Write => flock::WRLCK,
        };
        lock.l_pid = entry.pid;
    } else {
        lock.l_type = flock::UNLCK;
        lock.l_pid = 0;
    }
    Ok(())
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub fn clear_locks(dev: u32, inum: u32, pid: usize) {
    let mut guard = FILE_LOCKS.lock();
    guard.clear_pid(dev, inum, pid);
}
