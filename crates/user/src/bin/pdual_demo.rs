#![no_std]
extern crate alloc;

use ulib::{eprintln, println};
use ulib::{
    process::{Command, Stdio},
    sys,
};

fn main() {
    let mut cmd = Command::new("pdual_child");
    cmd.stdout(Stdio::MakePipe).stderr(Stdio::MakePipe);

    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("pipe_dual_demo: spawn/output failed: {}", e);
            return;
        }
    };

    println!("status={}", out.status.0);
    println!("stdout={}B stderr={}B", out.stdout.len(), out.stderr.len());

    match core::str::from_utf8(&out.stdout) {
        Ok(s) => println!("stdout_preview={}", s.trim_end()),
        Err(_) => println!("stdout_preview=<non-utf8>"),
    }

    let _ = sys::sleep(1);
}
