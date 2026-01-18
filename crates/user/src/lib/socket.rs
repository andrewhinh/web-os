pub use kernel::socket::{AF_INET, AF_UNIX, SOCK_DGRAM, SOCK_STREAM};

use crate::fs::File;
use crate::sys;

pub fn socket(domain: usize, stype: usize, protocol: usize) -> sys::Result<File> {
    let fd = sys::socket(domain, stype, protocol)?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn bind(sock: &File, path: &str) -> sys::Result<()> {
    sys::bind(sock.get_fd(), path)
}

pub fn listen(sock: &File, backlog: usize) -> sys::Result<()> {
    sys::listen(sock.get_fd(), backlog)
}

pub fn accept(sock: &File) -> sys::Result<File> {
    let fd = sys::accept(sock.get_fd())?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn connect(sock: &File, path: &str) -> sys::Result<()> {
    sys::connect(sock.get_fd(), path)
}
