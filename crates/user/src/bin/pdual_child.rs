#![no_std]
use ulib::io::Write;
use ulib::stdio::{stderr, stdout};

fn main() {
    let mut err = stderr();

    // pipe = 512B
    let chunk = [b'E'; 64];
    for _ in 0..128 {
        err.write_all(&chunk).unwrap();
    }

    let mut out = stdout();
    out.write_all(b"stdout: ok\n").unwrap();
}
