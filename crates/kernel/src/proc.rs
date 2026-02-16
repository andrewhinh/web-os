use alloc::string::String;
use alloc::vec::Vec;
use alloc::{boxed::Box, sync::Arc};
use core::arch::asm;
use core::mem::size_of;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::{cell::UnsafeCell, ops::Drop};

use crate::bio::BCACHE;
use crate::defs::AsBytes;
use crate::elf::{self, ElfHdr, ProgHdr};
use crate::error::{Error::*, Result};
use crate::exec::flags2perm;
use crate::file::File;
use crate::fs::{self, Inode, Path};
use crate::ipc::ShmSegment;
use crate::log::LOG;
use crate::memlayout::{STACK_PAGE_NUM, TRAMPOLINE, kstack, trapframe_va, user_mem_top};
use crate::mmap::{MAP_ANON, MAP_PRIVATE, MAP_SHARED, PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::param::*;
use crate::riscv::registers::scause::Exception;
use crate::riscv::{pteflags::*, *};
use crate::runq::{runq_is_empty, runq_pop, runq_push_cpu};
use crate::signal::{
    NSIG, SIG_DFL, SIG_IGN, SIGALRM, SIGCONT, SIGKILL, SigDefaultAction, WCONTINUED, WNOHANG,
    WUNTRACED, default_action, sig_mask,
};
use crate::spinlock::{Mutex, MutexGuard};
use crate::swtch::swtch;
use crate::sync::{LazyLock, OnceLock};
use crate::task::{ready_is_empty_cpu, run_ready_tasks_cpu};
use crate::trampoline::trampoline;
use crate::trap::{TICKS, usertrap_ret};
use crate::vm::{Addr, KVAddr, KVM, PAddr, Page, PageAllocator, Stack, UVAddr, Uvm, VirtAddr};
use crate::{array, println};

pub static CPUS: Cpus = Cpus::new();

#[allow(clippy::redundant_closure)]
pub static PROCS: LazyLock<Procs> = LazyLock::new(|| Procs::new());
pub static INITPROC: OnceLock<Arc<Proc>> = OnceLock::new();

#[inline]
fn make_runnable(idx: usize, guard: &mut ProcInner) {
    if guard.state != ProcState::RUNNABLE {
        guard.state = ProcState::RUNNABLE;
        let cpu = if guard.last_cpu < NCPU {
            guard.last_cpu
        } else {
            unsafe { Cpus::cpu_id() }
        };
        guard.last_cpu = cpu;
        runq_push_cpu(cpu, idx);
    }
}

#[derive(Debug)]

pub struct AddrSpace {
    pub inner: Mutex<AddrSpaceInner>,
}

#[derive(Debug)]

pub struct AddrSpaceInner {
    pub uvm: Option<Uvm>,
    pub sz: usize,
}

// Address spaces are shared across CPUs/threads.

unsafe impl Send for AddrSpace {}
unsafe impl Sync for AddrSpace {}

impl AddrSpace {
    pub fn new(uvm: Uvm, sz: usize) -> Self {
        Self {
            inner: Mutex::new(AddrSpaceInner { uvm: Some(uvm), sz }, "aspace"),
        }
    }
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        let mut inner = self.inner.lock();
        let Some(mut uvm) = inner.uvm.take() else {
            return;
        };
        let _ = uvm.try_unmap(TRAMPOLINE.into(), 1, false);
        for i in 0..NPROC {
            let _ = uvm.try_unmap(trapframe_va(i).into(), 1, false);
        }
        uvm.free(inner.sz);
    }
}

pub struct Cpus([UnsafeCell<Cpu>; NCPU]);
unsafe impl Sync for Cpus {}

// Per-CPU state
#[derive(Debug)]
pub struct Cpu {
    pub proc: Option<Arc<Proc>>,  // The process running on this cpu, or None.
    pub context: Context,         // swtch() here to enter scheduler().
    pub noff: isize,              // Depth of interrupts lock(lock_mycpu() depth).
    pub nest: [&'static str; 20], // manage nest for debugging.
    pub intena: bool,             // Were interrupts enabled before lock_mycpu()?
}

impl Cpus {
    const fn new() -> Self {
        Self(array![UnsafeCell::new(Cpu::new()); NCPU])
    }

    // # Safety
    // Must be called with interrupts disabled,
    // to prevent race with process being moved
    // to a different CPU.
    #[inline]
    pub unsafe fn cpu_id() -> usize {
        let id;
        unsafe { asm!("mv {0}, tp", out(reg) id) };
        id
    }

    // Return the reference to this Cpus's Cpu struct.
    // # Safety
    // interrupts must be disabled.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn mycpu(&self) -> *mut Cpu {
        let id = unsafe { Self::cpu_id() };
        self.0[id].get()
    }

    // Return the current proc pointer: Some(Arc<Proc>), or None if none.
    pub fn myproc() -> Option<Arc<Proc>> {
        let _intr_lock = Self::lock_mycpu("withoutspin");
        let c;
        unsafe {
            c = &*CPUS.mycpu();
        }
        c.proc.clone()
    }

    // disable interrupts on mycpu().
    // if all `IntrLock` are dropped, interrupts may recover
    // to previous state.
    pub fn lock_mycpu(name: &'static str) -> IntrLock {
        let old = intr_get();
        intr_off();
        unsafe { (*CPUS.mycpu()).locked(old, name) }
    }
}

impl Cpu {
    const fn new() -> Self {
        Self {
            proc: None,
            context: Context::new(),
            noff: 0,
            nest: [
                "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
            ],
            intena: false,
        }
    }

    // if all `IntrLock`'s are dropped, interrupts may recover
    // to previous state.
    fn locked(&mut self, old: bool, name: &'static str) -> IntrLock {
        if self.noff == 0 {
            self.intena = old;
        }
        assert!(
            (self.noff as usize) < self.nest.len(),
            "intrlock nest overflow"
        );
        self.nest[self.noff as usize] = name;
        self.noff += 1;
        IntrLock
    }

    pub fn unlock(&mut self) {
        // interrupts must be disabled.
        assert!(!intr_get(), "core unlock - interruptible");
        assert!(self.noff >= 1, "unlock");
        self.nest[(self.noff - 1) as usize] = "";

        self.noff -= 1;
        if self.noff == 0 && self.intena {
            intr_on()
        }
    }
}

#[derive(Debug)]
pub struct IntrLock;

impl Drop for IntrLock {
    fn drop(&mut self) {
        unsafe { (&mut *CPUS.mycpu()).unlock() }
    }
}

// Saved registers for kernel context switches.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Context {
    pub ra: usize,
    pub sp: usize,

    // callee-saved
    pub s0: usize,
    pub s1: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

// Per-process data for the trampoline.rs trap processing code, mapped
// to the page immediately below the trampoline page in the user page
// table. It is not mapped in the kernel page table. uservec() in trampoline.rs
// stores user registers in trapframe, restore registers from kernel_sp,
// kernel_hartid, and kernel_satp of trapframe, and jumps to the address
// pointed to by kernel_trap (usertrap()). The sequence of usertrap_ret()
// in trap.rs and userret() in trampoline.rs sets up kernel_* in trapframe,
// restores the user registers from trapframe, switches to the user page
// table and user space enters the user space. The return path to the
// user via usertrap_ret() does not return through the entire kernel
// call stack, so the trapframe contains callee-saved user registers
// such as s0-s11.
#[derive(Clone, Copy, Default, Debug)]
#[repr(C, align(4096))]
pub struct Trapframe {
    // 0
    pub kernel_satp: usize, // kernel page table
    // 8
    pub kernel_sp: usize, // top of process's kernel stack
    // 16
    pub kernel_trap: usize, // usertrap()
    // 24
    pub epc: usize, // saved user program counter
    // 32
    pub kernel_hartid: usize, // saved kernel tp
    // 40
    pub ra: usize,
    // 48
    pub sp: usize,
    // 56
    pub gp: usize,
    // 64
    pub tp: usize,
    // 72
    pub t0: usize,
    // 80
    pub t1: usize,
    // 88
    pub t2: usize,
    // 96
    pub s0: usize,
    // 104
    pub s1: usize,
    // 112
    pub a0: usize,
    // 120
    pub a1: usize,
    // 128
    pub a2: usize,
    // 136
    pub a3: usize,
    // 144
    pub a4: usize,
    // 152
    pub a5: usize,
    // 160
    pub a6: usize,
    // 168
    pub a7: usize,
    // 176
    pub s2: usize,
    // 184
    pub s3: usize,
    // 192
    pub s4: usize,
    // 200
    pub s5: usize,
    // 208
    pub s6: usize,
    // 216
    pub s7: usize,
    // 224
    pub s8: usize,
    // 232
    pub s9: usize,
    // 240
    pub s10: usize,
    // 248
    pub s11: usize,
    // 256
    pub t3: usize,
    // 264
    pub t4: usize,
    // 272
    pub t5: usize,
    // 280
    pub t6: usize,
}

