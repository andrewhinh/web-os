#![no_std]

use ulib::{
    eprintln,
    fs::{File, OpenOptions},
    io::{Read, Write},
    println, sys,
};

fn print_meta(label: &str, meta: &ulib::fs::Metadata) {
    println!(
        "test_fsync: {} size={} atime={} mtime={} ctime={}",
        label,
        meta.len(),
        meta.atime(),
        meta.mtime(),
        meta.ctime()
    );
}

fn main() -> sys::Result<()> {
    println!("test_fsync: start");
    let path = "/t_fsync.txt";

    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) => {
            eprintln!("test_fsync: open err={}", e);
            return Err(e);
        }
    };

    let meta0 = file.metadata()?;
    print_meta("created", &meta0);

    sys::sleep(1)?;
    let n = file.write(b"hello")?;
    println!("test_fsync: write n={}", n);
    let meta1 = file.metadata()?;
    print_meta("after_write", &meta1);

    file.sync()?;
    println!("test_fsync: fsync ok");

    sys::sleep(1)?;
    let mut reader = File::open(path)?;
    let mut buf = [0u8; 5];
    let r = reader.read(&mut buf)?;
    println!("test_fsync: read n={}", r);
    let meta2 = reader.metadata()?;
    print_meta("after_read", &meta2);

    Ok(())
}
