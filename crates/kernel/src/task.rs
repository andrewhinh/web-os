use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, task::Wake, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use crate::{array, param::NCPU, proc::Cpus, spinlock::Mutex, sync::OnceLock};

pub struct Task {
    pub(crate) id: TaskId,
    future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
}

impl Task {
    pub fn new(future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            id: TaskId::new(),
            future: Box::pin(future),
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.future.as_mut().poll(cx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TaskId(pub(crate) u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

const READYQ_SIZE: usize = 256;

#[derive(Debug)]
struct ReadyQueue {
    buf: [u64; READYQ_SIZE],
    head: usize,
    tail: usize,
    len: usize,
}

impl ReadyQueue {
    const fn new() -> Self {
        Self {
            buf: [0; READYQ_SIZE],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, id: TaskId) -> bool {
        if self.len == READYQ_SIZE {
            return false;
        }
        self.buf[self.tail] = id.0;
        self.tail = (self.tail + 1) % READYQ_SIZE;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<TaskId> {
        if self.len == 0 {
            return None;
        }
        let raw = self.buf[self.head];
        self.head = (self.head + 1) % READYQ_SIZE;
        self.len -= 1;
        Some(TaskId(raw))
    }
}

static READY: [Mutex<ReadyQueue>; NCPU] = array![Mutex::new(ReadyQueue::new(), "kready"); NCPU];
static EXEC: [OnceLock<Mutex<ExecutorState>>; NCPU] = array![OnceLock::new(); NCPU];
static POLLS: [AtomicUsize; NCPU] = array![AtomicUsize::new(0); NCPU];

struct ExecutorState {
    tasks: BTreeMap<TaskId, Task>,
    waker_cache: BTreeMap<TaskId, Waker>,
}

impl ExecutorState {
    fn new() -> Self {
        Self {
            tasks: BTreeMap::new(),
            waker_cache: BTreeMap::new(),
        }
    }

    fn insert_task(&mut self, task: Task) {
        let task_id = task.id;
        if self.tasks.insert(task_id, task).is_some() {
            panic!("task with same ID already in tasks");
        }
    }
}

struct TaskWaker {
    cpu: usize,
    task_id: TaskId,
}

impl TaskWaker {
    fn waker(cpu: usize, task_id: TaskId) -> Waker {
        Waker::from(Arc::new(TaskWaker { cpu, task_id }))
    }

    fn wake_task(&self) {
        if !READY[self.cpu].lock().push(self.task_id) {
            panic!("kready full");
        }
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task();
    }
}

#[inline]
fn this_cpu_id() -> usize {
    let _intr = Cpus::lock_mycpu("kcpu");
    unsafe { Cpus::cpu_id() }
}

pub fn init_cpu(cpu: usize) {
    assert!(cpu < NCPU, "bad cpu id");
    if EXEC[cpu]
        .set(Mutex::new(ExecutorState::new(), "kexec"))
        .is_err()
    {
        panic!("kexec already inited");
    }
    if TICK_WAKERS[cpu]
        .set(Mutex::new(Vec::new(), "tick_wakers"))
        .is_err()
    {
        panic!("tick_wakers already inited");
    }
}

pub fn spawn_local(task: Task) {
    spawn_on(this_cpu_id(), task);
}

pub fn spawn_on(cpu: usize, task: Task) {
    assert!(cpu < NCPU, "bad cpu id");
    let exec = EXEC[cpu].get().expect("task executor not initialized");
    let id = task.id;
    exec.lock().insert_task(task);
    if !READY[cpu].lock().push(id) {
        panic!("kready full");
    }
}

pub fn run_ready_tasks() {
    run_ready_tasks_cpu(this_cpu_id(), 0);
}

pub fn ready_is_empty_cpu(cpu: usize) -> bool {
    assert!(cpu < NCPU, "bad cpu id");
    READY[cpu].lock().is_empty()
}

pub fn run_ready_tasks_cpu(cpu: usize, budget: usize) {
    assert!(cpu < NCPU, "bad cpu id");
    let exec = EXEC[cpu].get().expect("kexec not inited");

    let mut ran = 0usize;
    loop {
        if budget != 0 && ran >= budget {
            break;
        }
        let Some(id) = READY[cpu].lock().pop() else {
            break;
        };
        POLLS[cpu].fetch_add(1, Ordering::Relaxed);

        let (mut task, waker) = {
            let mut st = exec.lock();
            let Some(task) = st.tasks.remove(&id) else {
                continue;
            };
            let w = st
                .waker_cache
                .entry(id)
                .or_insert_with(|| TaskWaker::waker(cpu, id))
                .clone();
            (task, w)
        };

        let mut cx = Context::from_waker(&waker);
        match task.poll(&mut cx) {
            Poll::Ready(()) => {
                exec.lock().waker_cache.remove(&id);
            }
            Poll::Pending => {
                exec.lock().tasks.insert(id, task);
            }
        }
        ran += 1;
    }
}

static TICK_SEQ: [AtomicUsize; NCPU] = array![AtomicUsize::new(0); NCPU];
static TICK_WAKERS: [OnceLock<Mutex<Vec<Waker>>>; NCPU] = array![OnceLock::new(); NCPU];

pub fn on_tick_cpu(cpu: usize) {
    assert!(cpu < NCPU, "bad cpu id");
    TICK_SEQ[cpu].fetch_add(1, Ordering::Release);
    let q = TICK_WAKERS[cpu].get().expect("tick_wakers not inited");
    let mut wakers = q.lock();
    for w in wakers.iter() {
        w.wake_by_ref();
    }
    wakers.clear();
}

pub fn sleep_ticks(n: usize) -> SleepTicks {
    let cpu = this_cpu_id();
    let now = TICK_SEQ[cpu].load(Ordering::Acquire);
    SleepTicks {
        cpu,
        target: now + n,
    }
}

pub struct SleepTicks {
    cpu: usize,
    target: usize,
}

impl Future for SleepTicks {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let cpu = self.cpu;
        let now = TICK_SEQ[cpu].load(Ordering::Acquire);
        if now >= self.target {
            return Poll::Ready(());
        }

        let q = TICK_WAKERS[cpu]
            .get()
            .expect("tick waker list not initialized; call task::init_cpu()");
        q.lock().push(cx.waker().clone());

        let now2 = TICK_SEQ[cpu].load(Ordering::Acquire);
        if now2 >= self.target {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

pub fn poll_count_total() -> usize {
    let mut sum = 0usize;
    for poll in POLLS.iter().take(NCPU) {
        sum = sum.wrapping_add(poll.load(Ordering::Relaxed));
    }
    sum
}
