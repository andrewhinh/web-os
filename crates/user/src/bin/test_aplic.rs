#![no_std]

use ulib::{
    eprintln,
    fs::File,
    io::{Read, Write},
    println, sys,
};

fn main() -> sys::Result<()> {
    let irq0 = sys::extirqcount()?;
    let tp0 = sys::ktaskpolls()?;
    println!("test_aplic: start ext_irqs={} ktaskpolls={}", irq0, tp0);

    // Trigger virtio-disk activity.
    let path = "/t_aplic.tmp";
    let mut f = File::create(path).map_err(|e| {
        eprintln!("test_aplic: create err={}", e);
        e
    })?;
    let buf = [b'A'; 512];
    for _ in 0..64 {
        f.write_all(&buf).map_err(|e| {
            eprintln!("test_aplic: write err={}", e);
            e
        })?;
    }
    drop(f);

    let mut f = File::open(path).map_err(|e| {
        eprintln!("test_aplic: open err={}", e);
        e
    })?;
    let mut rb = [0u8; 512];
    let mut read_total = 0usize;
    loop {
        let n = f.read(&mut rb).map_err(|e| {
            eprintln!("test_aplic: read err={}", e);
            e
        })?;
        if n == 0 {
            break;
        }
        read_total += n;
    }
    drop(f);
    let _ = sys::unlink(path);

    let irq1 = sys::extirqcount()?;
    let tp1 = sys::ktaskpolls()?;
    println!(
        "test_aplic: done ext_irqs={} (+{}) ktaskpolls={} (+{}) bytes_read={}",
        irq1,
        irq1.saturating_sub(irq0),
        tp1,
        tp1.saturating_sub(tp0),
        read_total
    );
    Ok(())
}
