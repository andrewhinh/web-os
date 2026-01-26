#![no_std]
extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

use kernel::defs::{AsBytes, FromBytes};
use kernel::dfs::{
    DFS_HOST, DFS_MAGIC, DFS_PORT_BASE, DFS_PORT_STRIDE, DFS_PORT_TRIES, DfsOp, DfsReq, DfsResp,
};
use kernel::error::Error;
use ulib::fs::File;
use ulib::io::{Read, Write};
use ulib::{env, eprintln, println, signal, socket, sys, sysinfo, thread};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
const MAX_WORKERS: usize = 4;

extern "C" fn worker_entry(listen_fd: usize, _unused: usize) {
    worker_loop(listen_fd);
    sys::exit(0);
}

extern "C" fn term_handler(_sig: usize) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

struct HandleTable {
    next: u32,
    files: BTreeMap<u32, File>,
}

impl HandleTable {
    fn new() -> Self {
        Self {
            next: 1,
            files: BTreeMap::new(),
        }
    }

    fn insert(&mut self, file: File) -> u32 {
        let handle = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        self.files.insert(handle, file);
        handle
    }

    fn get_mut(&mut self, handle: u32) -> Option<&mut File> {
        self.files.get_mut(&handle)
    }

    fn remove(&mut self, handle: u32) -> Option<File> {
        self.files.remove(&handle)
    }
}

fn worker_loop(listen_fd: usize) {
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
                eprintln!("dfs_server: accept err={}", e);
                continue;
            }
        };
        let _ = conn.set_nonblock();
        if let Err(e) = handle_conn(conn) {
            if SHUTDOWN.load(Ordering::SeqCst) {
                sys::exit(0);
            }
            eprintln!("dfs_server: conn err={}", e);
        }
    }
}

fn main() {
    let seat_id = env::var("SEAT_ID")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let base = DFS_PORT_BASE as usize;
    let stride = DFS_PORT_STRIDE as usize;
    let mut chosen = None;
    let mut last_err = None;
    for offset in 0..DFS_PORT_TRIES {
        let port = base + seat_id.saturating_add(offset).saturating_mul(stride);
        let addr = alloc::format!("{}:{}", DFS_HOST, port);
        let mut server = match socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0) {
            Ok(sock) => sock,
            Err(e) => {
                eprintln!("dfs_server: socket err={}", e);
                return;
            }
        };
        match socket::bind(&server, addr.as_str()) {
            Ok(()) => {
                if let Err(e) = socket::listen(&server, 8) {
                    eprintln!("dfs_server: listen err={}", e);
                    return;
                }
                let _ = server.set_nonblock();
                chosen = Some((addr, server));
                break;
            }
            Err(sys::Error::ResourceBusy) => {
                last_err = Some(sys::Error::ResourceBusy);
                continue;
            }
            Err(e) => {
                eprintln!("dfs_server: bind err={}", e);
                return;
            }
        }
    }
    let Some((addr, mut server)) = chosen else {
        eprintln!(
            "dfs_server: bind err={}",
            last_err.unwrap_or(sys::Error::ResourceBusy)
        );
        return;
    };
    println!("dfs_server: bind {}", addr);
    let _ = signal::signal(signal::SIGTERM, term_handler as *const () as usize);

    let listen_fd = server.get_fd();
    let worker_count = sysinfo::get_nprocs().saturating_sub(1).min(MAX_WORKERS);
    for _ in 0..worker_count {
        if let Err(e) = thread::thread_create(worker_entry, listen_fd, 0) {
            eprintln!("dfs_server: thread_create err={}", e);
            return;
        }
    }

    worker_loop(listen_fd);
    SHUTDOWN.store(true, Ordering::SeqCst);
    for _ in 0..worker_count {
        let _ = thread::thread_join();
    }
}

