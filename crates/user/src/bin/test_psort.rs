#![no_std]
extern crate alloc;

use alloc::{vec, vec::Vec};

use ulib::{eprintln, io::Write, println, process::Command, sys};

const RECORD_SIZE: usize = 100;

fn make_record(key: u32, tag: u8) -> [u8; RECORD_SIZE] {
    let mut rec = [0u8; RECORD_SIZE];
    rec[..4].copy_from_slice(&key.to_le_bytes());
    for byte in rec[4..].iter_mut() {
        *byte = tag;
    }
    rec
}

fn parse_records(data: &[u8]) -> Option<Vec<[u8; RECORD_SIZE]>> {
    if data.len() % RECORD_SIZE != 0 {
        return None;
    }
    let mut records = Vec::with_capacity(data.len() / RECORD_SIZE);
    for chunk in data.chunks_exact(RECORD_SIZE) {
        let mut rec = [0u8; RECORD_SIZE];
        rec.copy_from_slice(chunk);
        records.push(rec);
    }
    Some(records)
}

fn key_of(rec: &[u8; RECORD_SIZE]) -> u32 {
    u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]])
}

fn fail(msg: &str) -> ! {
    println!("test_psort: FAIL: {}", msg);
    sys::exit(1);
}

fn main() {
    let input_records: Vec<[u8; RECORD_SIZE]> = vec![
        make_record(42, 0x2a),
        make_record(7, 0x07),
        make_record(13, 0x0d),
        make_record(7, 0x71),
        make_record(0, 0x00),
        make_record(100, 0x64),
        make_record(5, 0x05),
        make_record(42, 0x2b),
        make_record(3, 0x03),
        make_record(9, 0x09),
        make_record(11, 0x0b),
        make_record(1, 0x01),
        make_record(99, 0x63),
        make_record(8, 0x08),
        make_record(2, 0x02),
        make_record(13, 0x0e),
        make_record(6, 0x06),
    ];

    let path = "/psort_input";
    let mut file = match ulib::fs::File::create(path) {
        Ok(file) => file,
        Err(_) => fail("cannot create input file"),
    };
    for rec in &input_records {
        if file.write_all(rec).is_err() {
            fail("write input file");
        }
    }

    let output = Command::new("psort")
        .arg(path)
        .output()
        .unwrap_or_else(|_| {
            eprintln!("test_psort: exec psort failed");
            sys::exit(1);
        });

    if output.status.0 != 0 {
        fail("psort exit nonzero");
    }

    let Some(output_records) = parse_records(&output.stdout) else {
        fail("output size not multiple of record size");
    };
    if output_records.len() != input_records.len() {
        fail("output record count mismatch");
    }

    for win in output_records.windows(2) {
        if key_of(&win[0]) > key_of(&win[1]) {
            fail("output not sorted by key");
        }
    }

    let mut remaining = input_records.clone();
    for rec in &output_records {
        if let Some(pos) = remaining.iter().position(|r| r == rec) {
            remaining.swap_remove(pos);
        } else {
            fail("output record not in input");
        }
    }
    if !remaining.is_empty() {
        fail("missing output records");
    }

    println!("test_psort: OK");
}
