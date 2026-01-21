#[cfg(all(target_os = "none", feature = "kernel"))]
use alloc::string::{String, ToString};
#[cfg(all(target_os = "none", feature = "kernel"))]
use alloc::vec::Vec;
use core::mem::variant_count;
#[cfg(all(target_os = "none", feature = "kernel"))]
use core::mem::{size_of, size_of_val};
#[cfg(all(target_os = "none", feature = "kernel"))]
use core::{concat, str};

#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::error::Error::*;
use crate::error::Result;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::{
    array, console,
    defs::AsBytes,
    dfs,
    exec::exec,
    fcntl::{self, FcntlCmd, OMode},
    file::{FTABLE, FType, File, RemoteFile},
    fs::{self, Path},
    ipc,
    log::{LOG, LogCrashStage, set_crash_stage},
    param::{MAXARG, MAXPATH, NOFILE},
    pipe::Pipe,
    poll,
    proc::*,
    riscv::PGSIZE,
    stat::FileType,
    task,
    trap::TICKS,
    vm::{Addr, UVAddr},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum SysCalls {
    Fork = 1,
    Exit = 2,
    Wait = 3,
    Pipe = 4,
    Read = 5,
    Kill = 6,
    Exec = 7,
    Fstat = 8,
    Chdir = 9,
    Dup = 10,
    Getpid = 11,
    Sbrk = 12,
    Sleep = 13,
    Uptime = 14,
    Open = 15,
    Write = 16,
    Mknod = 17,
    Unlink = 18,
    Link = 19,
    Mkdir = 20,
    Close = 21,
    Dup2 = 22,
    Fcntl = 23,
    Nonblock = 24,
    Freepages = 25,
    MMap = 26,
    MunMap = 27,
    Clone = 28,
    Join = 29,
    ExtIrqCount = 30,
    KTaskPolls = 31,
    Poll = 32,
    Select = 33,
    Waitpid = 34,
    Sigaction = 35,
    Sigreturn = 36,
    Setitimer = 37,
    ShmCreate = 38,
    ShmAttach = 39,
    ShmDetach = 40,
    ShmDestroy = 41,
    SemCreate = 42,
    SemWait = 43,
    SemTryWait = 44,
    SemPost = 45,
    SemClose = 46,
    Fsync = 47,
    Symlink = 48,
    Socket = 49,
    Bind = 50,
    Listen = 51,
    Accept = 52,
    Connect = 53,
    Setpgid = 54,
    Getpgrp = 55,
    Setsid = 56,
    Tcgetpgrp = 57,
    Tcsetpgrp = 58,
    LogCrash = 59,
    Getnprocs = 60,
    Getnprocsconf = 61,
    Invalid = 0,
}

#[derive(Debug, Clone, Copy)]
pub enum Fn {
    U(fn() -> Result<()>),    // return unit type
    I(fn() -> Result<usize>), // return integer
    N(fn() -> !),             // return never
}
impl Fn {
    pub fn call(self) -> isize {
        match self {
            Fn::U(uni) => uni()
                .and(Ok(0))
                .or_else(|err| Ok::<isize, ()>(err as isize))
                .unwrap(),
            Fn::I(int) => int()
                .map(|i| i as isize)
                .or_else(|err| Ok::<isize, ()>(err as isize))
                .unwrap(),
            Fn::N(nev) => nev(),
        }
    }
}
impl SysCalls {
    pub const TABLE: [(Fn, &'static str); variant_count::<Self>()] = [
        (Fn::N(Self::invalid), ""),
        (Fn::I(Self::fork), "()"), // Create a process, return child's PID.
        (Fn::N(Self::exit), "(xstatus: i32)"), /* Terminate the current process; status reported
                                    * to wait(). No Return. */
        (Fn::I(Self::wait), "(xstatus: &mut i32)"), /* Wait for a child to exit; exit status in
                                                     * &status; returns child PID. */
        (Fn::U(Self::pipe), "(p: &mut [usize])"), /* Create a pipe, put read/write file
                                                   * descriptors in p[0] and p[1]. */
        (Fn::I(Self::read), "(fd: usize, buf: &mut [u8])"), /* Read n bytes into buf; returns number read; or 0 if end of file */
        (Fn::U(Self::kill), "(pid: usize, sig: usize)"),    /* Terminate process PID. Returns
                                                             * Ok(()) or Err(()) */
        (
            Fn::I(Self::exec),
            "(filename: &str, argv: &[&str], envp: Option<&[Option<&str>]>)",
        ), // Load a file and execute it with arguments; only returns if error.
        (Fn::U(Self::fstat), "(fd: usize, st: &mut Stat)"), /* Place info about an open file
                                                             * into st. */
        (Fn::U(Self::chdir), "(dirname: &str)"), // Change the current directory.
        (Fn::I(Self::dup), "(fd: usize)"),       /* Return a new file descriptor referring to
                                                  * the same file as fd. */
        (Fn::I(Self::getpid), "()"), // Return the current process's PID.
        (Fn::I(Self::sbrk), "(n: usize)"), /* Grow process's memory by n bytes. Returns start of new
                                      * memory. */
        (Fn::U(Self::sleep), "(n: usize)"), // Pause for n clock ticks.
        (Fn::I(Self::uptime), "()"),        // Return how many clock ticks since start.
        (Fn::I(Self::open), "(filename: &str, flags: usize)"), /* Open a file; flags indicate
                                             * read/write; returns an fd. */
        (Fn::I(Self::write), "(fd: usize, b: &[u8])"), /* Write n bytes from buf to file
                                                        * descriptor fd; returns n. */
        (Fn::U(Self::mknod), "(file: &str, mj: usize, mi: usize)"), // Create a device file
        (Fn::U(Self::unlink), "(file: &str)"),                      // Remove a file
        (Fn::U(Self::link), "(file1: &str, file2: &str)"),          /* Create another name
                                                                     * (file2) for the file
                                                                     * file1. */
        (Fn::U(Self::mkdir), "(dir: &str)"), // Create a new directory.
        (Fn::U(Self::close), "(fd: usize)"), // Release open file fd.
        (Fn::I(Self::dup2), "(src: usize, dst: usize)"), //
        (Fn::I(Self::fcntl), "(fd: usize, cmd: FcntlCmd, arg: usize)"), //
        (Fn::I(Self::nonblock), "(fd: usize, on: usize)"), //
        (Fn::I(Self::freepages), "()"),      //
        (
            Fn::I(Self::mmap),
            "(addr: usize, len: usize, prot: usize, flags: usize, fd: usize, offset: usize)",
        ), //
        (Fn::U(Self::munmap), "(addr: usize, len: usize)"), //
        (
            Fn::I(Self::clone),
            "(fcn: usize, arg1: usize, arg2: usize, stack: usize)",
        ),
        (Fn::I(Self::join), "(stack: &mut usize)"),
        (Fn::I(Self::ext_irq_count), "()"),
        (Fn::I(Self::ktaskpolls), "()"),
        (
            Fn::I(Self::poll),
            "(fds: &mut [poll::PollFd], timeout: isize)",
        ),
        (
            Fn::I(Self::select),
            "(fds: &mut [poll::PollFd], timeout: isize)",
        ),
        (
            Fn::I(Self::waitpid),
            "(pid: isize, xstatus: &mut i32, options: usize)",
        ),
        (
            Fn::I(Self::sigaction),
            "(signum: usize, handler: usize, restorer: usize)",
        ),
        (Fn::U(Self::sigreturn), "()"),
        (Fn::I(Self::setitimer), "(initial: usize, interval: usize)"),
        (Fn::I(Self::shmcreate), "(size: usize)"),
        (Fn::I(Self::shmattach), "(id: usize, prot: usize)"),
        (Fn::U(Self::shmdetach), "(addr: usize)"),
        (Fn::U(Self::shmdestroy), "(id: usize)"),
        (Fn::I(Self::semcreate), "(value: usize)"),
        (Fn::U(Self::semwait), "(id: usize)"),
        (Fn::I(Self::semtrywait), "(id: usize)"),
        (Fn::U(Self::sempost), "(id: usize)"),
        (Fn::U(Self::semclose), "(id: usize)"),
        (Fn::U(Self::fsync), "(fd: usize)"),
        (Fn::U(Self::symlink), "(target: &str, linkpath: &str)"),
        (
            Fn::I(Self::socket),
            "(domain: usize, stype: usize, protocol: usize)",
        ),
        (Fn::U(Self::bind), "(fd: usize, path: &str)"),
        (Fn::U(Self::listen), "(fd: usize, backlog: usize)"),
        (Fn::I(Self::accept), "(fd: usize)"),
        (Fn::U(Self::connect), "(fd: usize, path: &str)"),
        (Fn::U(Self::setpgid), "(pid: usize, pgid: usize)"),
        (Fn::I(Self::getpgrp), "()"),
        (Fn::I(Self::setsid), "()"),
        (Fn::I(Self::tcgetpgrp), "(fd: usize)"),
        (Fn::U(Self::tcsetpgrp), "(fd: usize, pgid: usize)"),
        (Fn::U(Self::logcrash), "(stage: usize)"),
        (Fn::I(Self::getnprocs), "()"),
        (Fn::I(Self::getnprocsconf), "()"),
    ];

    pub fn invalid() -> ! {
        // syscall() should never dispatch here
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            exit(-1)
        }
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub fn syscall() {
    let p = Cpus::myproc().unwrap();
    let pdata = p.data_mut();
    let tf = pdata.trapframe.as_mut().unwrap();
    let syscall_id = SysCalls::from_usize(tf.a7);
    if syscall_id == SysCalls::Sigreturn {
        let _ = SysCalls::sigreturn();
        return;
    }
    tf.a0 = match syscall_id {
        SysCalls::Invalid => {
            println!("{} {}: unknown sys call {}", p.pid(), pdata.name, tf.a7);
            -1_isize as usize
        }
        _ => SysCalls::TABLE[syscall_id as usize].0.call() as usize,
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
enum Slice {
    Ref(UVAddr),
    Buf(SBInfo),
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
struct SBInfo {
    ptr: UVAddr,
    len: usize,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
unsafe impl AsBytes for SBInfo {}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn fetch_addr<T: AsBytes>(addr: UVAddr, buf: &mut T) -> Result<()> {
    let p_data = Cpus::myproc().unwrap().data();
    let aspace = p_data.aspace.as_ref().unwrap();
    let sz = aspace.inner.lock().sz;
    if addr.into_usize() >= sz || addr.into_usize() + size_of_val(buf) > sz {
        return Err(BadVirtAddr);
    }
    either_copyin(buf, addr.into())
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn fetch_slice<T: AsBytes>(slice_info: Slice, buf: &mut [T]) -> Result<Option<usize>> {
    let mut sbinfo: SBInfo = Default::default();
    match slice_info {
        Slice::Ref(addr) => {
            fetch_addr(addr, &mut sbinfo)?;
        }
        Slice::Buf(info) => {
            sbinfo = info;
        }
    }
    if *sbinfo.ptr.get() == 0 {
        // Option<&[&T]> = None
        // sbinfo.len = 0;
        return Ok(None);
    }
    if sbinfo.len > buf.len() {
        return Err(NoBufferSpace);
    } else {
        either_copyin(&mut buf[..sbinfo.len], sbinfo.ptr.into())?;
    }
    Ok(Some(sbinfo.len))
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn poll_check(fds: &mut [poll::PollFd]) -> Result<usize> {
    let p_data = Cpus::myproc().unwrap().data();
    let mut ready = 0;
    for fd in fds.iter_mut() {
        fd.revents = 0;
        let Some(file) = p_data.ofile.get(fd.fd).and_then(|f| f.as_ref()) else {
            fd.revents = poll::NVAL;
            ready += 1;
            continue;
        };
        let revents = file.poll(fd.events);
        fd.revents = revents;
        if revents != 0 {
            ready += 1;
        }
    }
    Ok(ready)
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn argraw(n: usize) -> usize {
    let tf = Cpus::myproc().unwrap().data().trapframe.as_ref().unwrap();
    match n {
        0 => tf.a0,
        1 => tf.a1,
        2 => tf.a2,
        3 => tf.a3,
        4 => tf.a4,
        5 => tf.a5,
        _ => panic!("arg"),
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
trait Arg {
    type Out<'a>;
    type In<'a>: AsBytes;
    fn from_arg<'a>(n: usize, input: &'a mut Self::In<'a>) -> Result<Self::Out<'a>>;
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Arg for SBInfo {
    type In<'a> = SBInfo;
    type Out<'a> = &'a SBInfo;

    fn from_arg<'a>(n: usize, input: &'a mut Self::In<'a>) -> Result<Self::Out<'a>> {
        let addr: UVAddr = argraw(n).into();
        fetch_addr(addr, input)?;
        Ok(&*input)
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Arg for Path {
    type In<'a> = [u8; MAXPATH];
    type Out<'a> = &'a Self;

    fn from_arg<'a>(n: usize, input: &'a mut Self::In<'a>) -> Result<Self::Out<'a>> {
        let addr: UVAddr = argraw(n).into();
        let len = fetch_slice(Slice::Ref(addr), input)?.ok_or(InvalidArgument)?;
        Ok(Self::new(
            str::from_utf8_mut(&mut input[..len])
                .or(Err(Utf8Error))?
                .trim_end_matches(char::from(0)),
        ))
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Arg for File {
    type In<'a> = usize;
    type Out<'a> = (&'a mut File, usize);

    fn from_arg<'a>(n: usize, input: &'a mut Self::In<'a>) -> Result<Self::Out<'a>> {
        let p_data = Cpus::myproc().unwrap().data_mut();

        *input = argraw(n);
        match p_data.ofile.get_mut(*input).ok_or(FileDescriptorTooLarge)? {
            Some(f) => Ok((f, *input)),
            None => Err(BadFileDescriptor),
        }
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
struct Argv([Option<String>; MAXARG]);

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Arg for Argv {
    type In<'a> = [SBInfo; MAXARG];
    type Out<'a> = Self;

    fn from_arg<'a>(n: usize, input: &'a mut Self::In<'a>) -> Result<Self::Out<'a>> {
        let mut argv = Argv(array![None; MAXARG]);
        let mut buf = [0u8; PGSIZE];
        let addr = UVAddr::from(argraw(n));

        let n = fetch_slice(Slice::Ref(addr), input)?.ok_or(InvalidArgument)?;
        for (i, &argument) in input.iter().take(n).enumerate() {
            if let Some(len) = fetch_slice(Slice::Buf(argument), &mut buf).unwrap() {
                let arg_str = str::from_utf8_mut(&mut buf[..len])
                    .or(Err(Utf8Error))
                    .unwrap();
                argv.0[i].replace(arg_str.to_string());
            }
        }
        Ok(argv)
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
struct Envp([Option<String>; MAXARG]);

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Arg for Envp {
    type In<'a> = [SBInfo; MAXARG];
    type Out<'a> = Self;

    fn from_arg<'a>(n: usize, input: &'a mut Self::In<'a>) -> Result<Self::Out<'a>> {
        let mut envp = Envp(array![None; MAXARG]);
        let mut buf = [0u8; PGSIZE];
        let addr = UVAddr::from(argraw(n));

        let Some(n) = fetch_slice(Slice::Ref(addr), input)? else {
            return Ok(envp);
        };
        for (i, &env) in input.iter().take(n).enumerate() {
            if let Some(len) = fetch_slice(Slice::Buf(env), &mut buf).unwrap() {
                let env_str = str::from_utf8_mut(&mut buf[..len])
                    .or(Err(Utf8Error))
                    .unwrap();
                envp.0[i].replace(env_str.to_string());
            }
        }
        Ok(envp)
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn fdalloc(file: File) -> Result<usize> {
    for (fd, f) in Cpus::myproc()
        .unwrap()
        .data_mut()
        .ofile
        .iter_mut()
        .enumerate()
    {
        if f.is_none() {
            f.replace(file);
            return Ok(fd);
        }
    }
    Err(FileDescriptorTooLarge)
}

// Process related system calls
impl SysCalls {
    pub fn exit() -> ! {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        loop {
            core::hint::spin_loop();
        }
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            exit(argraw(0) as i32)
            // not reached
        }
    }

    pub fn getpid() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            Ok(Cpus::myproc().unwrap().pid())
        }
    }

    pub fn fork() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            fork()
        }
    }

    pub fn clone() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);

        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let fcn = argraw(0);
            let arg1 = argraw(1);
            let arg2 = argraw(2);
            let stack = argraw(3);
            clone(fcn, arg1, arg2, stack)
        }
    }

    pub fn wait() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let addr: UVAddr = argraw(0).into();
            wait(addr)
        }
    }

    pub fn waitpid() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let pid = argraw(0) as isize;
            let addr: UVAddr = argraw(1).into();
            let options = argraw(2);
            waitpid(pid, addr, options)
        }
    }

    pub fn join() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let addr: UVAddr = argraw(0).into();
            join(addr)
        }
    }

    pub fn sbrk() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let p = Cpus::myproc().unwrap();
            let n = argraw(0) as isize;
            let addr = p.data().aspace.as_ref().unwrap().inner.lock().sz;
            grow(n).and(Ok(addr))
        }
    }

    pub fn mmap() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);

        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let addr = argraw(0);
            let len = argraw(1);
            let prot = argraw(2);
            let flags = argraw(3);
            let fd = argraw(4);
            let offset = argraw(5);
            mmap(addr, len, prot, flags, fd, offset)
        }
    }

    pub fn munmap() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());

        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let addr = argraw(0);
            let len = argraw(1);
            munmap(addr, len)
        }
    }

    pub fn sleep() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let p = Cpus::myproc().unwrap();
            let n = argraw(0);
            let mut ticks = TICKS.lock();
            let ticks0 = *ticks;
            while *ticks - ticks0 < n {
                if p.inner.lock().killed {
                    return Err(Interrupted);
                }
                let pending =
                    p.inner.lock().sig_pending & !crate::signal::sig_mask(crate::signal::SIGCONT);
                if pending != 0 {
                    return Err(Interrupted);
                }
                ticks = sleep(&(*ticks) as *const _ as usize, ticks);
            }
            Ok(())
        }
    }

    pub fn kill() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let pid = argraw(0);
            let sig = argraw(1);

            if pid == 0 {
                Err(PermissionDenied)
            } else {
                kill(pid, sig)
            }
        }
    }

    pub fn sigaction() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let signum = argraw(0);
            let handler = argraw(1);
            let restorer = argraw(2);
            sigaction(signum, handler, restorer)
        }
    }

    pub fn sigreturn() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            sigreturn()
        }
    }

    pub fn setitimer() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let initial = argraw(0);
            let interval = argraw(1);
            setitimer(initial, interval)
        }
    }

    pub fn shmcreate() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let size = argraw(0);
            ipc::shm_create(size)
        }
    }

    pub fn shmattach() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let id = argraw(0);
            let prot = argraw(1);
            ipc::shm_attach(id, prot)
        }
    }

    pub fn shmdetach() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let addr = argraw(0);
            ipc::shm_detach(addr)
        }
    }

    pub fn shmdestroy() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let id = argraw(0);
            ipc::shm_destroy(id)
        }
    }

    pub fn semcreate() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let value = argraw(0);
            ipc::sem_create(value)
        }
    }

    pub fn semwait() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let id = argraw(0);
            ipc::sem_wait(id)
        }
    }

    pub fn semtrywait() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let id = argraw(0);
            ipc::sem_try_wait(id)
        }
    }

    pub fn sempost() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let id = argraw(0);
            ipc::sem_post(id)
        }
    }

    pub fn semclose() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let id = argraw(0);
            ipc::sem_close(id)
        }
    }

    pub fn uptime() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            Ok(*TICKS.lock())
        }
    }

    pub fn freepages() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            Ok(crate::kalloc::free_pages())
        }
    }

    pub fn ext_irq_count() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);

        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            use core::sync::atomic::Ordering;
            Ok(crate::trap::EXT_IRQS.load(Ordering::Relaxed))
        }
    }

    pub fn ktaskpolls() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            Ok(task::poll_count_total())
        }
    }

    pub fn getnprocs() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            Ok(crate::param::NCPU)
        }
    }

    pub fn getnprocsconf() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            Ok(crate::param::NCPU)
        }
    }
}

