#![no_std]

use ulib::{
    env, eprintln,
    fs::{File, OpenOptions, remove_file},
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

fn journal_demo() -> sys::Result<()> {
    const PATH: &str = "/t_journal.txt";
    const DATA: &[u8] = b"journal-data";

    println!("test_fsync: journal demo");

    if let Ok(mut existing) = File::open(PATH) {
        let mut buf = [0u8; 32];
        let n = existing.read(&mut buf)?;
        if n == DATA.len() && &buf[..n] == DATA {
            let _ = remove_file(PATH);
            println!("test_fsync: journal recovered ok, removed {}", PATH);
        }
        if n > 0 {
            eprintln!("test_fsync: journal mismatch n={}", n);
            return Err(sys::Error::InvalidArgument);
        }
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(PATH)?;
    println!("test_fsync: journal phase1 arming crash");
    sys::logcrash(1)?;
    let n = file.write(DATA)?;
    println!("test_fsync: journal wrote n={}", n);
    file.sync()?;
    eprintln!("test_fsync: crash did not trigger");
    Ok(())
}

fn main() -> sys::Result<()> {
    if env::args().any(|arg| arg == "--journal") {
        return journal_demo();
    }

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