#[derive(Debug)]
pub struct Procs {
    pub pool: [Arc<Proc>; NPROC],
    parents: Mutex<[Option<Arc<Proc>>; NPROC]>,
}
unsafe impl Sync for Procs {}

#[derive(Debug)]
pub struct Proc {
    // process table index.
    idx: usize,
    // lock must be held when using inner data:
    pub inner: Mutex<ProcInner>,
    // these are private to the process, so lock need not be held.
    pub data: UnsafeCell<ProcData>,
}
unsafe impl Sync for Proc {}

// lock must be held when using these:
#[derive(Clone, Copy, Debug)]
pub struct ProcInner {
    pub state: ProcState, // Process state
    pub chan: usize,      // if non-zero, sleeping on chan
    pub killed: bool,     // if true, have been killed
    pub xstate: i32,      // Exit status to be returned to parent's wait
    pub pid: PId,         // Process ID
    pub pgid: usize,      // Process group ID
    pub sid: usize,       // Session ID
    pub last_cpu: usize,  // Last CPU this process was run on
    pub sig_pending: u32,
    pub sig_handlers: [usize; NSIG],
    pub sig_alarm_deadline: usize,
    pub sig_alarm_interval: usize,
    pub stop_sig: usize,
    pub stop_reported: bool,
    pub cont_pending: bool,
}

// These are private to the process, so lock need not be held.
#[derive(Debug)]
pub struct ProcData {
    pub kstack: KVAddr,                    // Virtual address of kernel stack
    pub aspace: Option<Arc<AddrSpace>>,    // Shared address space (user pagetable + size)
    pub trapframe: Option<Box<Trapframe>>, // data page for trampoline.rs
    pub trapframe_va: UVAddr,              // user-VA of this proc's trapframe mapping
    pub context: Context,                  // swtch() here to run process
    pub sig_trapframe: Trapframe,          // saved trapframe during signal
    pub sig_active: bool,                  // currently in signal handler
    pub sig_restorer: usize,               // user-space restorer for signals
    pub name: String,                      // Process name (debugging)
    pub is_thread: bool,                   // created by clone()
    pub ustack: usize,                     // clone()'s stack base
    pub ofile: [Option<File>; NOFILE],     // Open files
    pub cwd: Option<Inode>,                // Current directory
    pub mmap_base: usize,                  // top-down allocator, starts at user_mem_top(NPROC)
    pub vmas: Vec<Vma>,
}
unsafe impl Sync for ProcData {}
unsafe impl Send for ProcData {}

#[derive(Clone, Debug)]

pub struct Vma {
    pub start: UVAddr,
    pub len: usize,   // requested len (bytes)
    pub prot: usize,  // PROT_*
    pub flags: usize, // MAP_*
    pub file: Option<File>,
    pub file_off: usize,
    pub shm: Option<Arc<ShmSegment>>,
}

impl Vma {
    fn end_req(&self) -> UVAddr {
        self.start + self.len
    }

    fn len_pg(&self) -> usize {
        pgroundup(self.len)
    }

    fn end_pg(&self) -> UVAddr {
        self.start + self.len_pg()
    }

    fn contains_pg(&self, va: UVAddr) -> bool {
        va >= self.start && va < self.end_pg()
    }

    fn perm(&self) -> usize {
        let mut perm = PTE_U;
        if (self.prot & PROT_READ) != 0 {
            perm |= PTE_R;
        }
        if (self.prot & PROT_WRITE) != 0 {
            perm |= PTE_W;
        }
        if (self.prot & PROT_EXEC) != 0 {
            perm |= PTE_X;
        }
        perm
    }

    fn is_shared(&self) -> bool {
        (self.flags & MAP_SHARED) != 0
    }

    fn is_anon(&self) -> bool {
        (self.flags & MAP_ANON) != 0
    }

    fn is_shm(&self) -> bool {
        self.shm.is_some()
    }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum ProcState {
    UNUSED,
    USED,
    SLEEPING,
    STOPPED,
    RUNNABLE,
    RUNNING,
    ZOMBIE,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PId(usize);

impl PId {
    fn alloc() -> Self {
        static NEXTID: AtomicUsize = AtomicUsize::new(0);
        PId(NEXTID.fetch_add(1, Ordering::Relaxed))
    }
}

impl Context {
    pub const fn new() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s0: 0,
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            s5: 0,
            s6: 0,
            s7: 0,
            s8: 0,
            s9: 0,
            s10: 0,
            s11: 0,
        }
    }

    pub fn write_zero(&mut self) {
        self.ra = 0;
        self.sp = 0;
        self.s0 = 0;
        self.s1 = 0;
        self.s2 = 0;
        self.s3 = 0;
        self.s4 = 0;
        self.s5 = 0;
        self.s6 = 0;
        self.s7 = 0;
        self.s8 = 0;
        self.s9 = 0;
        self.s10 = 0;
        self.s11 = 0;
    }
}

impl Default for Procs {
    fn default() -> Self {
        Self::new()
    }
}

impl Procs {
    pub fn new() -> Self {
        let mut i = 0;
        Self {
            pool: core::iter::repeat_with(|| {
                let p = Arc::new(Proc::new(i));
                i += 1;
                p
            })
            .take(NPROC)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
            parents: Mutex::new(
                core::iter::repeat_with(|| None)
                    .take(NPROC)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
                "parents",
            ),
        }
    }

    // Allocate STACK_PAGE_NUM pages for each process's kernel stack.
    // map it high in memory, followed by an invalid guard page.
    #[allow(static_mut_refs)]
    pub unsafe fn mapstacks(&self) {
        for (p, _) in self.pool.iter().enumerate() {
            let pa = unsafe { Stack::try_new_zeroed() }.unwrap() as usize;
            let va = kstack(p);
            unsafe {
                KVM.get_mut()
                    .unwrap()
                    .map(va, pa.into(), PGSIZE * STACK_PAGE_NUM, PTE_R | PTE_W);
            }
        }
    }

    // Look in the process table for an UNUSED proc. If found,
    // initialize state required to run in the kernel, and return
    // reference to the proc with "proc" lock held. If there are
    // no free procs, or a memory allocation fails, return None.
    fn alloc(&self) -> Result<(&Arc<Proc>, MutexGuard<'_, ProcInner>)> {
        for p in self.pool.iter() {
            let mut lock = p.inner.lock();
            match lock.state {
                ProcState::UNUSED => {
                    lock.pid = PId::alloc();
                    lock.state = ProcState::USED;
                    lock.pgid = lock.pid.0;
                    lock.sid = lock.pid.0;
                    lock.stop_sig = 0;
                    lock.stop_reported = false;
                    lock.cont_pending = false;

                    let data = p.data_mut();
                    // Allocate a trapframe page.
                    if let Ok(tf) = Box::<Trapframe>::try_new_zeroed() {
                        data.trapframe.replace(unsafe { tf.assume_init() });
                    } else {
                        p.free(lock);
                        return Err(OutOfMemory);
                    }

                    data.trapframe_va = trapframe_va(p.idx).into();

                    // An empty user page table.
                    match p.uvmcreate() {
                        Ok(uvm) => {
                            data.aspace.replace(Arc::new(AddrSpace::new(uvm, 0)));
                        }
                        Err(err) => {
                            p.free(lock);
                            return Err(err);
                        }
                    }

                    // Set up new context to start executing at forkret,
                    // which returns to user space.
                    data.context.write_zero();
                    data.context.ra = fork_ret as *const () as usize;
                    data.context.sp = data.kstack.into_usize() + PGSIZE * STACK_PAGE_NUM;
                    return Ok((p, lock));
                }
                _ => continue,
            }
        }
        Err(WouldBlock)
    }
}

