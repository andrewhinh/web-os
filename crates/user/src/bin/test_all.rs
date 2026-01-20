#![no_std]

use ulib::{eprintln, println, process::Command, sys};

const TESTS: [&str; 16] = [
    "test_aplic",
    "test_cow",
    "test_disk",
    "test_fcntl",
    "test_fsync",
    "test_ipc",
    "test_ktask",
    "test_mmap",
    "test_net",
    "test_reverse",
    "test_pdual",
    "test_psort",
    "test_pzip",
    "test_poll",
    "test_signal",
    "test_thread",
];

fn run_test(name: &str) -> bool {
    println!("test_all: run {}", name);
    let mut cmd = Command::new(name);
    match cmd.status() {
        Ok(status) if status.0 == 0 => {
            println!("test_all: ok {}", name);
            true
        }
        Ok(status) => {
            eprintln!("test_all: fail {} status={}", name, status.0);
            false
        }
        Err(e) => {
            eprintln!("test_all: spawn {} err={}", name, e);
            false
        }
    }
}

fn main() {
    let mut failed = 0usize;
    for test in TESTS {
        if !run_test(test) {
            failed += 1;
        }
    }
    if failed == 0 {
        println!("test_all: ok");
        sys::exit(0);
    }
    eprintln!("test_all: failures={}", failed);
    sys::exit(1);
}
