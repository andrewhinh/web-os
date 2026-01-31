#![no_std]

use ulib::{
    env, eprintln,
    fs::File,
    io::{Read, Write},
    stdio::{stdin, stdout},
    sys,
};

fn main() {
    let args = env::args();

    if args.len() < 2 {
        cat(stdin()).unwrap();
        return;
    }

    let mut failed = false;
    for arg in args.skip(1) {
        match File::open(arg) {
            Ok(file) => {
                if let Err(e) = cat(file) {
                    eprintln!("{}", e);
                    failed = true;
                }
            }
            Err(e) => {
                eprintln!("{}", e);
                failed = true;
            }
        }
    }
    if failed {
        let _ = sys::exit(1);
    }
}

fn cat(mut reader: impl Read) -> sys::Result<()> {
    let mut buf = [0u8; 1024];

    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) if n == 0 => return Ok(()),
            Ok(n) => n,
            Err(e) => return Err(e),
        };
        stdout().write(&buf[..n])?;
    }
}
