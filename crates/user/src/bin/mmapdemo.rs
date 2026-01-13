#![no_std]

use kernel::mmap::{MAP_ANON, MAP_PRIVATE, MAP_SHARED, PROT_READ, PROT_WRITE};
use ulib::{
    eprint, eprintln,
    fs::{File, OpenOptions},
    io::{Read, Write},
    print, println, sys,
};

const PGSIZE: usize = 4096;

fn main() {
    anon_private_demo();
    file_shared_demo();
}

fn anon_private_demo() {
    println!("mmapdemo: anon private");

    let before = sys::freepages().unwrap_or(0);
    let len = 2 * PGSIZE;

    let addr = match sys::mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, 0, 0) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mmapdemo: mmap anon failed err={}", e);
            return;
        }
    };

    let after_mmap = sys::freepages().unwrap_or(0);

    unsafe {
        let p = addr as *mut u8;
        // triggers lazy allocation / page faults
        *p.add(0) = 0x11;
        *p.add(PGSIZE) = 0x22;

        let a = *p.add(0);
        let b = *p.add(PGSIZE);
        println!("mmapdemo: readback a={:#x} b={:#x}", a, b);
    }

    let after_touch = sys::freepages().unwrap_or(0);
    if let Err(e) = sys::munmap(addr, len) {
        eprintln!("mmapdemo: munmap anon failed err={}", e);
        return;
    }
    let after_unmap = sys::freepages().unwrap_or(0);

    println!(
        "mmapdemo: freepages before={} after_mmap={} after_touch={} after_unmap={}",
        before, after_mmap, after_touch, after_unmap
    );
}

fn file_shared_demo() {
    println!("mmapdemo: file shared writeback");

    let path = "/mmapdemo.txt";
    let mut f = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("mmapdemo: open failed path={} err={}", path, e);
            return;
        }
    };

    let _ = f.write(b"hello mmap\n");
    let fd = f.get_fd();

    let addr = match sys::mmap(0, PGSIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mmapdemo: mmap file failed err={}", e);
            return;
        }
    };

    unsafe {
        let p = addr as *mut u8;
        *p.add(0) = b'X';
        *p.add(1) = b'Y';
        let x0 = *p.add(0);
        let x1 = *p.add(1);
        println!("mmapdemo: mapped bytes {:?} {:?}", x0 as char, x1 as char);
    }

    if let Err(e) = sys::munmap(addr, PGSIZE) {
        eprintln!("mmapdemo: munmap file failed err={}", e);
        return;
    }

    drop(f);

    let mut f2 = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("mmapdemo: reopen failed err={}", e);
            return;
        }
    };
    let mut buf = [0u8; 16];
    let n = match f2.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("mmapdemo: read failed err={}", e);
            return;
        }
    };

    print_bytes("mmapdemo: file head=", &buf[..n]);
    println!("");
}

fn print_bytes(prefix: &str, b: &[u8]) {
    use ulib::print;
    print!("{}", prefix);
    for &c in b {
        if c == b'\n' {
            print!("\\n");
        } else if (0x20..=0x7e).contains(&c) {
            print!("{}", c as char);
        } else {
            print!("<{:02x}>", c);
        }
    }
}