// initialize the proc table at boottime.
pub fn init() {
    for (i, proc) in PROCS.pool.iter().enumerate() {
        proc.data_mut().kstack = kstack(i);
    }
}

impl Proc {
    pub fn new(idx: usize) -> Self {
        Self {
            idx,
            inner: Mutex::new(ProcInner::new(), "proc"),
            data: UnsafeCell::new(ProcData::new()),
        }
    }

    pub fn pid(&self) -> usize {
        self.inner.lock().pid.0
    }

    pub fn data(&self) -> &'static ProcData {
        unsafe { &*(self.data.get()) }
    }

    pub fn data_mut(&self) -> &'static mut ProcData {
        unsafe { &mut *(self.data.get()) }
    }

    fn free(&self, mut guard: MutexGuard<'_, ProcInner>) {
        let data = self.data_mut();
        let aspace = data.aspace.as_ref().map(Arc::clone);
        let mut writebacks = Vec::new();

        if let Some(aspace) = aspace {
            let mut olduvm = None;
            let mut oldsz = 0;
            {
                let mut inner = aspace.inner.lock();

                // Always unmap this proc/thread's trapframe from the user page table.
                if let Some(ref mut uvm) = inner.uvm {
                    let _ = uvm.try_unmap(data.trapframe_va, 1, false);
                }

                // If this is the last owner of the address space, tear down mmaps before
                // freeing.
                if !data.is_thread
                    && Arc::strong_count(&aspace) == 1
                    && let Some(uvm) = inner.uvm.take()
                {
                    oldsz = inner.sz;
                    inner.sz = 0;
                    olduvm = Some(uvm);
                }
            }
            if let Some(mut uvm) = olduvm {
                writebacks = data.munmap_all(&mut uvm);
                data.mmap_base = user_mem_top(NPROC);
                uvm.proc_uvmfree(oldsz);
            }
        }
        data.trapframe.take();
        data.aspace.take();
        data.vmas.clear();
        data.mmap_base = user_mem_top(NPROC);
        data.sig_trapframe = Trapframe::default();
        data.sig_active = false;
        data.sig_restorer = 0;
        guard.pid = PId(0);
        guard.pgid = 0;
        guard.sid = 0;
        data.name.clear();
        data.is_thread = false;
        data.ustack = 0;
        guard.chan = 0;
        guard.killed = false;
        guard.xstate = 0;
        guard.sig_pending = 0;
        guard.sig_handlers = [SIG_DFL; NSIG];
        guard.sig_alarm_deadline = 0;
        guard.sig_alarm_interval = 0;
        guard.stop_sig = 0;
        guard.stop_reported = false;
        guard.cont_pending = false;
        guard.state = ProcState::UNUSED;
        drop(guard);
        for wb in writebacks {
            let _ = wb.flush();
        }
    }

    // Create a user page table with no user memory
    // but with trampoline and trapframe pages.
    pub fn uvmcreate(&self) -> Result<Uvm> {
        // An empty user page table.
        let mut uvm = Uvm::create()?;

        // map the trampoline code (for system call return)
        // at the highest user virtual address.
        // only the supervisor uses it, on the way
        // to/from user space, so not PTE_U
        if let Err(err) = uvm.mappages(
            UVAddr::from(TRAMPOLINE),
            PAddr::from(trampoline as *const () as usize),
            PGSIZE,
            PTE_R | PTE_X,
        ) {
            uvm.free(0);
            return Err(err);
        }

        let data = self.data();
        // map the trapframe page just below the trampoline page, for
        // trampoline.rs
        if let Err(err) = uvm.mappages(
            data.trapframe_va,
            PAddr::from(data.trapframe.as_deref().unwrap() as *const _ as usize),
            PGSIZE,
            PTE_R | PTE_W,
        ) {
            uvm.unmap(UVAddr::from(TRAMPOLINE), 1, false);
            uvm.free(0);
            return Err(err);
        }

        Ok(uvm)
    }
}

pub fn either_copyout<T: ?Sized + AsBytes>(dst: VirtAddr, src: &T) -> Result<()> {
    match dst {
        VirtAddr::User(addr) => {
            let p = Cpus::myproc().unwrap();
            let aspace = p.data().aspace.as_ref().unwrap();
            let mut inner = aspace.inner.lock();
            inner.uvm.as_mut().unwrap().copyout(addr.into(), src)
        }
        VirtAddr::Kernel(addr) | VirtAddr::Physical(addr) => {
            let src = src.as_bytes();
            let len = src.len();
            assert!(PGSIZE > len, "either_copyout: len must be less than PGSIZE");
            let dst = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) };
            dst.copy_from_slice(src);
            Ok(())
        }
    }
}

pub fn either_copyin<T: ?Sized + AsBytes>(dst: &mut T, src: VirtAddr) -> Result<()> {
    match src {
        VirtAddr::User(addr) => {
            let p = Cpus::myproc().unwrap();
            let aspace = p.data().aspace.as_ref().unwrap();
            let mut inner = aspace.inner.lock();
            inner.uvm.as_mut().unwrap().copyin(dst, addr.into())
        }
        VirtAddr::Kernel(addr) | VirtAddr::Physical(addr) => {
            let dst = dst.as_bytes_mut();
            let len = dst.len();
            let src = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
            dst.copy_from_slice(src);
            Ok(())
        }
    }
}

// Set up first user process.
pub fn user_init(initcode: &'static [u8]) {
    let (p, ref mut guard) = PROCS.alloc().unwrap();
    INITPROC.set(p.clone()).unwrap();

    let data = p.data_mut();
    let aspace = data.aspace.as_ref().unwrap();
    let mut as_inner = aspace.inner.lock();
    let uvm = as_inner.uvm.as_mut().unwrap();

    let elf;
    unsafe {
        let (head, body, _) = initcode[0..size_of::<ElfHdr>()].align_to::<ElfHdr>();
        assert!(head.is_empty(), "elf_img is not aligned");
        elf = body.first().unwrap();
    }
    if elf.e_ident[elf::EI_MAG0] != elf::ELFMAG0
        || elf.e_ident[elf::EI_MAG1] != elf::ELFMAG1
        || elf.e_ident[elf::EI_MAG2] != elf::ELFMAG2
        || elf.e_ident[elf::EI_MAG3] != elf::ELFMAG3
    {
        panic!("initcode is not an elf img");
    }
    // Load program into user memory.
    let mut phdr: ProgHdr;
    let mut off = elf.e_phoff;
    let mut sz = 0;
    for _ in 0..elf.e_phnum {
        unsafe {
            let (head, body, _) = initcode[off..(off + size_of::<ProgHdr>())].align_to::<ProgHdr>();
            assert!(head.is_empty(), "elf program header is not aligned");
            phdr = *body.first().unwrap();
        }
        if phdr.p_type != elf::PT_LOAD || phdr.p_fsize == 0 {
            continue;
        }
        if phdr.p_msize < phdr.p_fsize {
            panic!("p_msize >= p_fsize");
        }
        let end_vaddr = phdr
            .p_vaddr
            .checked_add(phdr.p_msize)
            .expect("p_vaddr + p_msize overflow");
        let va = UVAddr::from(phdr.p_vaddr);
        assert!(va.is_aligned(), "init program va's must be aligned");

        sz = uvm.alloc(sz, end_vaddr, flags2perm(phdr.p_flags)).unwrap();

        // text segments may be mapped !PTE_W, so
        // load bytes by writing to the physical pages directly
        let src = initcode
            .get(phdr.p_offset..(phdr.p_offset + phdr.p_fsize))
            .expect("initcode: segment range");
        let mut i = 0usize;
        while i < phdr.p_fsize {
            let pa = uvm.walkaddr(va + i).unwrap();
            let n = core::cmp::min(PGSIZE, phdr.p_fsize - i);
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr().add(i), pa.into_usize() as *mut u8, n);
            }
            i += PGSIZE;
        }
        off += size_of::<ProgHdr>();
    }
    // Allocate two pages at the next page boundary.
    // Make the first inaccessible as a stack guard.
    // use the second as the user stack.
    sz = pgroundup(sz);
    sz = uvm.alloc(sz, sz + 2 * PGSIZE, pteflags::PTE_W).unwrap();
    uvm.clear(From::from(sz - 2 * PGSIZE));

    // prepare for the very first "return" from kernel to user.
    as_inner.sz = sz;
    let tf = data.trapframe.as_mut().unwrap();
    tf.epc = elf.e_entry; // user program counter
    tf.sp = UVAddr::from(sz).into_usize(); // user stack pointer

    data.name.push_str("initcode");
    make_runnable(p.idx, guard);
}

