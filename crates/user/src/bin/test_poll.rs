#![no_std]

use ulib::sys::poll;
use ulib::{
    eprintln,
    io::{Read, Write},
    pipe, print, println, sys,
};

fn main() -> sys::Result<()> {
    println!("test_poll: start");

    let (mut reader, mut writer) = pipe::pipe()?;
    reader.set_nonblock()?;

    let rfd = reader.get_fd();
    let wfd = writer.get_fd();

    let mut fds = [
        poll::PollFd {
            fd: rfd,
            events: poll::IN,
            revents: 0,
        },
        poll::PollFd {
            fd: wfd,
            events: poll::OUT,
            revents: 0,
        },
    ];

    let n0 = sys::poll(&mut fds, 0)?;
    print_poll("initial", &fds, n0);

    let msg = [b'Z'];
    match writer.write(&msg) {
        Ok(n) => println!("test_poll: write n={}", n),
        Err(e) => eprintln!("test_poll: write err={}", e),
    }

    let n1 = sys::poll(&mut fds, 0)?;
    print_poll("after_write", &fds, n1);

    let mut buf = [0u8; 1];
    match reader.read(&mut buf) {
        Ok(n) => println!("test_poll: read n={} b={}", n, buf[0] as char),
        Err(e) => eprintln!("test_poll: read err={}", e),
    }

    let n2 = sys::select(&mut fds, 0)?;
    print_poll("select", &fds, n2);

    match reader.read(&mut buf) {
        Err(sys::Error::WouldBlock) => println!("test_poll: nonblock read -> would block"),
        Ok(n) => println!("test_poll: read n={}", n),
        Err(e) => eprintln!("test_poll: read err={}", e),
    }

    Ok(())
}

fn print_poll(label: &str, fds: &[poll::PollFd], n: usize) {
    print!("test_poll: {} n={}", label, n);
    for fd in fds {
        print!(" fd={} ev=0x{:x} rev=0x{:x}", fd.fd, fd.events, fd.revents);
    }
    println!("");
}
