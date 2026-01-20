#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use core::cmp::min;

use ulib::{
    ExitCode, env, eprintln,
    fs::File,
    io::{Read, Write},
    stdio::stdout,
    sys,
};

#[derive(Clone, Copy)]
struct Run {
    count: u32,
    byte: u8,
}

fn usage() {
    eprintln!("usage: punzip <file1> [file2 ...]");
}

fn read_record<R: Read>(reader: &mut R, buf: &mut [u8; 5]) -> sys::Result<Option<Run>> {
    let mut offset = 0usize;
    while offset < buf.len() {
        match reader.read(&mut buf[offset..]) {
            Ok(0) => {
                if offset == 0 {
                    return Ok(None);
                }
                return Err(sys::Error::InvalidArgument);
            }
            Ok(n) => offset += n,
            Err(e) if e == sys::Error::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    let count = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    Ok(Some(Run {
        count,
        byte: buf[4],
    }))
}

fn write_repeat<W: Write>(writer: &mut W, byte: u8, mut count: u32) -> sys::Result<()> {
    let buf = [byte; 4096];
    while count > 0 {
        let chunk = min(count as usize, buf.len());
        writer.write_all(&buf[..chunk])?;
        count -= chunk as u32;
    }
    Ok(())
}

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args.next();
    let files: Vec<&str> = args
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect();

    if files.is_empty() {
        usage();
        return ExitCode::FAILURE;
    }

    let mut out = stdout();
    let mut buf = [0u8; 5];

    for path in files {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                eprintln!("error: cannot open file '{}'", path);
                return ExitCode::FAILURE;
            }
        };

        loop {
            let run = match read_record(&mut file, &mut buf) {
                Ok(Some(run)) => run,
                Ok(None) => break,
                Err(_) => {
                    eprintln!("punzip: file format error");
                    return ExitCode::FAILURE;
                }
            };

            if run.count == 0 {
                eprintln!("punzip: file format error");
                return ExitCode::FAILURE;
            }
            if write_repeat(&mut out, run.byte, run.count).is_err() {
                eprintln!("punzip: write error");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}