impl ProcInner {
    pub const fn new() -> Self {
        Self {
            state: ProcState::UNUSED,
            chan: 0,
            killed: false,
            xstate: 0,
            pid: PId(0),
            pgid: 0,
            sid: 0,
            last_cpu: 0,
            sig_pending: 0,
            sig_handlers: [SIG_DFL; NSIG],
            sig_alarm_deadline: 0,
            sig_alarm_interval: 0,
            stop_sig: 0,
            stop_reported: false,
            cont_pending: false,
        }
    }
}

impl Default for ProcInner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcData {
    pub fn new() -> Self {
        Self {
            kstack: KVAddr::from(0),
            aspace: None,
            trapframe: None,
            trapframe_va: UVAddr::from(0),
            context: Context::new(),
            sig_trapframe: Trapframe::default(),
            sig_active: false,
            sig_restorer: 0,
            name: String::new(),
            is_thread: false,
            ustack: 0,
            ofile: array![None; NOFILE],
            cwd: Default::default(),
            mmap_base: user_mem_top(NPROC),
            vmas: Vec::new(),
        }
    }
}

impl Default for ProcData {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcData {
    pub(crate) fn alloc_mmap_va(&mut self, sz: usize, len: usize) -> Result<UVAddr> {
        let len_pg = pgroundup(len);
        if len_pg == 0 {
            return Err(InvalidArgument);
        }
        if self.mmap_base < len_pg {
            return Err(NoBufferSpace);
        }
        let base = pgrounddown(self.mmap_base - len_pg);
        if base < pgroundup(sz) {
            return Err(NoBufferSpace);
        }
        self.mmap_base = base;
        Ok(UVAddr::from(base))
    }

    pub(crate) fn munmap_all(&mut self, uvm: &mut Uvm) -> Vec<Writeback> {
        let vmas = core::mem::take(&mut self.vmas);
        let mut writebacks = Vec::new();
        for v in vmas {
            let _ = munmap_vma_range(uvm, &v, v.start, v.len_pg(), &mut writebacks);
        }
        writebacks
    }
}

// A fork child's very first scheduling by scheduler()
// will swtch to fork_ret().
pub unsafe extern "C" fn fork_ret() -> ! {
    static mut FIRST: bool = true;

    // still holding "proc" lock from scheduler.
    // force_unlock() from my_proc() is needed because the stack is different
    unsafe {
        Cpus::myproc().unwrap().inner.force_unlock();
    }

    if unsafe { FIRST } {
        // File system initialization must be run in the context of a
        // regular process (e.g., because it calls sleep), and thus cannot
        // be run from main().
        unsafe {
            FIRST = false;
        }
        fs::init(ROOTDEV);
        // register initproc here, because namei must be called after fs initialization.
        INITPROC.get().unwrap().data_mut().cwd = Some(Path::new("/").namei().unwrap().1);
    }
    unsafe { usertrap_ret() }
}

// Print a process listing to console. For debugging.
// Runs when user types ^P on console.
// No lock to avoid wedging a stuck machine further.
pub fn dump() {
    println!("");
    for proc in PROCS.pool.iter() {
        let inner = unsafe { proc.inner.get_mut() };
        let data = unsafe { &(*proc.data.get()) };
        if inner.state != ProcState::UNUSED {
            println!(
                "pid: {:?} state: {:?} name: {:?}, chan: {}",
                inner.pid, inner.state, data.name, inner.chan
            );
        }
    }
}

// Per-CPU process scheduler.
// Each CPU calls scheduler() after setting itself up.
// Scheduler never returns. It loops, doing:
//  - choose a process to run.
//  - swtch to start running that process.
//  - eventually that process transfers control via swtch back to the scheduler.
pub fn scheduler() -> ! {
    let c = unsafe { CPUS.mycpu() };

    loop {
        // Avoid deadlock by ensuring that devices can interrupt.
        intr_on();

        let cpu = unsafe { Cpus::cpu_id() };
        run_ready_tasks_cpu(cpu, 32);

        let Some(idx) = runq_pop() else {
            intr_off();
            if runq_is_empty() && ready_is_empty_cpu(cpu) {
                unsafe {
                    asm!("wfi");
                }
            }
            intr_on();
            continue;
        };

        let p = &PROCS.pool[idx];
        let mut inner = p.inner.lock();
        if inner.state != ProcState::RUNNABLE {
            continue;
        }

        // Switch to chosen process. It is the process's job
        // to release its lock and then reacquire it
        // before jumping back to us.
        inner.state = ProcState::RUNNING;
        inner.last_cpu = cpu;
        unsafe {
            (*c).proc.replace(Arc::clone(p));
            swtch(&mut (*c).context, &p.data().context);
            // Process is done running for now.
            // It should have changed its p->state before coming back.
            (*c).proc.take();
        }
    }
}

// Switch to scheduler. Must hold only "proc" lock
// and have changed proc.state. Saves and restores
// intena because intena is a property of this
// kernel thread, not this CPU.
fn sched<'a>(guard: MutexGuard<'a, ProcInner>, ctx: &mut Context) -> MutexGuard<'a, ProcInner> {
    unsafe {
        let c = &mut *CPUS.mycpu();
        assert!(guard.holding(), "sched proc lock");
        assert!(
            c.noff == 1,
            "sched multiple locks {}, {:#?}, {:?}, {:?}, {:?}",
            c.noff,
            *PROCS,
            c.nest,
            guard,
            BCACHE
        );
        assert!(guard.state != ProcState::RUNNING, "sched running");
        assert!(!intr_get(), "sched interruptible");

        let intena = c.intena;
        // to scheduler
        swtch(ctx, &c.context);
        c.intena = intena;

        guard
    }
}

// Give up the CPU for one scheduling round.
pub fn yielding() {
    let p = Cpus::myproc().unwrap();
    let mut guard = p.inner.lock();
    make_runnable(p.idx, &mut guard);
    sched(guard, &mut p.data_mut().context);
}

// Kill + reap all child threads of parent.

pub fn reap_threads(parent: &Arc<Proc>) -> Result<()> {
    let mut parents = PROCS.parents.lock();
    for c in PROCS.pool.iter() {
        if parents[c.idx]
            .as_ref()
            .is_some_and(|pp| Arc::ptr_eq(pp, parent))
            && c.data().is_thread
        {
            let mut g = c.inner.lock();
            g.killed = true;
            if g.state == ProcState::SLEEPING {
                make_runnable(c.idx, &mut g);
            }
        }
    }

    loop {
        let mut havekids = false;
        for c in PROCS.pool.iter() {
            if parents[c.idx]
                .as_ref()
                .is_some_and(|pp| Arc::ptr_eq(pp, parent))
                && c.data().is_thread
            {
                havekids = true;
                let c_guard = c.inner.lock();
                if c_guard.state == ProcState::ZOMBIE {
                    c.free(c_guard);
                    parents[c.idx].take();
                }
            }
        }
        if !havekids {
            return Ok(());
        }
        if parent.inner.lock().killed {
            return Err(Interrupted);
        }
        parents = sleep(Arc::as_ptr(parent) as usize, parents);
    }
}

pub fn exit(status: i32) -> ! {
    let p = Cpus::myproc().unwrap();
    assert!(!Arc::ptr_eq(&p, INITPROC.get().unwrap()), "init exiting");

    if !p.data().is_thread {
        let _ = reap_threads(&p);
    }

    // Close all open files
    let data = p.data_mut();
    for fd in data.ofile.iter_mut() {
        let _file = fd.take();
    }

    LOG.begin_op();
    {
        let _ip = data.cwd.take();
    }
    LOG.end_op();

    let mut proc_guard;
    {
        let mut parents = PROCS.parents.lock();
        // Pass p's abandoned children to init.
        for opp in parents.iter_mut().filter(|pp| pp.is_some()) {
            let is_parent = opp.as_ref().is_some_and(|pp| Arc::ptr_eq(pp, &p));
            if is_parent {
                let initproc = INITPROC.get().unwrap();
                opp.replace(Arc::clone(initproc));
                self::wakeup(Arc::as_ptr(initproc) as usize);
            }
        }
        // Parent might be sleeping in wait().
        self::wakeup(Arc::as_ptr(parents[p.idx].as_ref().unwrap()) as usize);
        proc_guard = p.inner.lock();
        proc_guard.xstate = status;
        proc_guard.state = ProcState::ZOMBIE;
    }

    // jump into scheduler, never to return.
    sched(proc_guard, &mut data.context);

    panic!("zombie exit");
}

