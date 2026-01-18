#![no_std]

use ulib::io::{Read, Write};
use ulib::{eprintln, println, socket, sys};

const UDP_ADDR: &str = "10.0.2.15:4000";
const TCP_ADDR: &str = "10.0.2.15:5000";

fn main() {
    test_udp();
    test_tcp();
}

fn test_udp() {
    println!("test_net: udp");
    let mut server = match socket::socket(socket::AF_INET, socket::SOCK_DGRAM, 0) {
        Ok(sock) => sock,
        Err(e) => {
            eprintln!("test_net: udp socket err={}", e);
            return;
        }
    };
    if let Err(e) = socket::bind(&server, UDP_ADDR) {
        eprintln!("test_net: udp bind err={}", e);
        return;
    }
    let pid = match sys::fork() {
        Ok(pid) => pid,
        Err(e) => {
            eprintln!("test_net: udp fork err={}", e);
            return;
        }
    };
    if pid == 0 {
        let mut client = match socket::socket(socket::AF_INET, socket::SOCK_DGRAM, 0) {
            Ok(sock) => sock,
            Err(e) => {
                eprintln!("test_net: udp client socket err={}", e);
                sys::exit(1);
            }
        };
        if let Err(e) = socket::connect(&client, UDP_ADDR) {
            eprintln!("test_net: udp connect err={}", e);
            sys::exit(1);
        }
        let msg = b"ping";
        if let Err(e) = client.write(msg) {
            eprintln!("test_net: udp write err={}", e);
            sys::exit(1);
        }
        let mut buf = [0u8; 16];
        let n = match client.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("test_net: udp read err={}", e);
                sys::exit(1);
            }
        };
        println!("test_net: udp reply bytes={}", n);
        sys::exit(0);
    }
    let mut buf = [0u8; 16];
    let n = match server.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("test_net: udp server read err={}", e);
            let mut status: i32 = 0;
            let _ = sys::wait(&mut status);
            return;
        }
    };
    let _ = server.write(&buf[..n]);
    let mut status: i32 = 0;
    let _ = sys::wait(&mut status);
}

fn test_tcp() {
    println!("test_net: tcp");
    let server = match socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0) {
        Ok(sock) => sock,
        Err(e) => {
            eprintln!("test_net: tcp socket err={}", e);
            return;
        }
    };
    if let Err(e) = socket::bind(&server, TCP_ADDR) {
        eprintln!("test_net: tcp bind err={}", e);
        return;
    }
    if let Err(e) = socket::listen(&server, 4) {
        eprintln!("test_net: tcp listen err={}", e);
        return;
    }
    let pid = match sys::fork() {
        Ok(pid) => pid,
        Err(e) => {
            eprintln!("test_net: tcp fork err={}", e);
            return;
        }
    };
    if pid == 0 {
        let mut client = match socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0) {
            Ok(sock) => sock,
            Err(e) => {
                eprintln!("test_net: tcp client socket err={}", e);
                sys::exit(1);
            }
        };
        if let Err(e) = socket::connect(&client, TCP_ADDR) {
            eprintln!("test_net: tcp connect err={}", e);
            sys::exit(1);
        }
        let msg = b"ping";
        if let Err(e) = client.write(msg) {
            eprintln!("test_net: tcp write err={}", e);
            sys::exit(1);
        }
        let mut buf = [0u8; 16];
        let n = match client.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("test_net: tcp read err={}", e);
                sys::exit(1);
            }
        };
        println!("test_net: tcp reply bytes={}", n);
        sys::exit(0);
    }
    let mut conn = match socket::accept(&server) {
        Ok(sock) => sock,
        Err(e) => {
            eprintln!("test_net: tcp accept err={}", e);
            let mut status: i32 = 0;
            let _ = sys::wait(&mut status);
            return;
        }
    };
    let mut buf = [0u8; 16];
    let n = match conn.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("test_net: tcp server read err={}", e);
            let mut status: i32 = 0;
            let _ = sys::wait(&mut status);
            return;
        }
    };
    let _ = conn.write(&buf[..n]);
    let mut status: i32 = 0;
    let _ = sys::wait(&mut status);
}
