#![no_std]

use core::mem::size_of;

use kernel::fs::{BSIZE, FSMAGIC, SuperBlock};
use ulib::{eprintln, fs::File, io::Read, println};

fn main() {
    println!("test_disk: raw disk superblock");

    let mut disk = match File::open("/dev/disk") {
        Ok(file) => file,
        Err(e) => {
            eprintln!("test_disk: open /dev/disk failed err={}", e);
            return;
        }
    };

    let mut buf = [0u8; BSIZE];
    let boot_read = match disk.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("test_disk: read boot block failed err={}", e);
            return;
        }
    };
    if boot_read != BSIZE {
        eprintln!("test_disk: short boot read n={}", boot_read);
        return;
    }

    let sb_read = match disk.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("test_disk: read superblock failed err={}", e);
            return;
        }
    };
    if sb_read < size_of::<SuperBlock>() {
        eprintln!("test_disk: short superblock read n={}", sb_read);
        return;
    }

    let sb = unsafe { (buf.as_ptr() as *const SuperBlock).read_unaligned() };
    if sb.magic != FSMAGIC {
        eprintln!("test_disk: bad magic=0x{:x}", sb.magic);
        return;
    }

    println!("test_disk: ok magic=0x{:x}", sb.magic);
}
