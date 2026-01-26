#![no_std]
extern crate alloc;

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::cmp::min;
use core::sync::atomic::{AtomicBool, Ordering};

use ulib::{
    ExitCode, env, eprintln,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    println, signal, socket, sys, sysinfo, thread,
};

const DEFAULT_PORT: u16 = 10000;
const PORT_STRIDE: u16 = 3000;
const PORT_TRIES: usize = 8;
const DEFAULT_BUFFERS: usize = 4;
const MAX_REQUEST_LINE: usize = 4096;
const MAX_WORKERS: usize = 4;

static SHUTDOWN: AtomicBool = AtomicBool::new(false); // per-thread

#[derive(Clone, Copy, Debug)]
enum SchedAlg {
    Fifo,
    Sff,
}

impl SchedAlg {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "FIFO" => Some(Self::Fifo),
            "SFF" => Some(Self::Sff),
            _ => None,
        }
    }
}

struct Config {
    basedir: String,
    port: u16,
    buffers: usize,
    sched: SchedAlg,
}

enum RequestKind {
    File { path: PathBuf, size: usize },
    BadRequest,
    NotFound,
}

struct Request {
    conn: File,
    kind: RequestKind,
}

extern "C" fn worker_entry(listen_fd: usize, cfg_ptr: usize) {
    let cfg = unsafe { &*(cfg_ptr as *const Config) };
    worker_loop(listen_fd, cfg);
    sys::exit(0);
}

extern "C" fn term_handler(_sig: usize) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn worker_loop(listen_fd: usize, cfg: &Config) {
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
                eprintln!("wserver: accept err={}", e);
                continue;
            }
        };
        let _ = conn.clear_nonblock();
        let req = parse_request(conn, &cfg.basedir);
        handle_request(req);
    }
}

fn main() -> ExitCode {
    let mut cfg = match parse_args() {
        Ok(cfg) => cfg,
        Err(()) => {
            eprintln!("usage: wserver [-d basedir] [-p port] [-b buffers] [-s schedalg]");
            return ExitCode::FAILURE;
        }
    };

    let _ = signal::signal(signal::SIGTERM, term_handler as *const () as usize);

    let mut server = match socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0) {
        Ok(sock) => sock,
        Err(e) => {
            eprintln!("wserver: socket err={}", e);
            return ExitCode::FAILURE;
        }
    };
    let seat_id = env::var("SEAT_ID")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let base = cfg.port as usize;
    let stride = PORT_STRIDE as usize;
    let mut bound = None;
    for offset in 0..PORT_TRIES {
        let port = base + seat_id.saturating_add(offset).saturating_mul(stride);
        if port > u16::MAX as usize {
            break;
        }
        let addr = format!(":{}", port);
        match socket::bind(&server, &addr) {
            Ok(()) => {
                if let Err(e) = socket::listen(&server, cfg.buffers) {
                    eprintln!("wserver: listen err={}", e);
                    return ExitCode::FAILURE;
                }
                cfg.port = port as u16;
                bound = Some(addr);
                break;
            }
            Err(sys::Error::ResourceBusy) => continue,
            Err(e) => {
                eprintln!("wserver: bind {} err={}", addr, e);
                return ExitCode::FAILURE;
            }
        }
    }
    if bound.is_none() {
        eprintln!("wserver: bind err={}", sys::Error::ResourceBusy);
        return ExitCode::FAILURE;
    }
    let _ = server.set_nonblock();

    let cfg = Box::leak(Box::new(cfg));
    let cfg_ptr = cfg as *const Config as usize;
    let listen_fd = server.get_fd();
    let worker_count = sysinfo::get_nprocs().saturating_sub(1).min(MAX_WORKERS);
    for _ in 0..worker_count {
        if let Err(e) = thread::thread_create(worker_entry, listen_fd, cfg_ptr) {
            eprintln!("wserver: thread_create err={}", e);
            return ExitCode::FAILURE;
        }
    }

    println!(
        "wserver: base={} port={} buffers={} sched={}",
        cfg.basedir,
        cfg.port,
        cfg.buffers,
        match cfg.sched {
            SchedAlg::Fifo => "FIFO",
            SchedAlg::Sff => "SFF",
        }
    );

    worker_loop(listen_fd, cfg);
    SHUTDOWN.store(true, Ordering::SeqCst);
    for _ in 0..worker_count {
        let _ = thread::thread_join();
    }
    ExitCode::SUCCESS
}

fn parse_args() -> Result<Config, ()> {
    let mut args = env::args();
    let _ = args.next();
    let mut cfg = Config {
        basedir: ".".to_string(),
        port: DEFAULT_PORT,
        buffers: DEFAULT_BUFFERS,
        sched: SchedAlg::Fifo,
    };
    while let Some(arg) = args.next() {
        match arg {
            "-d" => {
                cfg.basedir = args.next().ok_or(())?.to_string();
            }
            "-p" => {
                let value = args.next().ok_or(())?;
                cfg.port = value.parse::<u16>().map_err(|_| ())?;
            }
            "-b" => {
                let value = args.next().ok_or(())?;
                cfg.buffers = value.parse::<usize>().map_err(|_| ())?;
                if cfg.buffers == 0 {
                    return Err(());
                }
            }
            "-s" => {
                let value = args.next().ok_or(())?;
                cfg.sched = SchedAlg::parse(value).ok_or(())?;
            }
            _ => return Err(()),
        }
    }
    Ok(cfg)
}

