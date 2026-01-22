#![no_std]
extern crate alloc;

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

use ulib::{
    eprintln,
    fs::File,
    io::{Read, Write},
    mutex::Mutex,
    println, signal, socket, sys, sysinfo, thread,
};

const DEFAULT_PORT: u16 = 10001;
const DEFAULT_BUFFERS: usize = 4;
const MAX_LINE: usize = 4096;
const MAX_WORKERS: usize = 4;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

struct Entry {
    flags: u32,
    value: Vec<u8>,
}

struct Store {
    map: Mutex<BTreeMap<String, Entry>>,
}

impl Store {
    fn new() -> Self {
        Self {
            map: Mutex::new(BTreeMap::new()),
        }
    }
}

enum ArithOp {
    Incr,
    Decr,
    Mult,
    Div,
}

extern "C" fn worker_entry(listen_fd: usize, store_ptr: usize) {
    let store = unsafe { &*(store_ptr as *const Store) };
    worker_loop(listen_fd, store);
    sys::exit(0);
}

extern "C" fn term_handler(_sig: usize) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn worker_loop(listen_fd: usize, store: &Store) {
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }
        let mut conn = match sys::accept(listen_fd) {
            Ok(fd) => unsafe { File::from_raw_fd(fd) },
            Err(sys::Error::WouldBlock) => {
                let _ = sys::sleep(1);
                continue;
            }
            Err(sys::Error::Interrupted) => continue,
            Err(e) => {
                if SHUTDOWN.load(Ordering::SeqCst) {
                    sys::exit(0);
                }
                eprintln!("memcached: accept err={}", e);
                continue;
            }
        };
        let _ = conn.clear_nonblock();
        let _ = serve_client(&mut conn, store);
    }
}

fn main() -> sys::Result<()> {
    let store = Box::leak(Box::new(Store::new()));

    let _ = signal::signal(signal::SIGTERM, term_handler as *const () as usize);

    let mut server = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0)?;
    let addr = format!(":{}", DEFAULT_PORT);
    socket::bind(&server, &addr)?;
    socket::listen(&server, DEFAULT_BUFFERS)?;
    let _ = server.set_nonblock();
    println!("memcached: listen {}", addr);

    let listen_fd = server.get_fd();
    let store_ptr = store as *const Store as usize;
    let worker_count = sysinfo::get_nprocs().saturating_sub(1).min(MAX_WORKERS);
    for _ in 0..worker_count {
        if let Err(e) = thread::thread_create(worker_entry, listen_fd, store_ptr) {
            eprintln!("memcached: thread_create err={}", e);
            return Err(e);
        }
    }

    worker_loop(listen_fd, store);
    SHUTDOWN.store(true, Ordering::SeqCst);
    for _ in 0..worker_count {
        let _ = thread::thread_join();
    }
    Ok(())
}

fn serve_client(conn: &mut File, store: &Store) -> sys::Result<()> {
    loop {
        let line = match read_line(conn, MAX_LINE)? {
            Some(line) => line,
            None => break,
        };
        let line = line.trim_end_matches(|c| c == '\r' || c == '\n');
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let cmd = match parts.next() {
            Some(cmd) => cmd,
            None => continue,
        };
        match cmd {
            "set" => {
                if handle_set(conn, store, parts).is_err() {
                    write_client_error(conn, "bad command line format")?;
                }
            }
            "get" => {
                if handle_get(conn, store, parts).is_err() {
                    write_client_error(conn, "bad command line format")?;
                }
            }
            "incr" => handle_arith(conn, store, parts, ArithOp::Incr)?,
            "decr" => handle_arith(conn, store, parts, ArithOp::Decr)?,
            "mult" => handle_arith(conn, store, parts, ArithOp::Mult)?,
            "div" => handle_arith(conn, store, parts, ArithOp::Div)?,
            "quit" => break,
            _ => {
                write_error(conn)?;
            }
        }
    }
    Ok(())
}

fn handle_set<'a>(
    conn: &mut File,
    store: &Store,
    mut parts: impl Iterator<Item = &'a str>,
) -> sys::Result<()> {
    let key = parts.next().ok_or(sys::Error::InvalidArgument)?;
    let flags = parts
        .next()
        .ok_or(sys::Error::InvalidArgument)?
        .parse::<u32>()
        .map_err(|_| sys::Error::InvalidArgument)?;
    let _exptime = parts
        .next()
        .ok_or(sys::Error::InvalidArgument)?
        .parse::<i64>()
        .map_err(|_| sys::Error::InvalidArgument)?;
    let bytes = parts
        .next()
        .ok_or(sys::Error::InvalidArgument)?
        .parse::<usize>()
        .map_err(|_| sys::Error::InvalidArgument)?;
    if parts.next().is_some() {
        return Err(sys::Error::InvalidArgument);
    }

    let mut value = vec![0u8; bytes];
    read_exact(conn, &mut value)?;
    read_trailing_newline(conn)?;

    let mut guard = store.map.lock();
    guard.insert(key.to_string(), Entry { flags, value });
    conn.write_all(b"STORED\r\n")
}

