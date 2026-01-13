#![no_std]
use alloc::format;

use ulib::{
    ExitCode, env, eprint, eprintln,
    fs::{self, File},
    path::Path,
    print, println, sys,
};
extern crate alloc;

fn main() -> ExitCode {
    let args = env::args();
    let mut failed = false;
    if args.len() < 2 {
        if let Err(e) = ls(".") {
            eprintln!("ls: .: {}", e);
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    for arg in args.skip(1) {
        if let Err(e) = ls(arg) {
            failed = true;
            eprintln!("ls: {}: {}", arg, e);
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn ls(path: &str) -> sys::Result<()> {
    let path = Path::new(path);
    match fs::read_dir(path) {
        Err(sys::Error::NotADirectory) => {
            let attr = File::open(path)?.metadata()?;
            let name = path.file_name().unwrap_or(path.to_str());
            println!(
                "{:14} {:6} {:3} {}",
                name,
                format!("{:?}", attr.file_type()),
                attr.inum(),
                attr.len()
            );
        }
        Err(e) => return Err(e),
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let attr = entry.metadata()?;
                println!(
                    "{:14} {:6} {:3} {}",
                    entry.file_name(),
                    format!("{:?}", attr.file_type()),
                    attr.inum(),
                    attr.len()
                );
            }
        }
    }
    Ok(())
}
