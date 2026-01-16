#[cfg(all(target_os = "none", feature = "kernel"))]
use alloc::sync::Arc;
#[cfg(all(target_os = "none", feature = "kernel"))]
use core::cell::UnsafeCell;
#[cfg(all(target_os = "none", feature = "kernel"))]
use core::ops::Deref;

#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::array;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::console;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::error::{Error::*, Result};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::fcntl::{self, FcntlCmd, Flock, OMode, fd, flock, omode};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::fs::{BSIZE, IData, Inode, Path, create};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::log::LOG;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::param::{MAXOPBLOCKS, NDEV, NFILE};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::pipe::Pipe;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::poll;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::proc::{Cpus, either_copyin, either_copyout};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::sleeplock::{SleepLock, SleepLockGuard};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::spinlock::Mutex;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::stat::{FileType, Stat};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::sync::{LazyLock, OnceLock};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::vm::VirtAddr;

#[cfg(all(target_os = "none", feature = "kernel"))]
pub static DEVSW: DevSW = DevSW::new();
#[cfg(all(target_os = "none", feature = "kernel"))]
pub static FTABLE: LazyLock<FTable> = LazyLock::new(|| Mutex::new(array![None; NFILE], "ftable"));

#[cfg(all(target_os = "none", feature = "kernel"))]
type FTable = Mutex<[Option<Arc<VFile>>; NFILE]>;

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Default, Clone, Debug)]
pub struct File {
    f: Option<Arc<VFile>>,
    readable: bool,
    writable: bool,
    cloexec: bool,
    nonblock: bool,
    append: bool,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub enum VFile {
    Device(DNod),
    Inode(FNod),
    Pipe(Pipe),
    None,
}

// Device Node
#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub struct DNod {
    driver: &'static dyn Device,
    off: UnsafeCell<usize>, // Safety: offset uses per-open file state.
    ip: Inode,
}

