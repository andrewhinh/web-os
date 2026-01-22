#![no_std]
extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use ulib::{
    eprintln,
    fs::File,
    io::{Read, Write},
    println,
    process::Command,
    signal, socket, sys,
};

const SERVER_IP: &str = "10.0.2.15";
const SERVER_PORT: u16 = 10001;

const READY_RETRIES: usize = 20;
const READY_SLEEP_TICKS: usize = 1;
const READY_TIMEOUT_TICKS: usize = 30;
const CLIENT_TIMEOUT_TICKS: usize = 200;
const WRITE_TIMEOUT_TICKS: usize = 50;
const READ_TIMEOUT_TICKS: usize = 200;

const PARALLEL_CLIENTS: usize = 4;

const MAX_LINE: usize = 4096;

fn main() {
    println!("test_memcached: start");
    let mut cmd = Command::new("/bin/memcached");
    cmd.pgrp(0);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("test_memcached: spawn err={}", e);
            sys::exit(1);
        }
    };
    let pid = child.pid();
    let mut ok = true;
    if !wait_for_ready() {
        eprintln!("test_memcached: server not ready");
        ok = false;
    }
    if ok && !run_parallel_clients() {
        eprintln!("test_memcached: parallel clients failed");
        ok = false;
    }
    if ok && !run_with_timeout(CLIENT_TIMEOUT_TICKS, || run_client_sequence("serial")) {
        eprintln!("test_memcached: serial client failed");
        ok = false;
    }
    terminate_child(pid);
    if ok {
        println!("test_memcached: ok");
        sys::exit(0);
    }
    eprintln!("test_memcached: fail");
    sys::exit(1);
}

fn wait_for_ready() -> bool {
    for _ in 0..READY_RETRIES {
        if run_with_timeout(READY_TIMEOUT_TICKS, run_ready_check) {
            return true;
        }
        let _ = sys::sleep(READY_SLEEP_TICKS);
    }
    false
}

fn run_parallel_clients() -> bool {
    let mut pids = Vec::new();
    for idx in 0..PARALLEL_CLIENTS {
        let key = format!("k{}", idx);
        match spawn_client(&key) {
            Ok(pid) => {
                pids.push(pid);
            }
            Err(e) => {
                eprintln!("test_memcached: spawn client err={}", e);
                return false;
            }
        }
    }
    wait_all(pids, CLIENT_TIMEOUT_TICKS)
}

fn run_with_timeout<F>(ticks: usize, f: F) -> bool
where
    F: FnOnce() -> sys::Result<()>,
{
    let pid = match sys::fork() {
        Ok(0) => {
            let ok = f().is_ok();
            sys::exit(if ok { 0 } else { 1 });
        }
        Ok(pid) => pid,
        Err(_) => return false,
    };
    match wait_child(pid, ticks) {
        Some(status) => status == 0,
        None => {
            let _ = sys::kill(pid, signal::SIGTERM);
            let _ = wait_child(pid, ticks);
            false
        }
    }
}

fn run_ready_check() -> sys::Result<()> {
    let key = "ready";
    let mut conn = connect_with_retry()?;
    let _ = conn.set_nonblock();
    let set_cmd = format!("set {} 0 3600 1\r\n1\r\n", key);
    send_cmd(&mut conn, &set_cmd, WRITE_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "STORED", READ_TIMEOUT_TICKS)?;
    let get_cmd = format!("get {}\r\n", key);
    send_cmd(&mut conn, &get_cmd, WRITE_TIMEOUT_TICKS)?;
    let value_line = format!("VALUE {} 0 1", key);
    expect_line(&mut conn, &value_line, READ_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "1", READ_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "END", READ_TIMEOUT_TICKS)?;
    Ok(())
}

fn run_client_sequence(key: &str) -> sys::Result<()> {
    let mut conn = connect_with_retry()?;
    let _ = conn.set_nonblock();
    let set_cmd = format!("set {} 0 3600 1\r\n9\r\n", key);
    send_cmd(&mut conn, &set_cmd, WRITE_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "STORED", READ_TIMEOUT_TICKS)?;

    let incr_cmd = format!("incr {} 1\r\n", key);
    send_cmd(&mut conn, &incr_cmd, WRITE_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "10", READ_TIMEOUT_TICKS)?;

    let decr_cmd = format!("decr {} 2\r\n", key);
    send_cmd(&mut conn, &decr_cmd, WRITE_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "8", READ_TIMEOUT_TICKS)?;

    let mult_cmd = format!("mult {} 2\r\n", key);
    send_cmd(&mut conn, &mult_cmd, WRITE_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "16", READ_TIMEOUT_TICKS)?;

    let div_cmd = format!("div {} 3\r\n", key);
    send_cmd(&mut conn, &div_cmd, WRITE_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "5", READ_TIMEOUT_TICKS)?;

    let get_cmd = format!("get {}\r\n", key);
    send_cmd(&mut conn, &get_cmd, WRITE_TIMEOUT_TICKS)?;
    let value_line = format!("VALUE {} 0 1", key);
    expect_line(&mut conn, &value_line, READ_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "5", READ_TIMEOUT_TICKS)?;
    expect_line(&mut conn, "END", READ_TIMEOUT_TICKS)?;

    let _ = send_cmd(&mut conn, "quit\r\n", WRITE_TIMEOUT_TICKS);
    Ok(())
}

