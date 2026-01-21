#![no_std]
extern crate alloc;

use alloc::{string::String, vec::Vec};

use ulib::{
    ExitCode, env, eprintln,
    fs::File,
    io::Read,
    mapreduce::{Getter, MR_DefaultHashPartition, MR_Emit, MR_Run},
    println, sysinfo,
};

fn map_file(path: &str) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => {
            eprintln!("mapreduce: cannot open '{}'", path);
            return;
        }
    };
    let mut data = String::new();
    if file.read_to_string(&mut data).is_err() {
        eprintln!("mapreduce: read error '{}'", path);
        return;
    }
    for word in data.split(|c: char| c.is_ascii_whitespace()) {
        let trimmed = word.trim();
        if !trimmed.is_empty() {
            MR_Emit(trimmed, "1");
        }
    }
}

fn reduce_word(key: &str, get_next: Getter, partition: usize) {
    let mut count = 0usize;
    while get_next(key, partition).is_some() {
        count += 1;
    }
    println!("{} {}", key, count);
}

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args.next();
    let files: Vec<&str> = args
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect();

    if files.is_empty() {
        eprintln!("usage: mapreduce <file1> [file2 ...]");
        return ExitCode::FAILURE;
    }

    let nprocs = sysinfo::get_nprocs_conf().max(1);
    MR_Run(
        &files,
        map_file,
        nprocs,
        reduce_word,
        nprocs,
        MR_DefaultHashPartition,
    );
    ExitCode::SUCCESS
}
