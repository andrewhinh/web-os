pub use kernel::mmap::{PROT_EXEC, PROT_READ, PROT_WRITE};

use crate::sys;

pub fn shm_create(size: usize) -> sys::Result<usize> {
    sys::shmcreate(size)
}

pub fn shm_attach(id: usize, prot: usize) -> sys::Result<*mut u8> {
    sys::shmattach(id, prot).map(|addr| addr as *mut u8)
}

pub fn shm_detach(addr: *mut u8) -> sys::Result<()> {
    sys::shmdetach(addr as usize)
}

pub fn shm_destroy(id: usize) -> sys::Result<()> {
    sys::shmdestroy(id)
}

pub fn sem_create(value: usize) -> sys::Result<usize> {
    sys::semcreate(value)
}

pub fn sem_wait(id: usize) -> sys::Result<()> {
    sys::semwait(id)
}

pub fn sem_try_wait(id: usize) -> sys::Result<bool> {
    sys::semtrywait(id).map(|v| v != 0)
}

pub fn sem_post(id: usize) -> sys::Result<()> {
    sys::sempost(id)
}

pub fn sem_close(id: usize) -> sys::Result<()> {
    sys::semclose(id)
}