// Atomically release lock and sleep on chan.
// Reacquires lock when awakened.
pub fn sleep<T>(chan: usize, mutex_guard: MutexGuard<'_, T>) -> MutexGuard<'_, T> {
    // Must acquire "proc" lock in order to
    // change proc.state and then call sched.
    // Once we hold "proc" lock, we can be
    // guaranteed that we won't miss any wakeup
    // (wakeup() locks "proc" lock),
    // so it's okay to release lock of mutex_guard.
    let mutex;
    {
        let p = Cpus::myproc().unwrap();
        let mut proc_lock = p.inner.lock();
        mutex = Mutex::unlock(mutex_guard);

        proc_lock.chan = chan;
        proc_lock.state = ProcState::SLEEPING;

        // to scheduler
        proc_lock = sched(proc_lock, &mut p.data_mut().context);

        // tidy up
        proc_lock.chan = 0;
    }
    // Reacquires original lock.
    mutex.lock()
}

// Wake up all processes sleeping on chan.
// Must be called without any "proc" lock.
pub fn wakeup(chan: usize) {
    let cur = Cpus::myproc();
    for (idx, p) in PROCS.pool.iter().enumerate() {
        if cur.as_ref().is_some_and(|cp| Arc::ptr_eq(p, cp)) {
            continue;
        }
        let mut guard = p.inner.lock();
        if guard.state == ProcState::SLEEPING && guard.chan == chan {
            make_runnable(idx, &mut guard);
        }
    }
}

pub fn on_tick(now: usize) {
    for (idx, p) in PROCS.pool.iter().enumerate() {
        let mut guard = p.inner.lock();
        if guard.sig_alarm_deadline == 0 {
            continue;
        }
        if guard.state == ProcState::UNUSED
            || guard.state == ProcState::ZOMBIE
            || guard.state == ProcState::STOPPED
        {
            continue;
        }
        if now < guard.sig_alarm_deadline {
            continue;
        }
        guard.sig_pending |= sig_mask(SIGALRM);
        if guard.sig_alarm_interval == 0 {
            guard.sig_alarm_deadline = 0;
        } else if let Some(next) = now.checked_add(guard.sig_alarm_interval) {
            guard.sig_alarm_deadline = next;
        } else {
            guard.sig_alarm_deadline = 0;
        }
        if guard.state == ProcState::SLEEPING {
            make_runnable(idx, &mut guard);
        }
    }
}

pub fn deliver_signals(p: &Arc<Proc>) {
    let data = p.data_mut();
    if data.sig_active {
        return;
    }
    let mut guard = p.inner.lock();
    let pending = guard.sig_pending;
    if pending == 0 {
        return;
    }
    let sig = pending.trailing_zeros() as usize + 1;
    let mask = sig_mask(sig);
    if sig == SIGKILL {
        guard.sig_pending &= !mask;
        guard.killed = true;
        return;
    }
    if sig == SIGCONT {
        guard.cont_pending = false;
    }
    let handler = guard.sig_handlers[sig - 1];
    if handler == SIG_IGN {
        guard.sig_pending &= !mask;
        return;
    }
    if handler == SIG_DFL {
        guard.sig_pending &= !mask;
        match default_action(sig) {
            SigDefaultAction::Terminate => {
                guard.killed = true;
            }
            SigDefaultAction::Stop => {
                guard.stop_sig = sig;
                guard.stop_reported = false;
                guard.state = ProcState::STOPPED;
                let parent = {
                    let parents = PROCS.parents.lock();
                    parents[p.idx].clone()
                };
                if let Some(parent) = parent {
                    wakeup(Arc::as_ptr(&parent) as usize);
                }
                let _guard = sched(guard, &mut data.context);
                return;
            }
            SigDefaultAction::Continue => {
                guard.cont_pending = false;
            }
            SigDefaultAction::Ignore => {}
        }
        return;
    }
    let restorer = data.sig_restorer;
    if restorer == 0 {
        guard.sig_pending &= !mask;
        guard.killed = true;
        return;
    }
    let tf = data.trapframe.as_mut().unwrap();
    data.sig_trapframe = **tf;
    data.sig_active = true;
    guard.sig_pending &= !mask;
    tf.epc = handler;
    tf.a0 = sig;
    tf.ra = restorer;
}

// Create a new process, copying the parent.
// Sets up child kernel stack to return as if from fork() system call.
pub fn fork() -> Result<usize> {
    let p = Cpus::myproc().unwrap();
    let p_data = p.data();
    let (c, mut c_guard) = PROCS.alloc()?;
    let c_data = c.data_mut();

    // Copy user memory from parent to child.
    let p_aspace = p_data.aspace.as_ref().unwrap();
    let mut p_as_inner = p_aspace.inner.lock();
    let p_sz = p_as_inner.sz;
    let p_uvm = p_as_inner.uvm.as_mut().unwrap();

    let c_aspace = c_data.aspace.as_ref().unwrap();
    let mut c_as_inner = c_aspace.inner.lock();
    c_as_inner.sz = p_sz;
    let c_uvm = c_as_inner.uvm.as_mut().unwrap();
    if let Err(err) = p_uvm.copy(c_uvm, p_sz) {
        c.free(c_guard);
        return Err(err);
    }

    // copy saved user registers
    let p_tf = p_data.trapframe.as_ref().unwrap();
    let c_tf = c_data.trapframe.as_mut().unwrap();
    c_tf.clone_from(p_tf);

    // Cause fork to return 0 in the child.
    c_tf.a0 = 0;

    // increment reference counts on open file descriptors.
    c_data.ofile.clone_from_slice(&p_data.ofile);
    c_data.cwd = p_data.cwd.clone();

    c_data.name.push_str(&p_data.name);
    c_data.sig_trapframe = Trapframe::default();
    c_data.sig_active = false;
    c_data.sig_restorer = p_data.sig_restorer;

    {
        let p_inner = p.inner.lock();
        c_guard.sig_handlers = p_inner.sig_handlers;
        c_guard.sig_pending = 0;
        c_guard.sig_alarm_deadline = p_inner.sig_alarm_deadline;
        c_guard.sig_alarm_interval = p_inner.sig_alarm_interval;
        c_guard.pgid = p_inner.pgid;
        c_guard.sid = p_inner.sid;
        c_guard.stop_sig = 0;
        c_guard.stop_reported = false;
        c_guard.cont_pending = false;
    }

    // copy mmap metadata + any already-mapped pages
    c_data.mmap_base = p_data.mmap_base;
    c_data.vmas = p_data.vmas.clone();
    for v in c_data.vmas.iter() {
        let is_shm = v.is_shm();
        let mut va = v.start;
        while va < v.end_pg() {
            if let Some(pte) = p_uvm.walk(va, false)
                && pte.is_v()
                && pte.is_leaf()
                && pte.is_u()
            {
                let pa = pte.to_pa();
                if is_shm {
                    c_uvm.mappages(va, pa, PGSIZE, pte.flags())?;
                    crate::kalloc::page_ref_inc(pa.into_usize());
                } else {
                    let mem = unsafe { Page::try_new_zeroed() }.ok_or(OutOfMemory)?;
                    unsafe {
                        *mem = (*(pa.into_usize() as *mut Page)).clone();
                    }
                    if let Err(err) = c_uvm.mappages(va, (mem as usize).into(), PGSIZE, pte.flags())
                    {
                        unsafe {
                            let _pg = Box::from_raw(mem);
                        }
                        return Err(err);
                    }
                    crate::kalloc::page_ref_init(mem as usize);
                }
            }
            va += PGSIZE;
        }
    }

    let pid = c_guard.pid;

    let c_inner = Mutex::unlock(c_guard);
    {
        let mut parents = PROCS.parents.lock();
        parents[c.idx] = Some(Arc::clone(&p));
    }
    make_runnable(c.idx, &mut c_inner.lock());

    Ok(pid.0)
}

// Create a new thread in the same address space as the caller.

pub fn clone(fcn: usize, arg1: usize, arg2: usize, stack: usize) -> Result<usize> {
    let p = Cpus::myproc().unwrap();
    let p_data = p.data();
    let stack_base: UVAddr = stack.into();
    if stack == 0 || !stack_base.is_aligned() {
        return Err(BadVirtAddr);
    }
    let p_aspace = p_data.aspace.as_ref().unwrap();
    let p_sz = p_aspace.inner.lock().sz;
    if stack.checked_add(PGSIZE).is_none() || stack + PGSIZE > p_sz {
        return Err(BadVirtAddr);
    }
    let (c, mut c_guard) = PROCS.alloc()?;
    let c_data = c.data_mut();

    // Switch child to share parent's address space.
    let _old = c_data.aspace.replace(Arc::clone(p_aspace));

    // Map child's trapframe into the shared user page table.
    {
        let mut as_inner = p_aspace.inner.lock();
        let uvm = as_inner.uvm.as_mut().unwrap();
        uvm.mappages(
            c_data.trapframe_va,
            PAddr::from(c_data.trapframe.as_deref().unwrap() as *const _ as usize),
            PGSIZE,
            PTE_R | PTE_W,
        )?;
    }

    // Start thread at fcn(arg1, arg2).
    let p_tf = p_data.trapframe.as_ref().unwrap();
    let c_tf = c_data.trapframe.as_mut().unwrap();
    c_tf.clone_from(p_tf);
    c_tf.epc = fcn;
    c_tf.sp = stack + PGSIZE;
    c_tf.a0 = arg1;
    c_tf.a1 = arg2;
    c_data.is_thread = true;
    c_data.ustack = stack;
    c_data.ofile.clone_from_slice(&p_data.ofile);
    c_data.cwd = p_data.cwd.clone();
    c_data.name.push_str(&p_data.name);
    c_data.sig_trapframe = Trapframe::default();
    c_data.sig_active = false;
    c_data.sig_restorer = p_data.sig_restorer;

    {
        let p_inner = p.inner.lock();
        c_guard.sig_handlers = p_inner.sig_handlers;
        c_guard.sig_pending = 0;
        c_guard.sig_alarm_deadline = p_inner.sig_alarm_deadline;
        c_guard.sig_alarm_interval = p_inner.sig_alarm_interval;
        c_guard.pgid = p_inner.pgid;
        c_guard.sid = p_inner.sid;
        c_guard.stop_sig = 0;
        c_guard.stop_reported = false;
        c_guard.cont_pending = false;
    }

    let pid = c_guard.pid;
    let c_inner = Mutex::unlock(c_guard);
    {
        let mut parents = PROCS.parents.lock();
        parents[c.idx] = Some(Arc::clone(&p));
    }
    make_runnable(c.idx, &mut c_inner.lock());

    Ok(pid.0)
}

// Wait for a child thread to exit; returns child's pid and writes its stack

// base into addr.

pub fn join(addr: UVAddr) -> Result<usize> {
    let pid;
    let mut havekids;
    let p = Cpus::myproc().unwrap();
    let mut parents = PROCS.parents.lock();

    loop {
        havekids = false;
        for c in PROCS.pool.iter() {
            match parents[c.idx] {
                Some(ref pp) if Arc::ptr_eq(pp, &p) => {
                    if !c.data().is_thread {
                        continue;
                    }
                    let c_guard = c.inner.lock();
                    havekids = true;
                    if c_guard.state == ProcState::ZOMBIE {
                        pid = c_guard.pid.0;
                        let stack = c.data().ustack;
                        {
                            let aspace = p.data().aspace.as_ref().unwrap();
                            let mut as_inner = aspace.inner.lock();
                            as_inner.uvm.as_mut().unwrap().copyout(addr, &stack)?;
                        }
                        c.free(c_guard);
                        parents[c.idx].take();
                        return Ok(pid);
                    }
                }
                _ => continue,
            }
        }

        if !havekids || p.inner.lock().killed {
            break Err(NoChildProcesses);
        }

        parents = sleep(Arc::as_ptr(&p) as usize, parents);
    }
}

// Kill the process with the given pid.
// The victim won't exit until it tries to return
// to user space (see usertrap in trap.rs)
pub fn kill(pid: usize, sig: usize) -> Result<()> {
    if sig == 0 || sig > NSIG {
        return Err(InvalidArgument);
    }
    let mask = sig_mask(sig);
    if mask == 0 {
        return Err(InvalidArgument);
    }
    for (idx, p) in PROCS.pool.iter().enumerate() {
        let mut guard = p.inner.lock();
        if guard.pid.0 == pid {
            if sig == SIGKILL
                || (guard.sig_handlers[sig - 1] == SIG_DFL
                    && default_action(sig) == SigDefaultAction::Terminate)
            {
                guard.killed = true;
            }
            if sig != SIGKILL && guard.sig_handlers[sig - 1] == SIG_IGN {
                return Ok(());
            }
            if sig == SIGCONT {
                guard.stop_sig = 0;
                guard.stop_reported = false;
                guard.cont_pending = true;
            }
            guard.sig_pending |= mask;
            if guard.state == ProcState::STOPPED
                && (sig == SIGCONT
                    || sig == SIGKILL
                    || (guard.sig_handlers[sig - 1] == SIG_DFL
                        && default_action(sig) == SigDefaultAction::Terminate))
            {
                make_runnable(idx, &mut guard);
            }
            if guard.state == ProcState::SLEEPING {
                // Wake process from sleep().
                make_runnable(idx, &mut guard);
            }
            return Ok(());
        }
    }
    Err(NoSuchProcess)
}

pub fn kill_pgrp(pgid: usize, sig: usize) -> Result<()> {
    if pgid == 0 {
        return Err(InvalidArgument);
    }
    if sig == 0 || sig > NSIG {
        return Err(InvalidArgument);
    }
    let mask = sig_mask(sig);
    if mask == 0 {
        return Err(InvalidArgument);
    }
    let mut found = false;
    for (idx, p) in PROCS.pool.iter().enumerate() {
        let mut guard = p.inner.lock();
        if guard.state == ProcState::UNUSED || guard.pgid != pgid {
            continue;
        }
        found = true;
        if sig == SIGKILL
            || (guard.sig_handlers[sig - 1] == SIG_DFL
                && default_action(sig) == SigDefaultAction::Terminate)
        {
            guard.killed = true;
        }
        if sig != SIGKILL && guard.sig_handlers[sig - 1] == SIG_IGN {
            continue;
        }
        if sig == SIGCONT {
            guard.stop_sig = 0;
            guard.stop_reported = false;
            guard.cont_pending = true;
        }
        guard.sig_pending |= mask;
        if guard.state == ProcState::STOPPED
            && (sig == SIGCONT
                || sig == SIGKILL
                || (guard.sig_handlers[sig - 1] == SIG_DFL
                    && default_action(sig) == SigDefaultAction::Terminate))
        {
            make_runnable(idx, &mut guard);
        }
        if guard.state == ProcState::SLEEPING {
            make_runnable(idx, &mut guard);
        }
    }
    if found { Ok(()) } else { Err(NoSuchProcess) }
}

pub fn getpgrp() -> Result<usize> {
    let p = Cpus::myproc().unwrap();
    Ok(p.inner.lock().pgid)
}

pub fn setpgid(pid: usize, pgid: usize) -> Result<()> {
    let p = Cpus::myproc().unwrap();
    let (my_pid, my_sid) = {
        let guard = p.inner.lock();
        (guard.pid.0, guard.sid)
    };
    let target_pid = if pid == 0 { my_pid } else { pid };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };
    if new_pgid == 0 {
        return Err(InvalidArgument);
    }
    let parents = PROCS.parents.lock();
    for (idx, proc) in PROCS.pool.iter().enumerate() {
        let mut guard = proc.inner.lock();
        if guard.pid.0 != target_pid {
            continue;
        }
        if guard.sid != my_sid {
            return Err(PermissionDenied);
        }
        if guard.pid.0 == guard.sid {
            return Err(PermissionDenied);
        }
        if !Arc::ptr_eq(proc, &p) && !parents[idx].as_ref().is_some_and(|pp| Arc::ptr_eq(pp, &p)) {
            return Err(PermissionDenied);
        }
        guard.pgid = new_pgid;
        return Ok(());
    }
    Err(NoSuchProcess)
}