fn connect_with_retry() -> sys::Result<File> {
    let addr = format!("{}:{}", SERVER_IP, SERVER_PORT);
    let mut last_err = sys::Error::NotConnected;
    for _ in 0..READY_RETRIES {
        let client = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0)?;
        match socket::connect(&client, &addr) {
            Ok(()) => return Ok(client),
            Err(e) => {
                last_err = e;
                drop(client);
                let _ = sys::sleep(READY_SLEEP_TICKS);
            }
        }
    }
    Err(last_err)
}

fn send_cmd(conn: &mut File, cmd: &str, ticks: usize) -> sys::Result<()> {
    let bytes = cmd.as_bytes();
    let mut sent = 0usize;
    for _ in 0..ticks {
        match conn.write(&bytes[sent..]) {
            Ok(0) => return Err(sys::Error::WriteZero),
            Ok(n) => {
                sent += n;
                if sent >= bytes.len() {
                    return Ok(());
                }
            }
            Err(sys::Error::WouldBlock) => {
                let _ = sys::sleep(1);
            }
            Err(sys::Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    Err(sys::Error::Uncategorized)
}

fn expect_line(conn: &mut File, expected: &str, ticks: usize) -> sys::Result<()> {
    let line = read_line_timeout(conn, ticks)?;
    let line = match line {
        Some(line) => line,
        None => return Err(sys::Error::NotConnected),
    };
    let trimmed = line.trim_end_matches(|c| c == '\r' || c == '\n');
    if trimmed == expected {
        Ok(())
    } else {
        eprintln!("test_memcached: expected '{}' got '{}'", expected, trimmed);
        Err(sys::Error::Uncategorized)
    }
}

fn read_line_timeout(conn: &mut File, max_wait: usize) -> sys::Result<Option<String>> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut ch = [0u8; 1];
    let mut waited = 0usize;
    while bytes.len() < MAX_LINE {
        match conn.read(&mut ch) {
            Ok(0) => break,
            Ok(_) => {
                bytes.push(ch[0]);
                waited = 0;
                if ch[0] == b'\n' {
                    break;
                }
            }
            Err(sys::Error::WouldBlock) | Err(sys::Error::Interrupted) => {
                waited += 1;
                if waited >= max_wait {
                    return Ok(None);
                }
                let _ = sys::sleep(1);
            }
            Err(e) => return Err(e),
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() >= MAX_LINE && bytes.last() != Some(&b'\n') {
        return Err(sys::Error::InvalidArgument);
    }
    let line = core::str::from_utf8(&bytes).map_err(|_| sys::Error::Utf8Error)?;
    Ok(Some(line.to_string()))
}

fn spawn_client(key: &str) -> sys::Result<usize> {
    let pid = sys::fork()?;
    if pid == 0 {
        let ok = run_client_sequence(key).is_ok();
        sys::exit(if ok { 0 } else { 1 });
    }
    Ok(pid)
}

fn wait_all(mut pids: Vec<usize>, ticks: usize) -> bool {
    let mut ok = true;
    for _ in 0..ticks {
        if pids.is_empty() {
            return ok;
        }
        let mut idx = 0usize;
        while idx < pids.len() {
            let pid = pids[idx];
            let mut status = 0;
            match sys::waitpid(pid as isize, &mut status, signal::WNOHANG) {
                Ok(0) => {
                    idx += 1;
                }
                Ok(_) => {
                    if status != 0 {
                        ok = false;
                    }
                    pids.remove(idx);
                }
                Err(sys::Error::Interrupted) => {}
                Err(_) => {
                    ok = false;
                    pids.remove(idx);
                }
            }
        }
        let _ = sys::sleep(1);
    }
    for pid in pids {
        let _ = sys::kill(pid, signal::SIGTERM);
        let _ = wait_child(pid, CLIENT_TIMEOUT_TICKS);
    }
    false
}

fn wait_child(pid: usize, ticks: usize) -> Option<i32> {
    let mut status = 0;
    for _ in 0..ticks {
        match sys::waitpid(pid as isize, &mut status, signal::WNOHANG) {
            Ok(0) => {
                let _ = sys::sleep(1);
            }
            Ok(_) => return Some(status),
            Err(sys::Error::Interrupted) => {}
            Err(_) => return None,
        }
    }
    None
}

fn terminate_child(pid: usize) {
    let _ = sys::killpg(pid, signal::SIGTERM);
    if wait_child(pid, CLIENT_TIMEOUT_TICKS).is_some() {
        return;
    }
    let _ = sys::killpg(pid, signal::SIGKILL);
    let _ = wait_child(pid, CLIENT_TIMEOUT_TICKS);
}
