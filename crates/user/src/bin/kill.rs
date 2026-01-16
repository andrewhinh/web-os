#![no_std]
use ulib::{env, signal, sys};

fn main() {
    let mut args = env::args().skip(1).peekable();
    let mut sig = signal::SIGTERM;

    if let Some(first) = args.peek() {
        if let Some(rest) = first.strip_prefix('-') {
            sig = rest.parse::<usize>().unwrap();
            let _ = args.next();
        }
    }

    if args.peek().is_none() {
        panic!("usage: kill [-sig] pid...");
    }

    for arg in args {
        sys::kill(arg.parse::<usize>().unwrap(), sig).unwrap()
    }
}
