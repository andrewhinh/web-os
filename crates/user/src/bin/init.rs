#![no_std]
extern crate alloc;
use ulib::{
    eprintln,
    fs::{self, File, OpenOptions},
    io::Read,
    path::Path,
    println,
    process::Command,
    stdio,
    sys::{self, Major},
};

fn journal_recover() {
    const PATH: &str = "/t_journal.txt";
    const DATA: &[u8] = b"journal-data";

    let Ok(mut file) = File::open(PATH) else {
        return;
    };
    let mut buf = [0u8; 32];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("init: journal read err={}", e);
            return;
        }
    };
    if n == DATA.len() && &buf[..n] == DATA {
        let _ = fs::remove_file(PATH);
        println!("init: journal recovered, removed {}", PATH);
    } else if n > 0 {
        let _ = fs::remove_file(PATH);
        eprintln!(
            "init: journal mismatch n={}, removed {}, rebuild fs.img or FORCE_MKFS=1",
            n, PATH
        );
    }
}

fn main() -> sys::Result<()> {
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/console")
        {
            Err(_) => {
                if !Path::new("/dev").is_dir() {
                    fs::create_dir("/dev")?;
                }
                sys::mknod("/dev/console", Major::Console as usize, 0)?;
            }
            Ok(stdin) => {
                stdio::stdout().set(stdin.try_clone()?)?;
                stdio::stderr().set(stdin.try_clone()?)?;
                stdio::stdin().set(stdin)?;
                break;
            }
        }
    }
    if File::open("/dev/null").is_err() {
        match sys::mknod("/dev/null", Major::Null as usize, 0) {
            Ok(()) => {}
            Err(sys::Error::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
    }
    if File::open("/dev/disk").is_err() {
        match sys::mknod("/dev/disk", Major::Disk as usize, 0) {
            Ok(()) => {}
            Err(sys::Error::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
    }
    if !Path::new("/tmp").is_dir() {
        let _ = fs::create_dir("/tmp");
    }

    journal_recover();

    loop {
        println!("\ninit: starting sh\n");
        let mut child = Command::new("/bin/sh").spawn().unwrap();
        child.wait().unwrap();
    }
}