// System Calls related to File operations
impl SysCalls {
    pub fn dup() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut _fd = 0;
            let (f, _) = File::from_arg(0, &mut _fd)?;
            fdalloc(f.clone())
        }
    }

    pub fn dup2() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let p = Cpus::myproc().unwrap().data_mut();
            let src_fd = argraw(0);
            let dst_fd = argraw(1);
            if src_fd == dst_fd {
                return Ok(dst_fd);
            }

            let src = p
                .ofile
                .get(src_fd)
                .ok_or(FileDescriptorTooLarge)?
                .as_ref()
                .ok_or(BadFileDescriptor)?
                .clone();

            let mut dst = src;
            dst.clear_cloexec();

            p.ofile
                .get_mut(dst_fd)
                .ok_or(FileDescriptorTooLarge)?
                .replace(dst);

            Ok(dst_fd)
        }
    }

    pub fn read() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut _fd = 0;
            let mut sbinfo: SBInfo = Default::default();

            let (f, _) = File::from_arg(0, &mut _fd)?;
            let sbinfo = SBInfo::from_arg(1, &mut sbinfo)?;

            f.read(sbinfo.ptr.into(), sbinfo.len)
        }
    }

    pub fn write() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut _fd = 0;
            let mut sbinfo: SBInfo = Default::default();

            let (f, _) = File::from_arg(0, &mut _fd)?;
            let sbinfo = SBInfo::from_arg(1, &mut sbinfo)?;

            f.write(sbinfo.ptr.into(), sbinfo.len)
        }
    }

    pub fn close() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            File::from_arg(0, &mut fd)?;
            let f = Cpus::myproc().unwrap().data_mut().ofile[fd].take().unwrap();
            if let Some((dev, inum)) = f.lock_key() {
                let pid = Cpus::myproc().unwrap().pid();
                fcntl::clear_locks(dev, inum, pid);
            }
            let _f = f;
            drop(_f);
            Ok(())
        }
    }

    pub fn fstat() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            let st: UVAddr = argraw(1).into();
            let (f, _) = File::from_arg(0, &mut fd)?;

            f.stat(From::from(st))
        }
    }

    pub fn fsync() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            let (f, _) = File::from_arg(0, &mut fd)?;
            f.sync()
        }
    }

    pub fn logcrash() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let stage = argraw(0);
            let stage = LogCrashStage::from_usize(stage).ok_or(InvalidArgument)?;
            set_crash_stage(stage);
            Ok(())
        }
    }

    pub fn link() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut old = [0; MAXPATH];
            let mut new = [0; MAXPATH];
            let old_path = Path::from_arg(0, &mut old)?;
            let new_path = Path::from_arg(1, &mut new)?;

            let old_remote = dfs::is_remote_path(old_path);
            let new_remote = dfs::is_remote_path(new_path);
            if old_remote || new_remote {
                if old_remote != new_remote {
                    return Err(CrossesDevices);
                }
                return dfs::link(old_path, new_path);
            }

            let res;
            {
                LOG.begin_op();
                res = fs::link(old_path, new_path);
                LOG.end_op();
            }
            res
        }
    }

    pub fn symlink() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut target = [0; MAXPATH];
            let mut linkpath = [0; MAXPATH];
            let target = Path::from_arg(0, &mut target)?;
            let linkpath = Path::from_arg(1, &mut linkpath)?;

            if dfs::is_remote_path(linkpath) {
                return dfs::symlink(target.as_str(), linkpath);
            }

            let res;
            {
                LOG.begin_op();
                res = fs::symlink(target, linkpath);
                LOG.end_op();
            }
            res
        }
    }

    pub fn socket() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let domain = argraw(0);
            let stype = argraw(1);
            let protocol = argraw(2);
            let file = File::socket(domain, stype, protocol)?;
            fdalloc(file)
        }
    }

    pub fn bind() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            let mut path = [0u8; MAXPATH];
            let (f, _) = File::from_arg(0, &mut fd)?;
            let path = Path::from_arg(1, &mut path)?;
            f.bind(path.as_str())
        }
    }

    pub fn listen() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            let (f, _) = File::from_arg(0, &mut fd)?;
            let backlog = argraw(1);
            f.listen(backlog)
        }
    }

    pub fn accept() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            let (f, _) = File::from_arg(0, &mut fd)?;
            let file = f.accept()?;
            fdalloc(file)
        }
    }

    pub fn connect() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut fd = 0;
            let mut path = [0u8; MAXPATH];
            let (f, _) = File::from_arg(0, &mut fd)?;
            let path = Path::from_arg(1, &mut path)?;
            f.connect(path.as_str())
        }
    }

    pub fn setpgid() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let pid = argraw(0);
            let pgid = argraw(1);
            setpgid(pid, pgid)
        }
    }

    pub fn getpgrp() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        getpgrp()
    }

    pub fn setsid() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        setsid()
    }

    pub fn tcgetpgrp() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let fd = argraw(0);
            let p = Cpus::myproc().unwrap();
            let data = p.data();
            let file = data
                .ofile
                .get(fd)
                .ok_or(FileDescriptorTooLarge)?
                .as_ref()
                .ok_or(BadFileDescriptor)?;
            if !file.is_console() {
                return Err(InvalidArgument);
            }
            let sid = p.inner.lock().sid;
            if console::session() != 0 && console::session() != sid {
                return Err(PermissionDenied);
            }
            Ok(console::fg_pgrp())
        }
    }

    pub fn tcsetpgrp() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let fd = argraw(0);
            let pgid = argraw(1);
            if pgid == 0 {
                return Err(InvalidArgument);
            }
            let p = Cpus::myproc().unwrap();
            let data = p.data();
            let file = data
                .ofile
                .get(fd)
                .ok_or(FileDescriptorTooLarge)?
                .as_ref()
                .ok_or(BadFileDescriptor)?;
            if !file.is_console() {
                return Err(InvalidArgument);
            }
            let sid = p.inner.lock().sid;
            if !pgid_in_session(pgid, sid) {
                return Err(PermissionDenied);
            }
            console::set_fg_pgrp(sid, pgid)
        }
    }

    pub fn unlink() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut path = [0; MAXPATH];
            let path = Path::from_arg(0, &mut path)?;

            if dfs::is_remote_path(path) {
                return dfs::unlink(path);
            }

            let res;
            {
                LOG.begin_op();
                res = fs::unlink(path);
                LOG.end_op();
            }
            res
        }
    }

    pub fn open() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut path = [0u8; MAXPATH];
            let omode = argraw(1);
            let path = Path::from_arg(0, &mut path)?;

            if dfs::is_remote_path(path) {
                let handle = dfs::open(path, omode)?;
                return FTABLE
                    .alloc(
                        OMode::from_usize(omode),
                        FType::Remote(RemoteFile::new(handle)),
                    )
                    .and_then(fdalloc);
            }

            let fd;
            {
                LOG.begin_op();
                fd = FTABLE
                    .alloc(OMode::from_usize(omode), FType::Node(path))
                    .and_then(fdalloc);
                LOG.end_op();
            }
            fd
        }
    }

    pub fn mkdir() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut path = [0u8; MAXPATH];
            let path = Path::from_arg(0, &mut path)?;

            if dfs::is_remote_path(path) {
                return dfs::mkdir(path);
            }

            let res;
            {
                LOG.begin_op();
                res = fs::create(path, FileType::Dir, 0, 0).and(Ok(()));
                LOG.end_op();
            }
            res
        }
    }

    pub fn mknod() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut path = [0u8; MAXPATH];
            let path = Path::from_arg(0, &mut path)?;
            let major = argraw(1) as u16;
            let minor = argraw(2) as u16;

            let res;
            {
                LOG.begin_op();
                res = fs::create(path, FileType::Device, major, minor).and(Ok(()));
                LOG.end_op();
            }
            res
        }
    }

    #[allow(clippy::redundant_closure_call)]
    pub fn chdir() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut path = [0u8; MAXPATH];
            let data = Cpus::myproc().unwrap().data_mut();
            let path = Path::from_arg(0, &mut path)?;

            let res;
            {
                LOG.begin_op();
                let mut chidr = || -> Result<()> {
                    let (_, ip) = path.namei()?;
                    {
                        let ip_guard = ip.lock();
                        if ip_guard.itype() != FileType::Dir {
                            return Err(NotADirectory);
                        }
                    }
                    data.cwd.replace(ip);
                    Ok(())
                };
                res = chidr();
                LOG.end_op();
            }
            res
        }
    }

    pub fn exec() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut path = [0u8; MAXPATH];
            let mut uargv: [SBInfo; MAXARG] = Default::default();
            let mut uenvp: [SBInfo; MAXARG] = Default::default();
            let path = Path::from_arg(0, &mut path)?;
            let argv = Argv::from_arg(1, &mut uargv)?;
            let envp = Envp::from_arg(2, &mut uenvp)?;
            exec(path, argv.0, envp.0)
        }
    }

    pub fn pipe() -> Result<()> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(());
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let slice_addr: UVAddr = argraw(0).into();
            let mut ptr: UVAddr = UVAddr::from(0);
            let mut len: usize = 0;
            fetch_addr(slice_addr, &mut ptr)?;
            fetch_addr(slice_addr + core::mem::size_of::<usize>(), &mut len)?;

            if len > 3 {
                return Err(InvalidArgument);
            }

            let (rf, wf) = Pipe::alloc()?;
            let fd0 = fdalloc(rf)?;
            let fd1 = match fdalloc(wf) {
                Ok(fd) => fd,
                Err(err) => {
                    Cpus::myproc().unwrap().data_mut().ofile[fd0].take();
                    return Err(err);
                }
            };

            if either_copyout(ptr.into(), &fd0).is_err()
                || either_copyout((ptr + size_of::<usize>()).into(), &fd1).is_err()
            {
                let p_data = Cpus::myproc().unwrap().data_mut();
                p_data.ofile[fd0].take();
                p_data.ofile[fd1].take();
                return Err(BadVirtAddr);
            }
            Ok(())
        }
    }

    pub fn fcntl() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut _fd = 0;

            let (f, _) = File::from_arg(0, &mut _fd)?;
            let cmd = FcntlCmd::from_usize(argraw(1));
            let arg = argraw(2);

            f.do_fcntl(cmd, arg)
        }
    }

    pub fn nonblock() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut _fd = 0;
            let (f, _) = File::from_arg(0, &mut _fd)?;
            let on = argraw(1) != 0;
            let cmd = if on {
                FcntlCmd::SetNonblock
            } else {
                FcntlCmd::ClearNonblock
            };
            f.do_fcntl(cmd, 0)
        }
    }

    pub fn poll() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        {
            let mut sbinfo: SBInfo = Default::default();
            let sbinfo = SBInfo::from_arg(0, &mut sbinfo)?;
            let timeout = argraw(1) as isize;

            if sbinfo.len > NOFILE {
                return Err(InvalidArgument);
            }

            if sbinfo.len == 0 {
                if timeout == 0 {
                    return Ok(0);
                }
                let start = *TICKS.lock();
                loop {
                    if timeout > 0 {
                        let now = *TICKS.lock();
                        if now - start >= timeout as usize {
                            return Ok(0);
                        }
                    }
                    if Cpus::myproc().unwrap().inner.lock().killed {
                        return Err(Interrupted);
                    }
                    let mut ticks = TICKS.lock();
                    let now = *ticks;
                    ticks = sleep(&now as *const _ as usize, ticks);
                }
            }

            let mut fds: Vec<poll::PollFd> = Vec::new();
            fds.resize(sbinfo.len, poll::PollFd::default());
            let Some(_) = fetch_slice(Slice::Buf(*sbinfo), &mut fds)? else {
                return Err(InvalidArgument);
            };

            let start = *TICKS.lock();
            loop {
                let ready = poll_check(&mut fds)?;
                if ready > 0 || timeout == 0 {
                    either_copyout(sbinfo.ptr.into(), &fds[..sbinfo.len])?;
                    return Ok(ready);
                }
                if timeout > 0 {
                    let now = *TICKS.lock();
                    if now - start >= timeout as usize {
                        either_copyout(sbinfo.ptr.into(), &fds[..sbinfo.len])?;
                        return Ok(0);
                    }
                }
                if Cpus::myproc().unwrap().inner.lock().killed {
                    return Err(Interrupted);
                }
                let mut ticks = TICKS.lock();
                let now = *ticks;
                ticks = sleep(&now as *const _ as usize, ticks);
            }
        }
    }

    pub fn select() -> Result<usize> {
        #[cfg(not(all(target_os = "none", feature = "kernel")))]
        return Ok(0);
        #[cfg(all(target_os = "none", feature = "kernel"))]
        Self::poll()
    }
}

