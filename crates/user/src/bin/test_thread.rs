#![no_std]
extern crate alloc;

use ulib::{mutex::Mutex, println, sys, thread};

static COUNT: Mutex<usize> = Mutex::new(0);

extern "C" fn worker(iters: usize, _unused: usize) {
    for _ in 0..iters {
        let mut g = COUNT.lock();
        *g += 1;
    }
}

fn main() {
    let nthreads = 4usize;
    let iters = 1000usize;

    for _ in 0..nthreads {
        thread::thread_create(worker, iters, 0).unwrap();
    }
    for _ in 0..nthreads {
        let _ = thread::thread_join().unwrap();
    }

    let final_count = *COUNT.lock();
    println!("test_thread: count={}", final_count);
    if final_count != nthreads * iters {
        println!("test_thread: FAIL");
        sys::exit(1);
    }
    println!("test_thread: OK");
}
