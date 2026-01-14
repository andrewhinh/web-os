#![no_std]

use ulib::{fs::File, io::Read, println, stdio::stdin, sys};

fn main() -> sys::Result<()> {
    let c0 = sys::extirqcount()?;
    println!("aplicdemo: extirqcount start={}", c0);

    // Trigger  virtio-disk activity.
    if let Ok(mut f) = File::open("/bin/sh") {
        let mut buf = [0u8; 512];
        for _ in 0..32 {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
        }
    }
    let c1 = sys::extirqcount()?;
    println!("aplicdemo: after disk read extirqcount={}", c1);

    println!("aplicdemo: type chars. enter to stop.");
    let mut b = [0u8; 1];
    loop {
        let n = stdin().read(&mut b)?;
        if n == 0 {
            break;
        }
        if b[0] == b'\n' || b[0] == b'\r' {
            break;
        }
        let c = sys::extirqcount()?;
        println!("aplicdemo: got '{}' extirqcount={}", b[0] as char, c);
    }

    let c2 = sys::extirqcount()?;
    println!("aplicdemo: done extirqcount={}", c2);
    Ok(())
}
