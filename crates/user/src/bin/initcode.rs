#![no_std]

use ulib::sys;

const INIT: &str = "/init";
const ARGV: [&str; 1] = ["init"];

fn main() {
    sys::exec(INIT, &ARGV, None).unwrap();
    sys::exit(0);
}
