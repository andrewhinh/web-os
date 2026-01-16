#![no_std]
use ulib::{env, fs};

fn main() {
    let mut args = env::args();
    let _ = args.next();
    let mut symlink = false;
    let mut target = args.next();
    if matches!(target, Some("-s")) {
        symlink = true;
        target = args.next();
    }
    let link = args.next();
    if target.is_none() || link.is_none() || args.next().is_some() {
        panic!("Usage: ln [-s] target link");
    }
    let target = target.unwrap();
    let link = link.unwrap();
    if symlink {
        fs::symlink(target, link).unwrap();
    } else {
        fs::hard_link(target, link).unwrap();
    }
}
