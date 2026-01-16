#![no_std]

use ulib::{eprintln, println, sys};

const PGSIZE: usize = 4096;
const PAGES: usize = 256;
const WRITE_PAGES: usize = 8;

fn main() {
    let pages = PAGES;
    let bytes = pages * PGSIZE;
    let Ok(base) = sys::sbrk(bytes) else {
        eprintln!("test_cow: sbrk failed ({} pages)", pages);
        return;
    };
    let mem = base as *mut u8;
    touch_pages(mem, bytes, 1);

    let mut p2c = [0usize; 2];
    let mut c2p = [0usize; 2];
    if sys::pipe(&mut p2c).is_err() || sys::pipe(&mut c2p).is_err() {
        eprintln!("test_cow: pipe failed");
        return;
    }
    let (p2c_r, p2c_w) = (p2c[0], p2c[1]);
    let (c2p_r, c2p_w) = (c2p[0], c2p[1]);

    let Ok(f_pre_fork) = sys::freepages() else {
        eprintln!("test_cow: freepages() failed");
        return;
    };

    match sys::fork() {
        Ok(0) => {
            let _ = sys::close(p2c_w);
            let _ = sys::close(c2p_r);
            child_detect(mem, pages, p2c_r, c2p_w)
        }
        Ok(_) => {
            let _ = sys::close(p2c_r);
            let _ = sys::close(c2p_w);

            let Ok(f_post_fork) = sys::freepages() else {
                eprintln!("test_cow: freepages() failed");
                return;
            };

            let _ = sys::write(p2c_w, &[b'w']);
            let mut ack = [0u8; 1];
            let _ = sys::read(c2p_r, &mut ack);

            let Ok(f_post_write) = sys::freepages() else {
                eprintln!("test_cow: freepages() failed");
                return;
            };

            let _ = sys::write(p2c_w, &[b'x']);
            let mut st: i32 = 0;
            let _ = sys::wait(&mut st);

            let delta_fork = f_pre_fork.saturating_sub(f_post_fork);
            let delta_write = f_post_fork.saturating_sub(f_post_write);

            let parent0 = unsafe { *mem.add(0) };
            if parent0 != 1 {
                eprintln!("test_cow: FAIL parent changed got={} want=1", parent0);
                return;
            }

            if delta_fork >= pages / 2 {
                println!(
                    "test_cow: FAIL no COW (fork copied) dfork={} dwrite={}",
                    delta_fork, delta_write
                );
            } else if delta_write >= 1 {
                println!(
                    "test_cow: PASS COW dfork={} dwrite={}",
                    delta_fork, delta_write
                );
            } else {
                eprintln!(
                    "test_cow: INCONCLUSIVE dfork={} dwrite={}",
                    delta_fork, delta_write
                );
            }
        }
        Err(e) => eprintln!("test_cow: fork failed err={}", e),
    }
}

fn child_detect(mem: *mut u8, pages: usize, p2c_r: usize, c2p_w: usize) -> ! {
    let mut cmd = [0u8; 1];
    if sys::read(p2c_r, &mut cmd).unwrap_or(0) != 1 || cmd[0] != b'w' {
        sys::exit(1);
    }
    let writes = core::cmp::min(pages, WRITE_PAGES);
    unsafe {
        for i in 0..writes {
            *mem.add(i * PGSIZE) = 2; // should allocate pages if COW
        }
    }
    let _ = sys::write(c2p_w, &[b'a']);
    if sys::read(p2c_r, &mut cmd).unwrap_or(0) != 1 || cmd[0] != b'x' {
        sys::exit(1);
    }
    sys::exit(0)
}

fn touch_pages(mem: *mut u8, bytes: usize, val: u8) {
    let mut off = 0usize;
    while off < bytes {
        unsafe { *mem.add(off) = val };
        off += PGSIZE;
    }
}