fn handle_get<'a>(
    conn: &mut File,
    store: &Store,
    mut parts: impl Iterator<Item = &'a str>,
) -> sys::Result<()> {
    let key = parts.next().ok_or(sys::Error::InvalidArgument)?;
    if parts.next().is_some() {
        return Err(sys::Error::InvalidArgument);
    }
    let (flags, value) = {
        let guard = store.map.lock();
        match guard.get(key) {
            Some(entry) => (entry.flags, entry.value.clone()),
            None => {
                conn.write_all(b"END\r\n")?;
                return Ok(());
            }
        }
    };
    let header = format!("VALUE {} {} {}\r\n", key, flags, value.len());
    conn.write_all(header.as_bytes())?;
    conn.write_all(&value)?;
    conn.write_all(b"\r\nEND\r\n")
}

fn handle_arith<'a>(
    conn: &mut File,
    store: &Store,
    mut parts: impl Iterator<Item = &'a str>,
    op: ArithOp,
) -> sys::Result<()> {
    let key = match parts.next() {
        Some(key) => key,
        None => {
            write_client_error(conn, "bad command line format")?;
            return Ok(());
        }
    };
    let delta_str = match parts.next() {
        Some(delta) => delta,
        None => {
            write_client_error(conn, "bad command line format")?;
            return Ok(());
        }
    };
    if parts.next().is_some() {
        write_client_error(conn, "bad command line format")?;
        return Ok(());
    }
    let delta = match delta_str.parse::<i64>() {
        Ok(delta) => delta,
        Err(_) => {
            write_client_error(conn, "invalid numeric delta")?;
            return Ok(());
        }
    };
    if matches!(op, ArithOp::Div) && delta == 0 {
        write_client_error(conn, "invalid numeric delta")?;
        return Ok(());
    }

    let mut guard = store.map.lock();
    let entry = match guard.get_mut(key) {
        Some(entry) => entry,
        None => {
            conn.write_all(b"NOT_FOUND\r\n")?;
            return Ok(());
        }
    };
    let value_str = match core::str::from_utf8(&entry.value) {
        Ok(value) => value.trim(),
        Err(_) => {
            write_client_error(conn, "cannot increment or decrement non-numeric value")?;
            return Ok(());
        }
    };
    let current = match value_str.parse::<i64>() {
        Ok(val) => val,
        Err(_) => {
            write_client_error(conn, "cannot increment or decrement non-numeric value")?;
            return Ok(());
        }
    };
    let next = match op {
        ArithOp::Incr => current.checked_add(delta),
        ArithOp::Decr => current.checked_sub(delta),
        ArithOp::Mult => current.checked_mul(delta),
        ArithOp::Div => current.checked_div(delta),
    };
    let next = match next {
        Some(val) => val,
        None => {
            write_client_error(conn, "invalid numeric delta")?;
            return Ok(());
        }
    };
    let next_str = next.to_string();
    entry.value = next_str.as_bytes().to_vec();
    conn.write_all(next_str.as_bytes())?;
    conn.write_all(b"\r\n")
}

fn read_line(conn: &mut File, max_len: usize) -> sys::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut ch = [0u8; 1];
    while bytes.len() < max_len {
        let n = conn.read(&mut ch)?;
        if n == 0 {
            break;
        }
        bytes.push(ch[0]);
        if ch[0] == b'\n' {
            break;
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() >= max_len && bytes.last() != Some(&b'\n') {
        return Err(sys::Error::InvalidArgument);
    }
    let line = core::str::from_utf8(&bytes).map_err(|_| sys::Error::Utf8Error)?;
    Ok(Some(line.to_string()))
}

fn read_exact(conn: &mut File, buf: &mut [u8]) -> sys::Result<()> {
    let mut offset = 0usize;
    while offset < buf.len() {
        let n = conn.read(&mut buf[offset..])?;
        if n == 0 {
            return Err(sys::Error::NotConnected);
        }
        offset += n;
    }
    Ok(())
}

fn read_trailing_newline(conn: &mut File) -> sys::Result<()> {
    let mut ch = [0u8; 1];
    let n = conn.read(&mut ch)?;
    if n == 0 {
        return Err(sys::Error::NotConnected);
    }
    if ch[0] == b'\r' {
        let n2 = conn.read(&mut ch)?;
        if n2 == 0 {
            return Err(sys::Error::NotConnected);
        }
        if ch[0] != b'\n' {
            return Err(sys::Error::InvalidArgument);
        }
    } else if ch[0] != b'\n' {
        return Err(sys::Error::InvalidArgument);
    }
    Ok(())
}

fn write_client_error(conn: &mut File, msg: &str) -> sys::Result<()> {
    let line = format!("CLIENT_ERROR {}\r\n", msg);
    conn.write_all(line.as_bytes())
}

fn write_error(conn: &mut File) -> sys::Result<()> {
    conn.write_all(b"ERROR\r\n")
}