pub fn setsid() -> Result<usize> {
    let p = Cpus::myproc().unwrap();
    let mut guard = p.inner.lock();
    if guard.pid.0 == guard.pgid {
        return Err(PermissionDenied);
    }
    guard.sid = guard.pid.0;
    guard.pgid = guard.pid.0;
    guard.stop_sig = 0;
    guard.stop_reported = false;
    guard.cont_pending = false;
    Ok(guard.sid)
}

pub fn pgid_in_session(pgid: usize, sid: usize) -> bool {
    if pgid == 0 || sid == 0 {
        return false;
    }
    for p in PROCS.pool.iter() {
        let guard = p.inner.lock();
        if guard.state != ProcState::UNUSED && guard.pgid == pgid && guard.sid == sid {
            return true;
        }
    }
    false
}

pub fn sigaction(sig: usize, handler: usize, restorer: usize) -> Result<usize> {
    if sig == 0 || sig > NSIG {
        return Err(InvalidArgument);
    }
    if sig == SIGKILL {
        return Err(PermissionDenied);
    }
    let p = Cpus::myproc().unwrap();
    let mut guard = p.inner.lock();
    let prev = guard.sig_handlers[sig - 1];
    let prev_ret = match prev {
        SIG_DFL => 0,
        SIG_IGN => 1,
        _ => prev,
    };
    if handler != SIG_DFL && handler != SIG_IGN {
        if restorer == 0 {
            return Err(InvalidArgument);
        }
        p.data_mut().sig_restorer = restorer;
    }
    guard.sig_handlers[sig - 1] = handler;
    Ok(prev_ret)
}

