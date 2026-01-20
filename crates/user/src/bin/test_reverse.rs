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
    println!("test_reverse: FAIL: {}", msg);
    sys::exit(1);
}

fn main() {
    let input = b"alpha\nbeta\ngamma\n";
    let expected = b"gamma\nbeta\nalpha\n";

    let input_path = "/rev_in";
    let output_path = "/rev_out";

    let mut file = match File::create(input_path) {
        Ok(file) => file,
        Err(_) => fail("create input"),
    };
    if file.write_all(input).is_err() {
        fail("write input");
    }

    let status = Command::new("reverse")
        .arg(input_path)
        .arg(output_path)
        .status()
        .unwrap_or_else(|_| {
            eprintln!("test_reverse: exec reverse failed");
            sys::exit(1);
        });
    if status.0 != 0 {
        fail("reverse exit nonzero");
    }

    let mut out = match File::open(output_path) {
        Ok(file) => file,
        Err(_) => fail("open output"),
    };
    let mut buf: Vec<u8> = Vec::new();
    if out.read_to_end(&mut buf).is_err() {
        fail("read output");
    }
    if buf.as_slice() != expected {
        fail("output mismatch");
    }

    println!("test_reverse: OK");
}
