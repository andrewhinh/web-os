use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    array,
    param::{NCPU, NPROC},
    proc::Cpus,
    spinlock::Mutex,
};

#[derive(Debug)]
pub struct RunQueue {
    buf: [usize; NPROC],
    head: usize,
    tail: usize,
    len: usize,
}

impl RunQueue {
    pub const fn new() -> Self {
        Self {
            buf: [0; NPROC],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn push(&mut self, idx: usize) {
        assert!(idx < NPROC, "runq push bad idx");
        assert!(self.len < NPROC, "runq full");
        self.buf[self.tail] = idx;
        self.tail = (self.tail + 1) % NPROC;
        self.len += 1;
    }

    #[inline]
    pub fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let idx = self.buf[self.head];
        self.head = (self.head + 1) % NPROC;
        self.len -= 1;
        Some(idx)
    }
}

impl Default for RunQueue {
    fn default() -> Self {
        Self::new()
    }
}

pub static RUNQ: Mutex<RunQueue> = Mutex::new(RunQueue::new(), "runq");

#[inline]
pub fn runq_push(idx: usize) {
    runq_push_local(idx);
}

#[inline]
pub fn runq_pop() -> Option<usize> {
    let cpu = this_cpu_id();
    runq_pop_or_steal(cpu)
}

#[inline]
pub fn runq_is_empty() -> bool {
    let cpu = this_cpu_id();
    RUNQS[cpu].lock().is_empty()
}

pub static RUNQS: [Mutex<RunQueue>; NCPU] = array![Mutex::new(RunQueue::new(), "runq"); NCPU];

static STEAL_START: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn this_cpu_id() -> usize {
    let _intr = Cpus::lock_mycpu("runq_cpu");
    unsafe { Cpus::cpu_id() }
}

#[inline]
pub fn runq_push_cpu(cpu: usize, idx: usize) {
    assert!(cpu < NCPU, "bad cpu id");
    RUNQS[cpu].lock().push(idx);
}

#[inline]
pub fn runq_push_local(idx: usize) {
    let cpu = this_cpu_id();
    runq_push_cpu(cpu, idx);
}

#[inline]
pub fn runq_pop_local(cpu: usize) -> Option<usize> {
    assert!(cpu < NCPU, "bad cpu id");
    RUNQS[cpu].lock().pop()
}

pub fn runq_pop_or_steal(cpu: usize) -> Option<usize> {
    if let Some(idx) = runq_pop_local(cpu) {
        return Some(idx);
    }

    let start = STEAL_START.fetch_add(1, Ordering::Relaxed) % NCPU;
    for off in 0..NCPU {
        let victim = (start + off) % NCPU;
        if victim == cpu {
            continue;
        }
        if let Some(idx) = runq_pop_local(victim) {
            return Some(idx);
        }
    }
    None
}
