use alloc::{boxed::Box, sync::Arc, vec::Vec};

use crate::array;
use crate::error::{Error::*, Result};
use crate::kalloc;
use crate::mmap::{MAP_ANON, MAP_SHARED, PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::param::{NSEM, NSHM};
use crate::proc::{Cpus, Vma};
use crate::riscv::{PGSIZE, pgroundup, pteflags::*};
use crate::semaphore::Semaphore;
use crate::spinlock::Mutex;
use crate::sync::LazyLock;
use crate::vm::{Addr, Page, UVAddr};

const SHM_MAX_PAGES: usize = 64;

static SHM_TABLE: LazyLock<Mutex<[Option<Arc<ShmSegment>>; NSHM]>> =
    LazyLock::new(|| Mutex::new(array![None; NSHM], "shm"));
static SEM_TABLE: LazyLock<Mutex<[Option<Arc<Semaphore>>; NSEM]>> =
    LazyLock::new(|| Mutex::new(array![None; NSEM], "sem"));

#[derive(Debug)]
pub struct ShmSegment {
    size: usize,
    pages: Vec<usize>,
}

impl ShmSegment {
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn len_pg(&self) -> usize {
        pgroundup(self.size)
    }

    pub fn page_pa(&self, idx: usize) -> Result<usize> {
        self.pages.get(idx).copied().ok_or(InvalidArgument)
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        for &pa in &self.pages {
            let new = kalloc::page_ref_dec(pa);
            if new == 0 {
                unsafe {
                    let _pg = Box::from_raw(pa as *mut Page);
                }
            }
        }
    }
}

fn prot_to_perm(prot: usize) -> Result<usize> {
    let valid = PROT_READ | PROT_WRITE | PROT_EXEC;
    if (prot & !valid) != 0 {
        return Err(InvalidArgument);
    }
    if (prot & valid) == 0 {
        return Err(InvalidArgument);
    }
    let mut perm = PTE_U;
    if (prot & PROT_READ) != 0 {
        perm |= PTE_R;
    }
    if (prot & PROT_WRITE) != 0 {
        perm |= PTE_W;
    }
    if (prot & PROT_EXEC) != 0 {
        perm |= PTE_X;
    }
    Ok(perm)
}

pub fn shm_create(size: usize) -> Result<usize> {
    if size == 0 {
        return Err(InvalidArgument);
    }
    let len_pg = pgroundup(size);
    let npages = len_pg / PGSIZE;
    if npages == 0 || npages > SHM_MAX_PAGES {
        return Err(InvalidArgument);
    }

    let mut pages = Vec::with_capacity(npages);
    for _ in 0..npages {
        let mem = match Box::<Page>::try_new_zeroed() {
            Ok(mem) => Box::into_raw(unsafe { mem.assume_init() }),
            Err(_) => {
                drop(ShmSegment { size, pages });
                return Err(OutOfMemory);
            }
        };
        let pa = mem as usize;
        kalloc::page_ref_init(pa);
        pages.push(pa);
    }

    let seg = Arc::new(ShmSegment { size, pages });
    let mut table = SHM_TABLE.lock();
    for (idx, slot) in table.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(seg);
            return Ok(idx + 1);
        }
    }
    Err(NoBufferSpace)
}

pub fn shm_attach(id: usize, prot: usize) -> Result<usize> {
    let perm = prot_to_perm(prot)?;
    let seg = {
        let table = SHM_TABLE.lock();
        let idx = id.checked_sub(1).ok_or(InvalidArgument)?;
        table
            .get(idx)
            .and_then(|slot| slot.as_ref().map(Arc::clone))
            .ok_or(NotFound)?
    };

    let p = Cpus::myproc().unwrap();
    let data = p.data_mut();
    let prev_base = data.mmap_base;
    let sz = {
        let aspace = data.aspace.as_ref().unwrap();
        aspace.inner.lock().sz
    };
    let len = seg.size();
    let start = data.alloc_mmap_va(sz, len)?;

    let aspace = data.aspace.as_ref().unwrap();
    let mut as_inner = aspace.inner.lock();
    let uvm = as_inner.uvm.as_mut().unwrap();

    for (idx, &pa) in seg.pages.iter().enumerate() {
        let va = start + idx * PGSIZE;
        if let Err(err) = uvm.mappages(va, pa.into(), PGSIZE, perm) {
            if idx > 0 {
                uvm.unmap(start, idx, true);
            }
            data.mmap_base = prev_base;
            return Err(err);
        }
        kalloc::page_ref_inc(pa);
    }

    data.vmas.push(Vma {
        start,
        len,
        prot,
        flags: MAP_SHARED | MAP_ANON,
        file: None,
        file_off: 0,
        shm: Some(seg),
    });

    Ok(start.into_usize())
}

pub fn shm_detach(addr: usize) -> Result<()> {
    if !addr.is_multiple_of(PGSIZE) {
        return Err(InvalidArgument);
    }
    let p = Cpus::myproc().unwrap();
    let data = p.data_mut();
    let start: UVAddr = addr.into();
    let len = data
        .vmas
        .iter()
        .find(|v| v.shm.is_some() && v.start == start)
        .map(|v| v.len)
        .ok_or(BadVirtAddr)?;
    crate::proc::munmap(addr, len)
}

pub fn shm_destroy(id: usize) -> Result<()> {
    let mut table = SHM_TABLE.lock();
    let idx = id.checked_sub(1).ok_or(InvalidArgument)?;
    let slot = table.get_mut(idx).ok_or(InvalidArgument)?;
    if let Some(seg) = slot.take() {
        drop(seg);
        Ok(())
    } else {
        Err(NotFound)
    }
}

pub fn sem_create(value: usize) -> Result<usize> {
    let count = isize::try_from(value).map_err(|_| InvalidArgument)?;
    let sem = Arc::new(Semaphore::new_with_count(count, isize::MAX, "sem")?);
    let mut table = SEM_TABLE.lock();
    for (idx, slot) in table.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(sem);
            return Ok(idx + 1);
        }
    }
    Err(NoBufferSpace)
}

pub fn sem_wait(id: usize) -> Result<()> {
    let sem = sem_get(id)?;
    sem.wait()
}

pub fn sem_try_wait(id: usize) -> Result<usize> {
    let sem = sem_get(id)?;
    sem.try_wait().map(|ok| ok as usize)
}

pub fn sem_post(id: usize) -> Result<()> {
    let sem = sem_get(id)?;
    sem.post()
}

pub fn sem_close(id: usize) -> Result<()> {
    let mut table = SEM_TABLE.lock();
    let idx = id.checked_sub(1).ok_or(InvalidArgument)?;
    let slot = table.get_mut(idx).ok_or(InvalidArgument)?;
    if let Some(sem) = slot.take() {
        sem.close();
        Ok(())
    } else {
        Err(NotFound)
    }
}

fn sem_get(id: usize) -> Result<Arc<Semaphore>> {
    let table = SEM_TABLE.lock();
    let idx = id.checked_sub(1).ok_or(InvalidArgument)?;
    table
        .get(idx)
        .and_then(|slot| slot.as_ref().map(Arc::clone))
        .ok_or(NotFound)
}