pub fn sigreturn() -> Result<()> {
    let p = Cpus::myproc().unwrap();
    let data = p.data_mut();
    if !data.sig_active {
        return Err(InvalidArgument);
    }
    let saved = data.sig_trapframe;
    let tf = data.trapframe.as_mut().unwrap();
    **tf = saved;
    data.sig_active = false;
    Ok(())
}

pub fn setitimer(initial: usize, interval: usize) -> Result<usize> {
    let p = Cpus::myproc().unwrap();
    let now = *TICKS.lock();
    let mut guard = p.inner.lock();
    let prev = if guard.sig_alarm_deadline == 0 {
        0
    } else {
        guard.sig_alarm_deadline.saturating_sub(now)
    };
    if initial == 0 {
        guard.sig_alarm_deadline = 0;
        guard.sig_alarm_interval = 0;
        return Ok(prev);
    }
    let Some(deadline) = now.checked_add(initial) else {
        return Err(InvalidArgument);
    };
    guard.sig_alarm_deadline = deadline;
    guard.sig_alarm_interval = interval;
    Ok(prev)
}

// Wait for a child process to exit and return its pid.
// Return Err, if this process has no children.
pub fn wait(addr: UVAddr) -> Result<usize> {
    waitpid(-1, addr, 0)
}

fn stop_status(sig: usize) -> i32 {
    (((sig & 0xff) << 8) | 0x7f) as i32
}

pub fn waitpid(pid: isize, addr: UVAddr, options: usize) -> Result<usize> {
    let mut havekids;
    let p = Cpus::myproc().unwrap();
    let want_pid = if pid > 0 { Some(pid as usize) } else { None };

    if pid < -1 {
        return Err(InvalidArgument);
    }

    let mut parents = PROCS.parents.lock();

    loop {
        // Scan through table looking for exited children.
        havekids = false;
        for c in PROCS.pool.iter() {
            match parents[c.idx] {
                Some(ref pp) if Arc::ptr_eq(pp, &p) => {
                    if c.data().is_thread {
                        continue;
                    }
                    // make sure the child isn't still in exit() or swtch().
                    let mut c_guard = c.inner.lock();
                    if let Some(want) = want_pid
                        && c_guard.pid.0 != want
                    {
                        continue;
                    }
                    havekids = true;
                    if c_guard.state == ProcState::STOPPED
                        && (options & WUNTRACED) != 0
                        && !c_guard.stop_reported
                    {
                        let pid = c_guard.pid.0;
                        let status = stop_status(c_guard.stop_sig);
                        let aspace = p.data().aspace.as_ref().unwrap();
                        let mut as_inner = aspace.inner.lock();
                        as_inner.uvm.as_mut().unwrap().copyout(addr, &status)?;
                        c_guard.stop_reported = true;
                        return Ok(pid);
                    }
                    if (options & WCONTINUED) != 0 && c_guard.cont_pending {
                        let pid = c_guard.pid.0;
                        let status = 0xffff_i32;
                        let aspace = p.data().aspace.as_ref().unwrap();
                        let mut as_inner = aspace.inner.lock();
                        as_inner.uvm.as_mut().unwrap().copyout(addr, &status)?;
                        c_guard.cont_pending = false;
                        return Ok(pid);
                    }
                    if c_guard.state == ProcState::ZOMBIE {
                        // Found one.
                        let pid = c_guard.pid.0;
                        let aspace = p.data().aspace.as_ref().unwrap();
                        let mut as_inner = aspace.inner.lock();
                        as_inner
                            .uvm
                            .as_mut()
                            .unwrap()
                            .copyout(addr, &c_guard.xstate)?;
                        c.free(c_guard);
                        parents[c.idx].take();
                        return Ok(pid);
                    }
                }
                _ => continue,
            }
        }
        if (options & WNOHANG) != 0 {
            return Ok(0);
        }
        // No point waiting if we don't have any children.
        if !havekids || p.inner.lock().killed {
            break Err(NoChildProcesses);
        }
        {
            let mut guard = p.inner.lock();
            let mut pending = guard.sig_pending;
            if pending != 0 {
                for sig in 1..=NSIG {
                    let mask = sig_mask(sig);
                    if pending & mask == 0 {
                        continue;
                    }
                    let handler = guard.sig_handlers[sig - 1];
                    if handler == SIG_IGN
                        || (handler == SIG_DFL && default_action(sig) == SigDefaultAction::Ignore)
                    {
                        pending &= !mask;
                        guard.sig_pending &= !mask;
                    }
                }
            }
            if pending != 0 {
                return Err(Interrupted);
            }
        }

        // wait for a child to exit
        parents = sleep(Arc::as_ptr(&p) as usize, parents);
    }
}

pub fn grow(n: isize) -> Result<()> {
    use core::cmp::Ordering;
    let p = Cpus::myproc().unwrap();
    let mmap_base = p.data().mmap_base;
    let aspace = p.data().aspace.as_ref().unwrap();
    let mut inner = aspace.inner.lock();
    let mut sz = inner.sz;
    let uvm = inner.uvm.as_mut().unwrap();

    match n.cmp(&0) {
        Ordering::Greater => {
            let newsz = sz + n as usize;
            if newsz >= mmap_base {
                return Err(NoBufferSpace);
            }
            sz = uvm.alloc(sz, newsz, PTE_W)?;
        }
        Ordering::Less => sz = uvm.dealloc(sz, (sz as isize + n) as usize),
        _ => (),
    }
    inner.sz = sz;
    Ok(())
}

// mmap syscalls

