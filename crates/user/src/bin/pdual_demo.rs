#![no_std]
extern crate alloc;

use ulib::{
    env, eprintln,
    io::Write,
    println,
    process::{Command, Stdio},
    stdio::{stderr, stdout},
    sys,
};

fn child() -> ! {
    let mut err = stderr();
    let mut out = stdout();

    let _ = err.write_all(b"stderr: ok\n");
    let chunk = [b'E'; 256];
    let _ = err.write_all(&chunk);
    let _ = out.write_all(b"stdout: ok\n");
    sys::exit(0)
}

fn main() {
    let mut args = env::args();
    let _ = args.next();
    if args.next().as_deref() == Some("--child") {
        child();
    }

    let mut cmd = Command::new("/bin/pdual_demo");
    cmd.arg("--child");
    cmd.stdout(Stdio::MakePipe).stderr(Stdio::MakePipe);

    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("pdual_demo: spawn/output failed: {}", e);
            return;
        }
    };

    println!("status={}", out.status.0);
    println!("stdout={}B stderr={}B", out.stdout.len(), out.stderr.len());

    match core::str::from_utf8(&out.stdout) {
        Ok(s) => println!("stdout_preview={}", s.trim_end()),
        Err(_) => println!("stdout_preview=<non-utf8>"),
    }

    match core::str::from_utf8(&out.stderr) {
        Ok(s) => println!("stderr_preview={}", s.trim_end()),
        Err(_) => println!("stderr_preview=<non-utf8>"),
    }
}