impl SysCalls {
    pub fn from_usize(n: usize) -> Self {
        match n {
            1 => Self::Fork,
            2 => Self::Exit,
            3 => Self::Wait,
            4 => Self::Pipe,
            5 => Self::Read,
            6 => Self::Kill,
            7 => Self::Exec,
            8 => Self::Fstat,
            9 => Self::Chdir,
            10 => Self::Dup,
            11 => Self::Getpid,
            12 => Self::Sbrk,
            13 => Self::Sleep,
            14 => Self::Uptime,
            15 => Self::Open,
            16 => Self::Write,
            17 => Self::Mknod,
            18 => Self::Unlink,
            19 => Self::Link,
            20 => Self::Mkdir,
            21 => Self::Close,
            22 => Self::Dup2,
            23 => Self::Fcntl,
            24 => Self::Nonblock,
            25 => Self::Freepages,
            26 => Self::MMap,
            27 => Self::MunMap,
            28 => Self::Clone,
            29 => Self::Join,
            30 => Self::ExtIrqCount,
            31 => Self::KTaskPolls,
            32 => Self::Poll,
            33 => Self::Select,
            34 => Self::Waitpid,
            35 => Self::Sigaction,
            36 => Self::Sigreturn,
            37 => Self::Setitimer,
            38 => Self::ShmCreate,
            39 => Self::ShmAttach,
            40 => Self::ShmDetach,
            41 => Self::ShmDestroy,
            42 => Self::SemCreate,
            43 => Self::SemWait,
            44 => Self::SemTryWait,
            45 => Self::SemPost,
            46 => Self::SemClose,
            47 => Self::Fsync,
            48 => Self::Symlink,
            49 => Self::Socket,
            50 => Self::Bind,
            51 => Self::Listen,
            52 => Self::Accept,
            53 => Self::Connect,
            54 => Self::Setpgid,
            55 => Self::Getpgrp,
            56 => Self::Setsid,
            57 => Self::Tcgetpgrp,
            58 => Self::Tcsetpgrp,
            59 => Self::LogCrash,
            60 => Self::Getnprocs,
            61 => Self::Getnprocsconf,
            _ => Self::Invalid,
        }
    }
}

