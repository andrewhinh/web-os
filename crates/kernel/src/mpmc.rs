use alloc::{collections::LinkedList, sync::Arc};
use core::fmt::Debug;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::condvar::Condvar;
use crate::error::{Error::NotConnected, Error::WouldBlock, Result};
use crate::semaphore::Semaphore;
use crate::spinlock::Mutex;

#[derive(Debug)]
pub struct SyncSender<T: Debug> {
    sem: Arc<Semaphore>, // count receiver
    buf: Arc<Mutex<LinkedList<T>>>,
    cond: Arc<Condvar>, // cont sender
    scnt: Arc<AtomicUsize>,
    rcnt: Arc<AtomicUsize>,
}
unsafe impl<T: Send + Debug> Send for SyncSender<T> {}

impl<T: Debug> Clone for SyncSender<T> {
    fn clone(&self) -> Self {
        self.scnt.fetch_add(1, Ordering::Relaxed);
        Self {
            sem: Arc::clone(&self.sem),
            buf: Arc::clone(&self.buf),
            cond: Arc::clone(&self.cond),
            scnt: Arc::clone(&self.scnt),
            rcnt: Arc::clone(&self.rcnt),
        }
    }
}

impl<T: Send + Debug> SyncSender<T> {
    pub fn send(&self, data: T) -> Result<()> {
        self.sem.wait()?;
        let mut buf = self.buf.lock();
        buf.push_back(data);
        self.cond.notify_all();
        Ok(())
    }

    pub fn try_send(&self, data: T) -> Result<()> {
        if !self.sem.try_wait()? {
            return Err(WouldBlock);
        }
        let mut buf = self.buf.lock();
        buf.push_back(data);
        self.cond.notify_all();
        Ok(())
    }

    pub fn can_send(&self) -> Result<bool> {
        self.sem.can_wait()
    }

    pub fn is_closed(&self) -> bool {
        self.rcnt.load(Ordering::Relaxed) == 0
    }
}

#[derive(Debug)]
pub struct Receiver<T: Debug> {
    sem: Arc<Semaphore>,
    buf: Arc<Mutex<LinkedList<T>>>,
    cond: Arc<Condvar>,
    scnt: Arc<AtomicUsize>,
    rcnt: Arc<AtomicUsize>,
}
unsafe impl<T: Send + Debug> Send for Receiver<T> {}

impl<T: Debug> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.rcnt.fetch_add(1, Ordering::Relaxed);
        Self {
            sem: Arc::clone(&self.sem),
            buf: Arc::clone(&self.buf),
            cond: Arc::clone(&self.cond),
            scnt: Arc::clone(&self.scnt),
            rcnt: Arc::clone(&self.rcnt),
        }
    }
}

impl<T: Debug> Receiver<T> {
    pub fn recv(&self) -> Result<T> {
        let mut buf = self.buf.lock();
        loop {
            if let Some(data) = buf.pop_front() {
                self.sem.post()?;
                break Ok(data);
            }
            if self.scnt.load(Ordering::Relaxed) > 0 {
                buf = self.cond.wait(buf);
            } else {
                break Err(NotConnected);
            }
        }
    }

    pub fn try_recv(&self) -> Result<T> {
        let mut buf = self.buf.lock();
        if let Some(data) = buf.pop_front() {
            self.sem.post()?;
            return Ok(data);
        }
        if self.scnt.load(Ordering::Relaxed) > 0 {
            Err(WouldBlock)
        } else {
            Err(NotConnected)
        }
    }

    pub fn has_data(&self) -> bool {
        !self.buf.lock().is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.scnt.load(Ordering::Relaxed) == 0
    }
}

pub fn sync_channel<T: Debug>(max: isize, name: &'static str) -> (SyncSender<T>, Receiver<T>) {
    let sem = Arc::new(Semaphore::new(max, name));
    let buf = Arc::new(Mutex::new(LinkedList::new(), name));
    let cond = Arc::new(Condvar::new());
    let scnt = Arc::new(AtomicUsize::new(1));
    let rcnt = Arc::new(AtomicUsize::new(1));
    let tx = SyncSender {
        sem: Arc::clone(&sem),
        buf: Arc::clone(&buf),
        cond: Arc::clone(&cond),
        scnt: Arc::clone(&scnt),
        rcnt: Arc::clone(&rcnt),
    };
    let rx = Receiver {
        sem,
        buf,
        cond,
        scnt,
        rcnt,
    };
    (tx, rx)
}

impl<T: Debug> Drop for SyncSender<T> {
    fn drop(&mut self) {
        self.scnt.fetch_sub(1, Ordering::Relaxed);
        self.cond.notify_all();
    }
}

impl<T: Debug> Drop for Receiver<T> {
    fn drop(&mut self) {
        let cnt = self.rcnt.fetch_sub(1, Ordering::Relaxed);
        if cnt == 1 {
            self.sem.close();
        }
    }
}
