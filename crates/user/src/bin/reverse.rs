#![no_std]
extern crate alloc;

use alloc::vec::Vec;

use ulib::{
    ExitCode, env, eprintln,
    fs::{self, File},
    io::{Read, Write},
    stdio::{stdin, stdout},
    sys,
};

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args.next();
    let rest: Vec<&str> = args
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect();
    if rest.len() > 2 {
        usage();
        return ExitCode::FAILURE;
    }
    let input = rest.get(0).copied();
    let output = rest.get(1).copied();

    if let (Some(input), Some(output)) = (input, output) {
        if input == output {
            eprintln!("Input and output file must differ");
            return ExitCode::FAILURE;
        }
    }

    let input_file = if let Some(ref path) = input {
        match File::open(path) {
            Ok(file) => Some(file),
            Err(_) => {
                eprintln!("error: cannot open file '{}'", path);
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    if let (Some(input_file), Some(output)) = (&input_file, &output) {
        if let Ok(in_meta) = input_file.metadata() {
            if let Ok(out_meta) = fs::metadata(output) {
                if in_meta.inum() == out_meta.inum() {
                    eprintln!("Input and output file must differ");
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    if let Some(output) = output {
        let out = match File::create(output) {
            Ok(file) => file,
            Err(_) => {
                eprintln!("error: cannot open file '{}'", output);
                return ExitCode::FAILURE;
            }
        };
        if let Err(_) = match input_file {
            Some(file) => reverse(file, out),
            None => reverse(stdin(), out),
        } {
            return ExitCode::FAILURE;
        }
    } else if let Err(_) = match input_file {
        Some(file) => reverse(file, stdout()),
        None => reverse(stdin(), stdout()),
    } {
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn usage() {
    eprintln!("usage: reverse <input> <output>");
}

fn reverse<R: Read, W: Write>(mut reader: R, mut writer: W) -> sys::Result<()> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e == sys::Error::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        for &byte in &buf[..n] {
            current.push(byte);
            if byte == b'\n' || byte == b'\r' {
                lines.push(core::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    for line in lines.iter().rev() {
        writer.write_all(line)?;
    }
    Ok(())
}
