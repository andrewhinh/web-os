#![no_std]

use ulib::{eprintln, fs, println, process::Command, sys};

fn fail(msg: &str) -> ! {
    println!("test_kv: FAIL: {}", msg);
    sys::exit(1);
}

fn run_kv(dir: &str, args: &[&str]) -> ulib::process::Output {
    let mut cmd = Command::new("kv");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.current_dir(dir);
    cmd.output().unwrap_or_else(|_| {
        eprintln!("test_kv: exec kv failed");
        sys::exit(1);
    })
}

fn main() {
    let dir = "/kv_test";
    match fs::create_dir(dir) {
        Ok(_) => {}
        Err(sys::Error::AlreadyExists) => {}
        Err(_) => fail("mkdir"),
    }

    let db_path = "/kv_test/database.txt";
    match fs::remove_file(db_path) {
        Ok(_) => {}
        Err(sys::Error::NotFound) => {}
        Err(_) => fail("cleanup"),
    }

    let output = run_kv(dir, &["p,10,remzi", "p,20,andrea"]);
    if output.status.0 != 0 {
        fail("put exit nonzero");
    }
    if !output.stdout.is_empty() {
        fail("put output");
    }

    let output = run_kv(dir, &["g,10"]);
    if output.status.0 != 0 {
        fail("get exit nonzero");
    }
    if output.stdout.as_slice() != b"10,remzi\n" {
        fail("get output");
    }

    let output = run_kv(dir, &["g,30"]);
    if output.status.0 != 0 {
        fail("get missing exit nonzero");
    }
    if output.stdout.as_slice() != b"30 not found\n" {
        fail("get missing output");
    }

    let output = run_kv(dir, &["a"]);
    if output.status.0 != 0 {
        fail("all exit nonzero");
    }
    let out_str = core::str::from_utf8(&output.stdout).unwrap_or_else(|_| fail("all utf8"));
    let mut has_10 = false;
    let mut has_20 = false;
    let mut count = 0usize;
    for line in out_str.split('\n').filter(|line| !line.is_empty()) {
        count += 1;
        match line {
            "10,remzi" => has_10 = true,
            "20,andrea" => has_20 = true,
            _ => {}
        }
    }
    if count != 2 || !has_10 || !has_20 {
        fail("all output");
    }

    let output = run_kv(dir, &["d,20"]);
    if output.status.0 != 0 {
        fail("delete exit nonzero");
    }
    if !output.stdout.is_empty() {
        fail("delete output");
    }

    let output = run_kv(dir, &["g,20"]);
    if output.status.0 != 0 {
        fail("get deleted exit nonzero");
    }
    if output.stdout.as_slice() != b"20 not found\n" {
        fail("get deleted output");
    }

    let output = run_kv(dir, &["c"]);
    if output.status.0 != 0 {
        fail("clear exit nonzero");
    }
    if !output.stdout.is_empty() {
        fail("clear output");
    }

    let output = run_kv(dir, &["a"]);
    if output.status.0 != 0 {
        fail("all after clear exit nonzero");
    }
    if !output.stdout.is_empty() {
        fail("all after clear output");
    }

    let output = run_kv(dir, &["x"]);
    if output.status.0 != 0 {
        fail("bad command exit nonzero");
    }
    if output.stdout.as_slice() != b"bad command\n" {
        fail("bad command output");
    }

    println!("test_kv: OK");
}