pub fn mmap(
    addr: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> Result<usize> {
    if addr != 0 {
        return Err(InvalidArgument);
    }

    if len == 0 {
        return Err(InvalidArgument);
    }

    if !offset.is_multiple_of(PGSIZE) {
        return Err(InvalidArgument);
    }

    let shared = (flags & MAP_SHARED) != 0;
    let private = (flags & MAP_PRIVATE) != 0;
    if shared == private {
        return Err(InvalidArgument);
    }

    let p = Cpus::myproc().unwrap();
    let data = p.data_mut();

    let file = if (flags & MAP_ANON) != 0 {
        None
    } else {
        let f = data
            .ofile
            .get(fd)
            .ok_or(FileDescriptorTooLarge)?
            .as_ref()
            .ok_or(BadFileDescriptor)?
            .clone();

        // basic checks
        if (prot & PROT_READ) != 0 && !f.is_readable() {
            return Err(PermissionDenied);
        }

        if shared && (prot & PROT_WRITE) != 0 && !f.is_writable() {
            return Err(PermissionDenied);
        }

        if f.inode().is_none() {
            return Err(InvalidArgument);
        }

        Some(f)
    };

    let sz = {
        let aspace = data.aspace.as_ref().unwrap();
        aspace.inner.lock().sz
    };
    let start = data.alloc_mmap_va(sz, len)?;

    data.vmas.push(Vma {
        start,
        len,
        prot,
        flags,
        file,
        file_off: offset,
        shm: None,
    });

    Ok(start.into_usize())
}

pub fn munmap(addr: usize, len: usize) -> Result<()> {
    if !addr.is_multiple_of(PGSIZE) || len == 0 {
        return Err(InvalidArgument);
    }

    let p = Cpus::myproc().unwrap();
    let data = p.data_mut();

    let start: UVAddr = addr.into();
    let len_pg = pgroundup(len);
    let end = start + len_pg;

    let mut writebacks = Vec::new();

    // may touch multiple vmas
    let mut i = 0;
    {
        let aspace = data.aspace.as_ref().unwrap();
        let mut as_inner = aspace.inner.lock();
        let uvm = as_inner.uvm.as_mut().unwrap();

        while i < data.vmas.len() {
            let v = data.vmas[i].clone();
            let v_start = v.start;
            let v_end = v.end_pg();

            let ov_start = if start > v_start { start } else { v_start };
            let ov_end = if end < v_end { end } else { v_end };

            if ov_start >= ov_end {
                i += 1;
                continue;
            }

            if v.is_shm() && (ov_start != v_start || ov_end != v_end) {
                return Err(InvalidArgument);
            }

            munmap_vma_range(uvm, &v, ov_start, ov_end - ov_start, &mut writebacks)?;

            // adjust vma
            let unmap_a = ov_start;
            let unmap_b = ov_end;

            if unmap_a == v_start && unmap_b == v_end {
                data.vmas.remove(i);
                continue;
            } else if unmap_a == v_start {
                let delta = unmap_b - v_start;
                data.vmas[i].start = unmap_b;
                data.vmas[i].file_off += delta;
                data.vmas[i].len = data.vmas[i].len.saturating_sub(delta);
            } else if unmap_b == v_end {
                let delta = v_end - unmap_a;
                data.vmas[i].len = data.vmas[i].len.saturating_sub(delta);
            } else {
                // split
                let left_len = unmap_a - v_start;
                let right_off = unmap_b - v_start;
                let right_len = v_end - unmap_b;

                let mut right = data.vmas[i].clone();
                right.start = unmap_b;
                right.file_off += right_off;
                right.len = core::cmp::min(right.len.saturating_sub(right_off), right_len);

                data.vmas[i].len = core::cmp::min(data.vmas[i].len, left_len);
                data.vmas.insert(i + 1, right);
                i += 1;
            }

            i += 1;
        }
    }

    for wb in writebacks {
        wb.flush()?;
    }

    Ok(())
}

pub(crate) struct Writeback {
    ip: Inode,
    file_off: u32,
    data: Box<Page>,
    data_off: usize,
    len: usize,
}

impl Writeback {
    pub(crate) fn flush(self) -> Result<()> {
        LOG.begin_op();
        {
            let mut guard = self.ip.lock();
            let base = self.data.as_ref() as *const Page as usize;
            let src = VirtAddr::Kernel(base + self.data_off);
            let _ = guard.write(src, self.file_off, self.len)?;
        }
        LOG.end_op();
        Ok(())
    }
}

fn munmap_vma_range(
    uvm: &mut Uvm,
    v: &Vma,
    start: UVAddr,
    len_pg: usize,
    writebacks: &mut Vec<Writeback>,
) -> Result<()> {
    let mut a = start;
    let end = start + len_pg;

    while a < end {
        // only touch mapped pages
        if let Some(pte) = uvm.walk(a, false)
            && pte.is_v()
            && pte.is_leaf()
        {
            // writeback for shared, file-backed
            if v.is_shared()
                && !v.is_anon()
                && let Some(f) = v.file.as_ref()
                && let Some(ip) = f.inode()
            {
                let page_off = a - v.start;
                let write_start = core::cmp::max(a.into_usize(), v.start.into_usize());
                let write_end = core::cmp::min(a.into_usize() + PGSIZE, v.end_req().into_usize());

                if write_end > write_start {
                    let n = write_end - write_start;
                    let file_off = v.file_off + page_off + (write_start - a.into_usize());

                    if file_off > u32::MAX as usize {
                        return Err(InvalidArgument);
                    }

                    let mut buf = match Box::<Page>::try_new_zeroed() {
                        Ok(mem) => unsafe { mem.assume_init() },
                        Err(_) => return Err(OutOfMemory),
                    };
                    let pa = pte.to_pa().into_usize();
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            pa as *const u8,
                            buf.as_mut() as *mut Page as *mut u8,
                            PGSIZE,
                        );
                    }

                    writebacks.push(Writeback {
                        ip,
                        file_off: file_off as u32,
                        data: buf,
                        data_off: write_start - a.into_usize(),
                        len: n,
                    });
                }
            }

            uvm.unmap(a, 1, true);
        }

        a += PGSIZE;
    }

    Ok(())
}

pub fn handle_user_page_fault(fault_addr: usize, cause: Exception) -> Result<()> {
    let p = Cpus::myproc().unwrap();
    let data = p.data_mut();

    let mut va: UVAddr = fault_addr.into();
    va.rounddown();

    let idx = data
        .vmas
        .iter()
        .position(|v| v.contains_pg(va))
        .ok_or(BadVirtAddr)?;

    let v = data.vmas[idx].clone();

    // permission check based on fault type
    match cause {
        Exception::LoadPageFault if (v.prot & PROT_READ) == 0 => return Err(PermissionDenied),
        Exception::StorePageFault if (v.prot & PROT_WRITE) == 0 => {
            return Err(PermissionDenied);
        }
        Exception::InstructionPageFault if (v.prot & PROT_EXEC) == 0 => {
            return Err(PermissionDenied);
        }
        _ => {}
    }

    let aspace = data.aspace.as_ref().unwrap();
    {
        let mut as_inner = aspace.inner.lock();
        let uvm = as_inner.uvm.as_mut().unwrap();

        // already mapped?
        if let Some(pte) = uvm.walk(va, false)
            && pte.is_v()
            && pte.is_leaf()
            && pte.is_u()
        {
            return Err(BadVirtAddr);
        }
    }

    if let Some(shm) = v.shm.as_ref() {
        let page_idx = (va - v.start) / PGSIZE;
        let pa = shm.page_pa(page_idx)?;
        let mut as_inner = aspace.inner.lock();
        let uvm = as_inner.uvm.as_mut().unwrap();
        uvm.mappages(va, pa.into(), PGSIZE, v.perm())?;
        crate::kalloc::page_ref_inc(pa);
        return Ok(());
    }

    let mem = unsafe { Page::try_new_zeroed() }.ok_or(OutOfMemory)?;

    if !v.is_anon()
        && let Some(f) = v.file.as_ref()
        && let Some(ip) = f.inode()
    {
        let page_off = va - v.start;
        let file_off = v.file_off + page_off;

        if file_off <= u32::MAX as usize {
            let mut guard = ip.lock();
            if let Err(err) = guard.read(VirtAddr::Kernel(mem as usize), file_off as u32, PGSIZE) {
                unsafe {
                    let _pg = Box::from_raw(mem);
                }
                return Err(err);
            }
        }
    }

    let mut as_inner = aspace.inner.lock();
    let uvm = as_inner.uvm.as_mut().unwrap();

    if let Some(pte) = uvm.walk(va, false)
        && pte.is_v()
        && pte.is_leaf()
        && pte.is_u()
    {
        unsafe {
            let _pg = Box::from_raw(mem);
        }
        return Ok(());
    }

    if let Err(err) = uvm.mappages(va, (mem as usize).into(), PGSIZE, v.perm()) {
        unsafe {
            let _pg = Box::from_raw(mem);
        }
        return Err(err);
    }
    crate::kalloc::page_ref_init(mem as usize);

    Ok(())
}