// Generate system call interface for userland
#[cfg(not(target_os = "none"))]
impl SysCalls {
    pub fn into_enum_iter() -> std::vec::IntoIter<SysCalls> {
        (0..core::mem::variant_count::<SysCalls>())
            .map(SysCalls::from_usize)
            .collect::<Vec<SysCalls>>()
            .into_iter()
    }

    pub fn signature(self) -> String {
        let syscall = Self::TABLE[self as usize];
        format!(
            "fn {}{} -> {}",
            self.fn_name(),
            syscall.1,
            self.return_type()
        )
    }

    pub fn return_type(&self) -> &'static str {
        match Self::TABLE[*self as usize].0 {
            Fn::I(_) => "Result<usize>",
            Fn::U(_) => "Result<()>",
            Fn::N(_) => "!",
        }
    }

    pub fn fn_name(&self) -> String {
        format!("{:?}", self).to_lowercase()
    }

    pub fn args(&self) -> Vec<(&'static str, &'static str)> {
        Self::TABLE[*self as usize]
            .1
            .strip_suffix(')')
            .unwrap()
            .strip_prefix('(')
            .unwrap()
            .split(',')
            .filter_map(|s| s.trim().split_once(": "))
            .collect::<Vec<(&str, &str)>>()
    }

    pub fn gen_usys(self) -> String {
        let mut i = 0;
        let indent = 4;
        let part1 = format!(
            r#"
pub {} {{
    let _ret: isize;
    unsafe {{
        asm!(
            "ecall",{}"#,
            self.signature(),
            "\n",
        );
        let mut part2 = self
            .args()
            .iter()
            .map(|s| match s {
                (_, s1) if s1.contains("&str") | s1.contains("&[") | s1.contains("&mut [") => {
                    let ret = format!(
                        "{:indent$}in(\"a{}\") &{} as *const _ as usize,\n",
                        "",
                        i,
                        s.0,
                        indent = indent * 3
                    );
                    i += 1;
                    ret
                }
                (_, s1) if !s1.contains(']') && s1.contains("&mut ") => {
                    let ret = format!(
                        "{:indent$}in(\"a{}\") {} as *mut _ as usize,\n",
                        "",
                        i,
                        s.0,
                        indent = indent * 3
                    );
                    i += 1;
                    ret
                }
                (_, s1) if !s1.contains(']') && s1.contains('&') => {
                    let ret = format!(
                        "{:indent$}in(\"a{}\") {} as *const _ as usize,\n",
                        "",
                        i,
                        s.0,
                        indent = indent * 3
                    );
                    i += 1;
                    ret
                }
                (_, s1) if s1.contains("FcntlCmd") => {
                    let ret = format!(
                        "{:indent$}in(\"a{}\") {} as usize,\n",
                        "",
                        i,
                        s.0,
                        indent = indent * 3
                    );
                    i += 1;
                    ret
                }
                (_, _) => {
                    let ret = format!(
                        "{:indent$}in(\"a{}\") {},\n",
                        "",
                        i,
                        s.0,
                        indent = indent * 3
                    );
                    i += 1;
                    ret
                }
            })
            .collect::<Vec<String>>();
        let part3 = format!(
            r#"{:indent$}in("a7") {},
            lateout("a0") _ret,
        );
    }}
"#,
            "",
            self as usize,
            indent = indent * 3
        );
        let part4 = format!(
            "{:indent$}{}\n}}",
            "",
            match Self::TABLE[self as usize].0 {
                Fn::I(_) =>
                    "match _ret { 0.. => Ok(_ret as usize), _ => Err(Error::from_isize(_ret)) }",
                Fn::U(_) => "match _ret { 0 => Ok(()), _ => Err(Error::from_isize(_ret)) }",
                Fn::N(_) => "unreachable!()",
            },
            indent = indent
        );
        let mut out: Vec<String> = Vec::new();
        out.push(part1);
        out.append(&mut part2);
        out.push(part3);
        out.push(part4);
        out.iter().flat_map(|s| s.chars()).collect::<String>()
    }
}
