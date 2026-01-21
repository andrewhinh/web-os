#![no_std]
extern crate alloc;

use alloc::{
    collections::btree_map::BTreeMap,
    string::{String, ToString},
};

use ulib::{eprintln, fs::File, io::Write, println, process::Command, sys};

fn fail(msg: &str) -> ! {
    println!("test_mr: FAIL: {}", msg);
    sys::exit(1);
}

fn write_input(path: &str, data: &[u8]) {
    let mut file = match File::create(path) {
        Ok(file) => file,
        Err(_) => fail("create input"),
    };
    if file.write_all(data).is_err() {
        fail("write input");
    }
}

fn parse_counts(output: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            fail("bad output line");
        };
        let Some(count_str) = parts.next() else {
            fail("bad output line");
        };
        if parts.next().is_some() {
            fail("bad output line");
        }
        let count = count_str
            .parse::<usize>()
            .unwrap_or_else(|_| fail("bad count"));
        if counts.insert(key.to_string(), count).is_some() {
            fail("duplicate key");
        }
    }
    counts
}

fn main() {
    let input1 = "/mr_in1";
    let input2 = "/mr_in2";
    write_input(input1, b"alpha beta beta gamma\nalpha");
    write_input(input2, b"beta delta\ngamma gamma\n");

    let output = Command::new("mapreduce")
        .arg(input1)
        .arg(input2)
        .output()
        .unwrap_or_else(|_| {
            eprintln!("test_mr: exec mapreduce failed");
            sys::exit(1);
        });
    if output.status.0 != 0 {
        fail("mapreduce exit nonzero");
    }
    let out_str = core::str::from_utf8(&output.stdout).unwrap_or_else(|_| fail("output utf8"));

    let counts = parse_counts(out_str);
    let mut expected = BTreeMap::new();
    expected.insert("alpha".to_string(), 2);
    expected.insert("beta".to_string(), 3);
    expected.insert("gamma".to_string(), 3);
    expected.insert("delta".to_string(), 1);

    if counts != expected {
        fail("counts mismatch");
    }
    println!("test_mr: OK");
}
