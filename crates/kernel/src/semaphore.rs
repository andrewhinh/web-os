use crate::{
    condvar::Condvar,
    error::{Error::Interrupted, Error::InvalidArgument, Result},
    proc::Cpus,
    spinlock::Mutex,
};

#[derive(Debug)]
pub struct Semaphore {
    mutex: Mutex<SemState>,
    cond: Condvar,
    max: isize,
}

#[derive(Debug)]
struct SemState {
    count: isize,
    closed: bool,
}

impl Semaphore {
    pub fn new(max: isize, name: &'static str) -> Self {
        debug_assert!(max >= 0);
        Self {
            mutex: Mutex::new(
                SemState {
                    count: max,
                    closed: false,
                },
                name,
            ),
            cond: Condvar::new(),
            max,
        }
    }

    pub fn new_with_count(count: isize, max: isize, name: &'static str) -> Result<Self> {
        if max < 0 || count < 0 || count > max {
            return Err(InvalidArgument);
        }
        Ok(Self {
            mutex: Mutex::new(
                SemState {
                    count,
                    closed: false,
                },
                name,
            ),
            cond: Condvar::new(),
            max,
        })
    }

    pub fn wait(&self) -> Result<()> {
        let mut state = self.mutex.lock();
        loop {
            if let Some(p) = Cpus::myproc()
                && p.inner.lock().killed
            {
                return Err(Interrupted);
            }
            if state.closed {
                return Err(InvalidArgument);
            }
            if state.count == 0 {
                state = self.cond.wait(state);
                continue;
            }
            state.count -= 1;
            break;
        }
        Ok(())
    }

    pub fn try_wait(&self) -> Result<bool> {
        let mut state = self.mutex.lock();
        if state.closed {
            return Err(InvalidArgument);
        }
        if state.count == 0 {
            return Ok(false);
        }
        state.count -= 1;
        Ok(true)
    }

    pub fn can_wait(&self) -> Result<bool> {
        let state = self.mutex.lock();
        if state.closed {
            return Err(InvalidArgument);
        }
        Ok(state.count > 0)
    }

    pub fn post(&self) -> Result<()> {
        let mut state = self.mutex.lock();
        if state.closed {
            return Err(InvalidArgument);
        }
        if state.count == self.max {
            return Err(InvalidArgument);
        }
        state.count += 1;
        self.cond.notify_all();
        Ok(())
    }

    pub fn close(&self) {
        let mut state = self.mutex.lock();
        state.closed = true;
        self.cond.notify_all();
    }
}
