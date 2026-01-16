#![no_std]

use kernel::mmap::{PROT_READ, PROT_WRITE};
use ulib::{eprintln, ipc, println, sys};

const PGSIZE: usize = 4096;

fn main() {
    println!("test_ipc: shm + sem");

    let shm_id = match ipc::shm_create(PGSIZE) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("test_ipc: shm_create failed err={}", e);
            return;
        }
    };
    let sem_ready = match ipc::sem_create(0) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("test_ipc: sem_create ready failed err={}", e);
            let _ = ipc::shm_destroy(shm_id);
            return;
        }
    };
    let sem_done = match ipc::sem_create(0) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("test_ipc: sem_create done failed err={}", e);
            let _ = ipc::sem_close(sem_ready);
            let _ = ipc::shm_destroy(shm_id);
            return;
        }
    };

    let pid = match sys::fork() {
        Ok(pid) => pid,
        Err(e) => {
            eprintln!("test_ipc: fork failed err={}", e);
            let _ = ipc::sem_close(sem_ready);
            let _ = ipc::sem_close(sem_done);
            let _ = ipc::shm_destroy(shm_id);
            return;
        }
    };

    if pid == 0 {
        let addr = match ipc::shm_attach(shm_id, PROT_READ | PROT_WRITE) {
            Ok(addr) => addr,
            Err(e) => {
                eprintln!("test_ipc: child shm_attach failed err={}", e);
                sys::exit(1);
            }
        };
        if let Err(e) = ipc::sem_wait(sem_ready) {
            eprintln!("test_ipc: child sem_wait failed err={}", e);
            let _ = ipc::shm_detach(addr);
            sys::exit(1);
        }
        unsafe {
            let p = addr as *mut u64;
            let v = *p;
            *p = v + 1;
        }
        let _ = ipc::sem_post(sem_done);
        let _ = ipc::shm_detach(addr);
        sys::exit(0);
    }

    let addr = match ipc::shm_attach(shm_id, PROT_READ | PROT_WRITE) {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("test_ipc: parent shm_attach failed err={}", e);
            let _ = ipc::sem_close(sem_ready);
            let _ = ipc::sem_close(sem_done);
            let _ = ipc::shm_destroy(shm_id);
            return;
        }
    };

    unsafe {
        *(addr as *mut u64) = 41;
    }
    let _ = ipc::sem_post(sem_ready);
    if let Err(e) = ipc::sem_wait(sem_done) {
        eprintln!("test_ipc: parent sem_wait failed err={}", e);
    }
    unsafe {
        let v = *(addr as *mut u64);
        println!("test_ipc: value={}", v);
    }

    let _ = ipc::shm_detach(addr);
    let _ = ipc::shm_destroy(shm_id);
    let _ = ipc::sem_close(sem_ready);
    let _ = ipc::sem_close(sem_done);
    let mut status: i32 = 0;
    let _ = sys::wait(&mut status);
}