// Device functions, map this trait using dyn
#[cfg(all(target_os = "none", feature = "kernel"))]
pub trait Device: Send + Sync {
    fn read(&self, dst: VirtAddr, n: usize, offset: usize) -> Result<usize>;
    fn write(&self, src: VirtAddr, n: usize, offset: usize) -> Result<usize>;
    fn major(&self) -> Major;
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl core::fmt::Debug for dyn Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Device fn {:?}", self.major())
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Deref for DNod {
    type Target = dyn Device;

    fn deref(&self) -> &Self::Target {
        self.driver
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
unsafe impl Send for DNod {}
#[cfg(all(target_os = "none", feature = "kernel"))]
unsafe impl Sync for DNod {}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl DNod {
    pub fn new(driver: &'static dyn Device, ip: Inode) -> Self {
        Self {
            driver,
            off: UnsafeCell::new(0),
            ip,
        }
    }

    fn read(&self, dst: VirtAddr, n: usize) -> Result<usize> {
        let off = unsafe { &mut *self.off.get() };
        let r = self.driver.read(dst, n, *off)?;
        *off += r;
        Ok(r)
    }

    fn write(&self, src: VirtAddr, n: usize) -> Result<usize> {
        let off = unsafe { &mut *self.off.get() };
        let r = self.driver.write(src, n, *off)?;
        *off += r;
        Ok(r)
    }
}

// File & directory Node
#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub struct FNod {
    off: UnsafeCell<u32>, // Safety: If inode lock is obtained.
    ip: Inode,
}
#[cfg(all(target_os = "none", feature = "kernel"))]
unsafe impl Send for FNod {}
#[cfg(all(target_os = "none", feature = "kernel"))]
unsafe impl Sync for FNod {}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl FNod {
    pub fn new(ip: Inode, offset: u32) -> Self {
        Self {
            off: UnsafeCell::new(offset),
            ip,
        }
    }

    fn read(&self, dst: VirtAddr, n: usize) -> Result<usize> {
        LOG.begin_op();
        let res = {
            let mut ip = self.ip.lock();
            let off = unsafe { &mut *self.off.get() };

            let r = ip.read(dst, *off, n)?;
            if r > 0 {
                ip.touch_atime();
            }
            *off += r as u32;
            Ok(r)
        };
        LOG.end_op();
        res
    }

    fn write(&self, src: VirtAddr, n: usize, append: bool) -> Result<usize> {
        // write a few blocks at a time to avoid exceeding the maximum
        // log transaction size, including i-node, indirect block,
        // allocation blocks, and 2 blocks of slop for non-aligned
        // writes. this really belongs lower down, since inode write()
        // might be writing a device like the console.
        let max = ((MAXOPBLOCKS - 1 - 1 - 2) / 2) * BSIZE;
        let mut ret: Result<usize> = Ok(0);
        let mut i: usize = 0;

        while i < n {
            let mut n1 = n - i;
            if n1 > max {
                n1 = max
            }

            LOG.begin_op();
            {
                let mut guard = self.ip.lock();
                let off = unsafe { &mut *self.off.get() };
                if append {
                    *off = guard.size();
                }
                ret = guard.write(src, *off, n1);
                match ret {
                    Ok(r) => {
                        guard.touch_mtime_ctime();
                        *off += r as u32;
                        i += r;
                        ret = Ok(i);
                    }
                    _ => break, // error from inode write
                }
            }
            LOG.end_op();
        }
        if ret.is_err() {
            LOG.end_op();
        }
        ret
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl VFile {
    fn read(&self, dst: VirtAddr, n: usize) -> Result<usize> {
        match self {
            VFile::Device(d) => d.read(dst, n),
            VFile::Inode(f) => f.read(dst, n),
            VFile::Pipe(p) => p.read(dst, n),
            _ => panic!("file read"),
        }
    }

    fn write(&self, src: VirtAddr, n: usize, append: bool) -> Result<usize> {
        match self {
            VFile::Device(d) => d.write(src, n),
            VFile::Inode(f) => f.write(src, n, append),
            VFile::Pipe(p) => p.write(src, n),
            _ => panic!("file write"),
        }
    }

    // Get metadata about file.
    // addr pointing to a struct stat.
    pub fn stat(&self, addr: VirtAddr) -> Result<()> {
        let mut stat: Stat = Default::default();

        match self {
            VFile::Device(DNod { driver: _, ip, .. }) | VFile::Inode(FNod { off: _, ip }) => {
                {
                    ip.lock().stat(&mut stat);
                }
                either_copyout(addr, &stat)
            }
            _ => Err(BadFileDescriptor),
        }
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl File {
    // Read from file.
    pub fn read(&mut self, dst: VirtAddr, n: usize) -> Result<usize> {
        if !self.readable {
            return Err(InvalidArgument);
        }
        match self.f.as_ref().unwrap().as_ref() {
            VFile::Pipe(p) if self.nonblock => p.read_nonblock(dst, n),
            VFile::Device(d) if self.nonblock && d.major() == Major::Console => {
                if !console::readable() {
                    return Err(WouldBlock);
                }
                d.read(dst, n)
            }
            _ => self.f.as_ref().unwrap().read(dst, n),
        }
    }

    // Write to file.
    pub fn write(&mut self, src: VirtAddr, n: usize) -> Result<usize> {
        if !self.writable {
            return Err(InvalidArgument);
        }
        match self.f.as_ref().unwrap().as_ref() {
            VFile::Pipe(p) if self.nonblock => p.write_nonblock(src, n),
            _ => self.f.as_ref().unwrap().write(src, n, self.append),
        }
    }

    pub fn sync(&self) -> Result<()> {
        match self.f.as_ref().unwrap().as_ref() {
            VFile::Inode(_) | VFile::Device(_) => {
                LOG.sync();
                Ok(())
            }
            VFile::Pipe(_) => Err(InvalidArgument),
            VFile::None => Err(BadFileDescriptor),
        }
    }

    pub fn poll(&self, events: usize) -> usize {
        let mut revents = 0;
        match self.f.as_ref().unwrap().as_ref() {
            VFile::Pipe(p) => {
                if self.readable && events & poll::IN != 0 && p.poll_readable().unwrap_or(false) {
                    revents |= poll::IN;
                }
                if self.writable && events & poll::OUT != 0 && p.poll_writable().unwrap_or(false) {
                    revents |= poll::OUT;
                }
                if self.readable && p.poll_read_hup().unwrap_or(false) {
                    revents |= poll::HUP;
                }
                if self.writable && p.poll_write_hup().unwrap_or(false) {
                    revents |= poll::HUP;
                }
            }
            VFile::Device(d) => {
                if self.readable && events & poll::IN != 0 {
                    let ready = match d.major() {
                        Major::Console => console::readable(),
                        _ => true,
                    };
                    if ready {
                        revents |= poll::IN;
                    }
                }
                if self.writable && events & poll::OUT != 0 {
                    revents |= poll::OUT;
                }
            }
            VFile::Inode(_) => {
                if self.readable && events & poll::IN != 0 {
                    revents |= poll::IN;
                }
                if self.writable && events & poll::OUT != 0 {
                    revents |= poll::OUT;
                }
            }
            VFile::None => {}
        }
        revents
    }

    pub fn is_cloexec(&self) -> bool {
        self.cloexec
    }

    pub fn clear_cloexec(&mut self) {
        self.cloexec = false;
    }

    fn access_mode_bits(&self) -> usize {
        match (self.readable, self.writable) {
            (true, false) => omode::RDONLY,
            (false, true) => omode::WRONLY,
            (true, true) => omode::RDWR,
            (false, false) => omode::RDONLY,
        }
    }

    fn status_flags(&self) -> usize {
        let mut flags = self.access_mode_bits();
        if self.append {
            flags |= omode::APPEND;
        }
        if self.nonblock {
            flags |= omode::NONBLOCK;
        }
        flags
    }

    fn set_status_flags(&mut self, flags: usize) -> Result<()> {
        let allowed = omode::APPEND | omode::NONBLOCK | omode::WRONLY | omode::RDWR;
        if flags & !allowed != 0 {
            return Err(InvalidArgument);
        }
        self.append = flags & omode::APPEND != 0;
        self.nonblock = flags & omode::NONBLOCK != 0;
        Ok(())
    }

    pub fn lock_key(&self) -> Option<(u32, u32)> {
        match self.f.as_ref()?.as_ref() {
            VFile::Inode(FNod { off: _, ip }) | VFile::Device(DNod { driver: _, ip, .. }) => {
                Some((ip.dev(), ip.inum()))
            }
            _ => None,
        }
    }

    pub fn do_fcntl(&mut self, cmd: FcntlCmd, arg: usize) -> Result<usize> {
        use FcntlCmd::*;
        match cmd {
            GetFl => return Ok(self.status_flags()),
            SetFl => self.set_status_flags(arg)?,
            GetFd => return Ok(if self.cloexec { fd::CLOEXEC } else { 0 }),
            SetFd => {
                if arg & !fd::CLOEXEC != 0 {
                    return Err(InvalidArgument);
                }
                self.cloexec = arg & fd::CLOEXEC != 0;
            }
            GetLk => {
                if arg == 0 {
                    return Err(InvalidArgument);
                }
                let mut lock: Flock = Default::default();
                either_copyin(&mut lock, VirtAddr::User(arg))?;
                let (dev, inum) = self.lock_key().ok_or(InvalidArgument)?;
                let pid = Cpus::myproc().unwrap().pid();
                fcntl::get_lock(dev, inum, pid, &mut lock)?;
                either_copyout(VirtAddr::User(arg), &lock)?;
                return Ok(0);
            }
            SetLk => {
                if arg == 0 {
                    return Err(InvalidArgument);
                }
                let mut lock: Flock = Default::default();
                either_copyin(&mut lock, VirtAddr::User(arg))?;
                if lock.l_type == flock::RDLCK && !self.readable {
                    return Err(InvalidArgument);
                }
                if lock.l_type == flock::WRLCK && !self.writable {
                    return Err(InvalidArgument);
                }
                let (dev, inum) = self.lock_key().ok_or(InvalidArgument)?;
                let pid = Cpus::myproc().unwrap().pid();
                fcntl::set_lock(dev, inum, pid, &lock)?;
                return Ok(0);
            }
            SetCloexec => self.cloexec = true,
            SetNonblock => self.nonblock = true,
            ClearNonblock => self.nonblock = false,
            _ => return Err(InvalidArgument),
        }
        Ok(0)
    }

    pub fn is_readable(&self) -> bool {
        self.readable
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn inode(&self) -> Option<Inode> {
        match self.f.as_ref()?.as_ref() {
            VFile::Inode(FNod { off: _, ip }) | VFile::Device(DNod { driver: _, ip, .. }) => {
                Some(ip.clone())
            }
            _ => None,
        }
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Deref for File {
    type Target = Arc<VFile>;

    fn deref(&self) -> &Self::Target {
        self.f.as_ref().unwrap()
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Drop for File {
    fn drop(&mut self) {
        let f = self.f.take().unwrap();
        if Arc::strong_count(&f) < 2 {
            panic!("file drop");
        }

        if Arc::strong_count(&f) == 2 {
            let mut guard = FTABLE.lock();
            // drop arc<vfile> in table
            for ff in guard.iter_mut() {
                match ff {
                    Some(vff) if Arc::ptr_eq(&f, vff) => {
                        ff.take(); // drop ref in table. ref count = 1;
                    }
                    _ => (),
                }
            }
        }

        // if ref count == 1
        if let Ok(VFile::Inode(FNod { off: _, ip }) | VFile::Device(DNod { driver: _, ip, .. })) =
            Arc::try_unwrap(f)
        {
            LOG.begin_op();
            drop(ip);
            LOG.end_op();
        }
    }
}

// File Allocation Type Source
#[cfg(all(target_os = "none", feature = "kernel"))]
pub enum FType<'a> {
    Node(&'a Path),
    Pipe(Pipe),
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl FTable {
    // Allocate a file structure
    // Must be called inside transaction if FType == FType::Node.
    pub fn alloc(&self, opts: OMode, ftype: FType<'_>) -> Result<File> {
        let inner: Arc<VFile> = Arc::new(match ftype {
            FType::Node(path) => {
                let ip: Inode;
                let mut ip_guard: SleepLockGuard<'_, IData>;

                if opts.is_create() {
                    ip = create(path, FileType::File, 0, 0)?;
                    ip_guard = ip.lock();
                } else {
                    (_, ip) = path.namei()?;
                    ip_guard = ip.lock();
                    if ip_guard.itype() == FileType::Dir && !opts.is_rdonly() {
                        return Err(IsADirectory);
                    }
                }
                // ?
                match ip_guard.itype() {
                    FileType::Device if ip_guard.major() != Major::Invalid => {
                        let driver = DEVSW.get(ip_guard.major()).unwrap();
                        SleepLock::unlock(ip_guard);
                        VFile::Device(DNod::new(driver, ip))
                    }
                    FileType::Dir | FileType::File => {
                        let mut offset = 0;
                        if opts.is_trunc() && ip_guard.itype() == FileType::File {
                            ip_guard.trunc();
                        } else if opts.is_append() && ip_guard.itype() == FileType::File {
                            offset = ip_guard.size();
                        }
                        SleepLock::unlock(ip_guard);
                        VFile::Inode(FNod::new(ip, offset))
                    }
                    _ => return Err(NoSuchNode),
                }
            }
            FType::Pipe(pi) => VFile::Pipe(pi),
        });

        let mut guard = self.lock();

        let mut empty: Option<&mut Option<Arc<VFile>>> = None;
        for f in guard.iter_mut() {
            match f {
                None if empty.is_none() => {
                    empty = Some(f);
                    break;
                }
                _ => continue,
            }
        }

        let f = empty.ok_or(FileTableOverflow)?;
        f.replace(inner);
        Ok(File {
            f: f.clone(), // ref count = 2
            readable: opts.is_read(),
            writable: opts.is_write(),
            cloexec: opts.is_cloexec(),
            nonblock: opts.is_nonblock(),
            append: opts.is_append(),
        })
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub struct DevSW {
    table: [OnceLock<&'static dyn Device>; NDEV],
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl core::fmt::Debug for DevSW {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[")?;
        for (count, v) in self.table.iter().enumerate() {
            if count != 0 {
                write!(f, ", ")?;
            }
            if let Some(&v) = v.get() {
                write!(f, "{:?}", v)?;
            } else {
                write!(f, "None")?;
            }
        }
        write!(f, "]")
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Default for DevSW {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl DevSW {
    pub const fn new() -> Self {
        Self {
            table: array![OnceLock::new(); NDEV],
        }
    }

    pub fn set(
        &self,
        devnum: Major,
        dev: &'static dyn Device,
    ) -> core::result::Result<(), &'static (dyn Device + 'static)> {
        self.table[devnum as usize].set(dev)
    }

    pub fn get(&self, devnum: Major) -> Option<&'static dyn Device> {
        match self.table[devnum as usize].get() {
            Some(&dev) => Some(dev),
            None => None,
        }
    }
}

// Device Major Number
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Major {
    Null = 0,
    Console = 1,
    Disk = 2,
    #[default]
    Invalid,
}

impl Major {
    pub fn from_u16(bits: u16) -> Major {
        match bits {
            0 => Major::Null,
            1 => Major::Console,
            2 => Major::Disk,
            _ => Major::Invalid,
        }
    }
}