fn read_exact(file: &mut File, buf: &mut [u8]) -> sys::Result<bool> {
    let mut off = 0usize;
    while off < buf.len() {
        if SHUTDOWN.load(Ordering::SeqCst) {
            return Err(Error::Interrupted);
        }
        match file.read(&mut buf[off..]) {
            Ok(0) => return Ok(false),
            Ok(n) => off += n,
            Err(Error::WouldBlock) => {
                let _ = sys::sleep(1);
            }
            Err(Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn write_all(file: &mut File, buf: &[u8]) -> sys::Result<()> {
    let mut off = 0usize;
    while off < buf.len() {
        match file.write(&buf[off..]) {
            Ok(0) => return Err(Error::WriteZero),
            Ok(n) => off += n,
            Err(Error::WouldBlock) => {
                let _ = sys::sleep(1);
            }
            Err(Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn read_bytes(file: &mut File, len: usize) -> sys::Result<Vec<u8>> {
    let mut buf = alloc::vec![0u8; len];
    if !read_exact(file, &mut buf)? {
        return Err(Error::NotConnected);
    }
    Ok(buf)
}

fn send_resp(file: &mut File, status: i32, handle: u32, data: Option<&[u8]>) -> sys::Result<()> {
    let len = data.map(|d| d.len()).unwrap_or(0);
    let resp = DfsResp {
        magic: DFS_MAGIC,
        status,
        handle,
        len: len as u32,
    };
    write_all(file, resp.as_bytes())?;
    if let Some(data) = data {
        write_all(file, data)?;
    }
    Ok(())
}

fn handle_conn(mut conn: File) -> sys::Result<()> {
    let mut handles = HandleTable::new();
    loop {
        let mut hdr = [0u8; size_of::<DfsReq>()];
        let ok = read_exact(&mut conn, &mut hdr)?;
        if !ok {
            return Ok(());
        }
        let Some(req) = DfsReq::read_from(&hdr) else {
            return Err(Error::InvalidArgument);
        };
        if req.magic != DFS_MAGIC {
            return Err(Error::InvalidArgument);
        }

        match req.op {
            x if x == DfsOp::Open as u16 => {
                let path = read_bytes(&mut conn, req.len as usize)?;
                let path = core::str::from_utf8(&path).map_err(|_| Error::Utf8Error)?;
                let fd = match sys::open(path, req.flags as usize) {
                    Ok(fd) => fd,
                    Err(e) => {
                        let _ = send_resp(&mut conn, e as i32, 0, None);
                        continue;
                    }
                };
                let file = unsafe { File::from_raw_fd(fd) };
                let handle = handles.insert(file);
                send_resp(&mut conn, 0, handle, None)?;
            }
            x if x == DfsOp::Read as u16 => {
                let Some(file) = handles.get_mut(req.handle) else {
                    send_resp(&mut conn, Error::BadFileDescriptor as i32, 0, None)?;
                    continue;
                };
                let mut buf = alloc::vec![0u8; req.len as usize];
                match file.read(&mut buf) {
                    Ok(n) => {
                        send_resp(&mut conn, n as i32, req.handle, Some(&buf[..n]))?;
                    }
                    Err(e) => {
                        send_resp(&mut conn, e as i32, 0, None)?;
                    }
                }
            }
            x if x == DfsOp::Write as u16 => {
                let Some(file) = handles.get_mut(req.handle) else {
                    send_resp(&mut conn, Error::BadFileDescriptor as i32, 0, None)?;
                    continue;
                };
                let data = read_bytes(&mut conn, req.len as usize)?;
                match file.write(&data) {
                    Ok(n) => {
                        send_resp(&mut conn, n as i32, req.handle, None)?;
                    }
                    Err(e) => {
                        send_resp(&mut conn, e as i32, 0, None)?;
                    }
                }
            }
            x if x == DfsOp::Close as u16 => {
                if handles.remove(req.handle).is_none() {
                    send_resp(&mut conn, Error::BadFileDescriptor as i32, 0, None)?;
                } else {
                    send_resp(&mut conn, 0, 0, None)?;
                }
            }
            x if x == DfsOp::Stat as u16 => {
                let Some(file) = handles.get_mut(req.handle) else {
                    send_resp(&mut conn, Error::BadFileDescriptor as i32, 0, None)?;
                    continue;
                };
                match file.stat() {
                    Ok(stat) => {
                        send_resp(&mut conn, 0, req.handle, Some(stat.as_bytes()))?;
                    }
                    Err(e) => {
                        send_resp(&mut conn, e as i32, 0, None)?;
                    }
                }
            }
            x if x == DfsOp::Mkdir as u16 => {
                let path = read_bytes(&mut conn, req.len as usize)?;
                let path = core::str::from_utf8(&path).map_err(|_| Error::Utf8Error)?;
                match sys::mkdir(path) {
                    Ok(()) => send_resp(&mut conn, 0, 0, None)?,
                    Err(e) => send_resp(&mut conn, e as i32, 0, None)?,
                }
            }
            x if x == DfsOp::Unlink as u16 => {
                let path = read_bytes(&mut conn, req.len as usize)?;
                let path = core::str::from_utf8(&path).map_err(|_| Error::Utf8Error)?;
                match sys::unlink(path) {
                    Ok(()) => send_resp(&mut conn, 0, 0, None)?,
                    Err(e) => send_resp(&mut conn, e as i32, 0, None)?,
                }
            }
            x if x == DfsOp::Link as u16 => {
                let oldp = read_bytes(&mut conn, req.len as usize)?;
                let newp = read_bytes(&mut conn, req.aux as usize)?;
                let oldp = core::str::from_utf8(&oldp).map_err(|_| Error::Utf8Error)?;
                let newp = core::str::from_utf8(&newp).map_err(|_| Error::Utf8Error)?;
                match sys::link(oldp, newp) {
                    Ok(()) => send_resp(&mut conn, 0, 0, None)?,
                    Err(e) => send_resp(&mut conn, e as i32, 0, None)?,
                }
            }
            x if x == DfsOp::Symlink as u16 => {
                let target = read_bytes(&mut conn, req.len as usize)?;
                let linkp = read_bytes(&mut conn, req.aux as usize)?;
                let target = core::str::from_utf8(&target).map_err(|_| Error::Utf8Error)?;
                let linkp = core::str::from_utf8(&linkp).map_err(|_| Error::Utf8Error)?;
                match sys::symlink(target, linkp) {
                    Ok(()) => send_resp(&mut conn, 0, 0, None)?,
                    Err(e) => send_resp(&mut conn, e as i32, 0, None)?,
                }
            }
            x if x == DfsOp::Fsync as u16 => {
                let Some(file) = handles.get_mut(req.handle) else {
                    send_resp(&mut conn, Error::BadFileDescriptor as i32, 0, None)?;
                    continue;
                };
                match file.sync() {
                    Ok(()) => send_resp(&mut conn, 0, 0, None)?,
                    Err(e) => send_resp(&mut conn, e as i32, 0, None)?,
                }
            }
            _ => {
                send_resp(&mut conn, Error::InvalidArgument as i32, 0, None)?;
            }
        }
    }
}