fn parse_request(conn: File, basedir: &str) -> Request {
    let mut conn = conn;
    let line = match read_request_line(&mut conn) {
        Ok(line) => line,
        Err(e) => {
            eprintln!("wserver: read request line err={}", e);
            return Request {
                conn,
                kind: RequestKind::BadRequest,
            };
        }
    };
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.split_whitespace();
    let method = match parts.next() {
        Some(method) => method,
        None => {
            return Request {
                conn,
                kind: RequestKind::BadRequest,
            };
        }
    };
    let raw_path = match parts.next() {
        Some(path) => path,
        None => {
            return Request {
                conn,
                kind: RequestKind::BadRequest,
            };
        }
    };
    if method != "GET" {
        eprintln!("wserver: method not supported {}", method);
        return Request {
            conn,
            kind: RequestKind::BadRequest,
        };
    }
    let rel_path = match sanitize_path(raw_path) {
        Some(path) => path,
        None => {
            eprintln!("wserver: bad path {}", raw_path);
            return Request {
                conn,
                kind: RequestKind::BadRequest,
            };
        }
    };

    let mut full_path = PathBuf::from(basedir.to_string());
    full_path.push(rel_path.as_str());

    match fs::metadata(full_path.as_path()) {
        Ok(meta) if meta.is_file() => {
            println!("wserver: serve {}", full_path.as_path().to_str());
            Request {
                conn,
                kind: RequestKind::File {
                    path: full_path,
                    size: meta.len(),
                },
            }
        }
        _ => {
            eprintln!("wserver: not found {}", full_path.as_path().to_str());
            Request {
                conn,
                kind: RequestKind::NotFound,
            }
        }
    }
}

fn read_request_line(conn: &mut File) -> sys::Result<String> {
    let mut bytes = Vec::new();
    let mut ch = [0u8; 1];
    while bytes.len() < MAX_REQUEST_LINE {
        match conn.read(&mut ch) {
            Ok(0) => break,
            Ok(_) => {
                bytes.push(ch[0]);
                if ch[0] == b'\n' {
                    break;
                }
            }
            Err(sys::Error::WouldBlock) => {
                let _ = sys::sleep(1);
            }
            Err(sys::Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    if bytes.is_empty() {
        return Err(sys::Error::NotConnected);
    }
    let line = core::str::from_utf8(&bytes).map_err(|_| sys::Error::Utf8Error)?;
    Ok(line.to_string())
}

fn sanitize_path(raw: &str) -> Option<String> {
    let mut path = raw;
    if let Some((left, _)) = raw.split_once('?') {
        path = left;
    }
    if !path.starts_with('/') {
        return None;
    }
    while path.starts_with('/') {
        path = &path[1..];
    }
    if path.is_empty() {
        return Some("index.html".to_string());
    }
    for comp in Path::new(path).components() {
        if matches!(comp, Component::ParentDir) {
            return None;
        }
    }
    Some(path.to_string())
}

fn handle_request(req: Request) {
    let mut conn = req.conn;
    match req.kind {
        RequestKind::BadRequest => {
            let _ = write_header(&mut conn, 400, "Bad Request", 0);
        }
        RequestKind::NotFound => {
            let _ = write_header(&mut conn, 404, "Not Found", 0);
        }
        RequestKind::File { path, size } => {
            let mut file = match File::open(path.as_path()) {
                Ok(file) => file,
                Err(_) => {
                    let _ = write_header(&mut conn, 404, "Not Found", 0);
                    return;
                }
            };
            if write_header(&mut conn, 200, "OK", size).is_err() {
                return;
            }
            let mut buf = [0u8; 512];
            let mut remaining = size;
            while remaining > 0 {
                let chunk = min(remaining, buf.len());
                let n = match file.read(&mut buf[..chunk]) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                if write_all_retry(&mut conn, &buf[..n]).is_err() {
                    break;
                }
                remaining = remaining.saturating_sub(n);
            }
        }
    }
}

fn write_header(conn: &mut File, code: u16, reason: &str, len: usize) -> sys::Result<()> {
    let header = format!(
        "HTTP/1.0 {} {}\r\nContent-Length: {}\r\n\r\n",
        code, reason, len
    );
    write_all_retry(conn, header.as_bytes())
}

fn write_all_retry(conn: &mut File, mut buf: &[u8]) -> sys::Result<()> {
    while !buf.is_empty() {
        match conn.write(buf) {
            Ok(0) => return Err(sys::Error::WriteZero),
            Ok(n) => buf = &buf[n..],
            Err(sys::Error::WouldBlock) => {
                let _ = sys::sleep(1);
            }
            Err(sys::Error::Interrupted) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
