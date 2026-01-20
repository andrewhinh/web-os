#![no_std]
extern crate alloc;

use alloc::vec::Vec;

use ulib::{
    eprintln,
    fs::File,
    io::{Read, Write},
    println,
    process::Command,
    sys,
};

fn fail(msg: &str) -> ! {
    println!("test_pzip: FAIL: {}", msg);
    sys::exit(1);
}

fn main() {
    let input = b"aaabbbccccccddeeeeffffgggg\n";
    let input_path = "/pzip_input";
    let zip_path = "/pzip_output";

    let mut file = match File::create(input_path) {
        Ok(file) => file,
        Err(_) => fail("create input"),
    };
    if file.write_all(input).is_err() {
        fail("write input");
    }

    let zip_out = Command::new("pzip")
        .arg(input_path)
        .output()
        .unwrap_or_else(|_| {
            eprintln!("test_pzip: exec pzip failed");
            sys::exit(1);
        });
    if zip_out.status.0 != 0 {
        fail("pzip exit nonzero");
    }

    let mut zip_file = match File::create(zip_path) {
        Ok(file) => file,
        Err(_) => fail("create zip output"),
    };
    if zip_file.write_all(&zip_out.stdout).is_err() {
        fail("write zip output");
    }

    let unzip_out = Command::new("punzip")
        .arg(zip_path)
        .output()
        .unwrap_or_else(|_| {
            eprintln!("test_pzip: exec punzip failed");
            sys::exit(1);
        });
    if unzip_out.status.0 != 0 {
        fail("punzip exit nonzero");
    }
    if unzip_out.stdout.as_slice() != input {
        fail("roundtrip mismatch");
    }

    println!("test_pzip: OK");
}
