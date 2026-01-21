#![no_std]
extern crate alloc;

use alloc::{format, string::String, vec::Vec};

use ulib::{
    ExitCode, eprintln,
    fs::File,
    io::{Read, Write},
    println,
    process::Command,
    signal, socket, sys,
};

const SERVER_IP: &str = "10.0.2.15";
const SERVER_PORT: u16 = 10000;
const TEST_PATH_OK: &str = "/etc/paths";
const TEST_PATH_MISS: &str = "/nope";

const READY_RETRIES: usize = 20;
const READY_SLEEP_TICKS: usize = 1;
const READY_TIMEOUT_TICKS: usize = 30;
const CLIENT_TIMEOUT_TICKS: usize = 200;
const WRITE_TIMEOUT_TICKS: usize = 50;
const READ_TIMEOUT_TICKS: usize = 200;

const PARALLEL_CLIENTS: usize = 4;

fn main() -> ExitCode {
    println!("test_wserver: start");
    let mut cmd = Command::new("wserver");
    let server = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("test_wserver: spawn err={}", e);
            return ExitCode::FAILURE;
        }
    };

    let mut ok = true;
    if !wait_for_ready() {
        eprintln!("test_wserver: server not ready");
        ok = false;
    }

    if ok && !run_parallel_clients() {
        eprintln!("test_wserver: parallel clients failed");
        ok = false;
    }
    if ok
        && !run_with_timeout(CLIENT_TIMEOUT_TICKS, || {
            run_client_expect(TEST_PATH_MISS, "HTTP/1.0 404")
        })
    {
        eprintln!("test_wserver: 404 check failed");
        ok = false;
    }

    terminate_child(server.pid());

    if ok {
        println!("test_wserver: OK");
        ExitCode::SUCCESS
    } else {
        println!("test_wserver: FAIL");
        ExitCode::FAILURE
    }
}

fn wait_for_ready() -> bool {
    for _ in 0..READY_RETRIES {
        if run_with_timeout(READY_TIMEOUT_TICKS, || {
            run_client_expect(TEST_PATH_OK, "HTTP/1.0 200")
        }) {
            return true;
        }
        let _ = sys::sleep(READY_SLEEP_TICKS);
    }
    false
}

fn run_parallel_clients() -> bool {
    let mut pids = Vec::new();
    for _ in 0..PARALLEL_CLIENTS {
        match spawn_client(TEST_PATH_OK, "HTTP/1.0 200") {
            Ok(pid) => pids.push(pid),
            Err(e) => {
                eprintln!("test_wserver: spawn client err={}", e);
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
        Some(status) => {
            if status != 0 {
                eprintln!("test_wserver: child status={}", status);
            }
            status == 0
        }
        None => {
            eprintln!("test_wserver: child timeout");
            let _ = sys::kill(pid, signal::SIGTERM);
            let _ = wait_child(pid, ticks);
            false
        }
    }
}

fn run_client_expect(path: &str, expect_prefix: &str) -> sys::Result<()> {
    let mut conn = match connect_retry() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("test_wserver: connect err={}", e);
            return Err(e);
        }
    };
    let _ = conn.set_nonblock();
    send_request(&mut conn, path, WRITE_TIMEOUT_TICKS)?;
    let status = read_status_line(&mut conn, READ_TIMEOUT_TICKS)?;
    if !status.starts_with(expect_prefix) {
        eprintln!("test_wserver: status={}", status);
        return Err(sys::Error::Uncategorized);
    }
    Ok(())
}

fn spawn_client(path: &str, expect_prefix: &str) -> sys::Result<usize> {
    let pid = sys::fork()?;
    if pid == 0 {
        let ok = run_client_expect(path, expect_prefix).is_ok();
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

fn send_request(conn: &mut File, path: &str, ticks: usize) -> sys::Result<()> {
    let req = format!("GET {} HTTP/1.0\r\n\r\n", path);
    let bytes = req.as_bytes();
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

fn read_status_line(conn: &mut File, ticks: usize) -> sys::Result<String> {
    let mut line = Vec::new();
    let mut buf = [0u8; 128];
    for _ in 0..ticks {
        match conn.read(&mut buf) {
            Ok(0) => {
                let _ = sys::sleep(1);
            }
            Ok(n) => {
                for b in &buf[..n] {
                    line.push(*b);
                    if *b == b'\n' {
                        let raw = core::str::from_utf8(&line).map_err(|_| sys::Error::Utf8Error)?;
                        let trimmed = raw.trim_end_matches(['\r', '\n']);
                        return Ok(String::from(trimmed));
                    }
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

fn connect_retry() -> sys::Result<File> {
    let addr = format!("{}:{}", SERVER_IP, SERVER_PORT);
    let mut last_err = sys::Error::NotConnected;
    for _ in 0..READY_RETRIES {
        let conn = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0)?;
        match socket::connect(&conn, &addr) {
            Ok(()) => return Ok(conn),
            Err(e) => {
                last_err = e;
                drop(conn);
                let _ = sys::sleep(READY_SLEEP_TICKS);
            }
        }
    }
    Err(last_err)
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
    let _ = sys::kill(pid, signal::SIGTERM);
    if wait_child(pid, CLIENT_TIMEOUT_TICKS).is_some() {
        return;
    }
    let _ = sys::kill(pid, signal::SIGKILL);
    let _ = wait_child(pid, CLIENT_TIMEOUT_TICKS);
}
