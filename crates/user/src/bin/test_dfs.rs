#![no_std]
extern crate alloc;

use alloc::{format, string::String, vec::Vec};

use ulib::{
    ExitCode, eprintln,
    fs::{self, File},
    io::{Read, Write},
    println,
    process::Command,
    signal, sys,
};

const TEST_DIR_PREFIX: &str = "/dfs/dfstest";
const TEST_FILE_NAME: &str = "hello.txt";
const PAYLOAD: &[u8] = b"hello";
const MISSING_PATH: &str = "/dfs/dfstest-missing";

const READY_RETRIES: usize = 20;
const READY_SLEEP_TICKS: usize = 1;
const READY_TIMEOUT_TICKS: usize = 30;
const CLIENT_TIMEOUT_TICKS: usize = 200;

const PARALLEL_CLIENTS: usize = 4;

fn main() -> ExitCode {
    println!("test_dfs: start");
    let mut cmd = Command::new("dfs_server");
    cmd.pgrp(0);
    let server = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("test_dfs: spawn err={}", e);
            return ExitCode::FAILURE;
        }
    };

    let mut ok = true;
    if !wait_for_ready() {
        eprintln!("test_dfs: server not ready");
        ok = false;
    }
    if ok && !run_parallel_clients() {
        eprintln!("test_dfs: parallel clients failed");
        ok = false;
    }
    if ok && !run_with_timeout(CLIENT_TIMEOUT_TICKS, run_missing) {
        eprintln!("test_dfs: missing file check failed");
        ok = false;
    }

    terminate_child(server.pid());

    if ok {
        println!("test_dfs: OK");
        ExitCode::SUCCESS
    } else {
        println!("test_dfs: FAIL");
        ExitCode::FAILURE
    }
}

fn wait_for_ready() -> bool {
    for _ in 0..READY_RETRIES {
        if run_with_timeout(READY_TIMEOUT_TICKS, || match File::open(MISSING_PATH) {
            Err(sys::Error::NotFound) => Ok(()),
            Err(e) => Err(e),
            Ok(_) => Err(sys::Error::Uncategorized),
        }) {
            return true;
        }
        let _ = sys::sleep(READY_SLEEP_TICKS);
    }
    false
}

fn run_parallel_clients() -> bool {
    let mut pids = Vec::new();
    for idx in 0..PARALLEL_CLIENTS {
        match spawn_client(idx) {
            Ok(pid) => pids.push(pid),
            Err(e) => {
                eprintln!("test_dfs: spawn client err={}", e);
                return false;
            }
        }
    }
    wait_all(pids, CLIENT_TIMEOUT_TICKS)
}

fn run_client(idx: usize) -> sys::Result<()> {
    let dir = format!("{}-{}", TEST_DIR_PREFIX, idx);
    let path = format!("{}/{}", dir, TEST_FILE_NAME);
    match fs::create_dir(dir.as_str()) {
        Ok(()) | Err(sys::Error::AlreadyExists) => {}
        Err(e) => return Err(e),
    }

    {
        let mut file = File::create(path.as_str())?;
        file.write_all(PAYLOAD)?;
        file.sync()?;
    }

    let mut file = File::open(path.as_str())?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    if data.as_slice() != PAYLOAD {
        eprintln!(
            "test_dfs: read mismatch idx={} got={} bytes={:?}",
            idx,
            data.len(),
            data.as_slice()
        );
        return Err(sys::Error::Uncategorized);
    }

    let _ = fs::remove_file(path.as_str());
    let _ = fs::remove_file(dir.as_str());
    Ok(())
}

fn run_missing() -> sys::Result<()> {
    match File::open(MISSING_PATH) {
        Err(sys::Error::NotFound) => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(sys::Error::Uncategorized),
    }
}

fn spawn_client(idx: usize) -> sys::Result<usize> {
    let pid = sys::fork()?;
    if pid == 0 {
        let ok = run_client(idx).is_ok();
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
                eprintln!("test_dfs: child status={}", status);
            }
            status == 0
        }
        None => {
            eprintln!("test_dfs: child timeout");
            let _ = sys::kill(pid, signal::SIGTERM);
            let _ = wait_child(pid, ticks);
            false
        }
    }
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
