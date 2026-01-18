#![no_std]
use ulib::{env, sys};

fn main() {
    let mut args = env::args().skip(1).peekable();

    if args.peek().is_none() {
        panic!("Usage: sleep TIME...")
    }

    let n = args.next().unwrap();
    match sys::sleep(n.parse().unwrap()) {
        Ok(_) => {}
        Err(sys::Error::Interrupted) => {}
        Err(err) => panic!("{err}"),
    }
}
