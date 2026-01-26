use crate::defs::{AsBytes, FromBytes};

pub const DFS_MAGIC: u32 = 0x4446_5331; // "DFS1"
pub const DFS_PREFIX: &str = "/dfs";
pub const DFS_PREFIX_DIR: &str = "/dfs/";
pub const DFS_HOST: &str = "10.0.2.15";
pub const DFS_PORT_BASE: u16 = 7000;
pub const DFS_PORT_STRIDE: u16 = 3000;
pub const DFS_PORT_TRIES: usize = 8;
pub const DFS_MAX_CHUNK: usize = 512;

#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DfsOp {
    Open = 1,
    Read = 2,
    Write = 3,
    Close = 4,
    Stat = 5,
    Mkdir = 6,
    Unlink = 7,
    Link = 8,
    Symlink = 9,
    Fsync = 10,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DfsReq {
    pub magic: u32,
    pub op: u16,
    pub _pad: u16,
    pub flags: u32,
    pub handle: u32,
    pub len: u32,
    pub aux: u32,
}

impl DfsReq {
    pub fn new(op: DfsOp, flags: u32, handle: u32, len: u32, aux: u32) -> Self {
        Self {
            magic: DFS_MAGIC,
            op: op as u16,
            _pad: 0,
            flags,
            handle,
            len,
            aux,
        }
    }
}

unsafe impl AsBytes for DfsReq {}
unsafe impl FromBytes for DfsReq {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DfsResp {
    pub magic: u32,
    pub status: i32,
    pub handle: u32,
    pub len: u32,
}

impl DfsResp {
    pub fn ok(handle: u32, len: u32) -> Self {
        Self {
            magic: DFS_MAGIC,
            status: 0,
            handle,
            len,
        }
    }
}

unsafe impl AsBytes for DfsResp {}
unsafe impl FromBytes for DfsResp {}

#[cfg(all(target_os = "none", feature = "kernel"))]
mod client {
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cmp::min;
    use core::mem::size_of;

    use super::{
        DFS_HOST, DFS_MAGIC, DFS_MAX_CHUNK, DFS_PORT_BASE, DFS_PORT_STRIDE, DFS_PORT_TRIES,
        DFS_PREFIX, DFS_PREFIX_DIR, DfsOp, DfsReq, DfsResp,
    };
    use crate::defs::{AsBytes, FromBytes};
    use crate::error::{Error::*, Result};
    use crate::fs::Path;
    use crate::proc::Cpus;
    use crate::proc::{either_copyin, either_copyout};
    use crate::sleeplock::SleepLock;
    use crate::socket::{InetSocket, SOCK_STREAM};
    use crate::spinlock::Mutex;
    use crate::stat::Stat;
    use crate::sync::LazyLock;
    use crate::trap::TICKS;
    use crate::vm::VirtAddr;

    struct ClientState {
        sockets: BTreeMap<u16, Arc<InetSocket>>,
    }

    static CLIENT: LazyLock<Mutex<ClientState>> = LazyLock::new(|| {
        Mutex::new(
            ClientState {
                sockets: BTreeMap::new(),
            },
            "dfs",
        )
    });
    static RPC_LOCK: LazyLock<SleepLock<()>> = LazyLock::new(|| SleepLock::new((), "dfs_rpc"));

    fn seat_id() -> usize {
        Cpus::myproc().map(|p| p.inner.lock().seat_id).unwrap_or(0)
    }

    fn dfs_port_candidates() -> [u16; DFS_PORT_TRIES] {
        let seat = seat_id();
        let stride = DFS_PORT_STRIDE as usize;
        let base = DFS_PORT_BASE as usize;
        let mut ports = [0u16; DFS_PORT_TRIES];
        for (i, port) in ports.iter_mut().enumerate().take(DFS_PORT_TRIES) {
            *port = (base + (seat.saturating_add(i)).saturating_mul(stride)).min(u16::MAX as usize)
                as u16;
        }
        ports
    }

    fn dfs_addr(port: u16) -> String {
        alloc::format!("{}:{}", DFS_HOST, port)
    }

    fn reset_socket(port: u16) {
        CLIENT.lock().sockets.remove(&port);
    }

    fn get_socket() -> Result<(u16, Arc<InetSocket>)> {
        let ports = dfs_port_candidates();
        let mut last_err = None;
        for &port in &ports {
            {
                let guard = CLIENT.lock();
                if let Some(sock) = guard.sockets.get(&port) {
                    return Ok((port, Arc::clone(sock)));
                }
            }
            let sock = InetSocket::new(SOCK_STREAM);
            let addr = dfs_addr(port);
            match sock.connect(addr.as_str(), false) {
                Ok(()) => {
                    let mut guard = CLIENT.lock();
                    if let Some(existing) = guard.sockets.get(&port) {
                        return Ok((port, Arc::clone(existing)));
                    }
                    guard.sockets.insert(port, Arc::clone(&sock));
                    return Ok((port, sock));
                }
                Err(e) => {
                    last_err = Some(e);
                    reset_socket(port);
                }
            }
        }
        Err(last_err.unwrap_or(NotConnected))
    }

    fn call(req: &DfsReq, payloads: &[&[u8]]) -> Result<(DfsResp, Vec<u8>)> {
        let wait_start = *TICKS.lock();
        let guard = RPC_LOCK.lock();
        let wait_end = *TICKS.lock();
        let wait_ticks = wait_end.saturating_sub(wait_start);
        let (port, sock) = get_socket()?;
        if wait_ticks >= 50 {
            println!(
                "DFSLOG|rpc_wait seat={} port={} wait={}",
                seat_id(),
                port,
                wait_ticks
            );
        }
        if let Err(err) = send_all(&sock, req.as_bytes()) {
            reset_socket(port);
            drop(guard);
            return Err(err);
        }
        for payload in payloads {
            if let Err(err) = send_all(&sock, payload) {
                reset_socket(port);
                drop(guard);
                return Err(err);
            }
        }
        let mut resp_buf = [0u8; size_of::<DfsResp>()];
        if let Err(err) = recv_all(&sock, &mut resp_buf) {
            reset_socket(port);
            drop(guard);
            return Err(err);
        }
        let Some(resp) = DfsResp::read_from(&resp_buf) else {
            reset_socket(port);
            drop(guard);
            return Err(InvalidArgument);
        };
        if resp.magic != DFS_MAGIC {
            reset_socket(port);
            drop(guard);
            return Err(InvalidArgument);
        }
        let mut data = alloc::vec![0u8; resp.len as usize];
        let res = if data.is_empty() {
            Ok(())
        } else {
            recv_all(&sock, &mut data)
        };
        if let Err(err) = res {
            reset_socket(port);
            drop(guard);
            return Err(err);
        }
        let total_ticks = (*TICKS.lock()).saturating_sub(wait_start);
        if total_ticks >= 200 {
            println!(
                "DFSLOG|rpc_slow seat={} port={} op={} dt={}",
                seat_id(),
                port,
                req.op,
                total_ticks
            );
        }
        Ok((resp, data))
    }

    fn send_all(sock: &Arc<InetSocket>, buf: &[u8]) -> Result<()> {
        let mut off = 0usize;
        while off < buf.len() {
            let ptr = buf.as_ptr() as usize + off;
            let n = sock.write(VirtAddr::Kernel(ptr), buf.len() - off, false)?;
            if n == 0 {
                return Err(NotConnected);
            }
            off += n;
        }
        Ok(())
    }

    fn recv_all(sock: &Arc<InetSocket>, buf: &mut [u8]) -> Result<()> {
        let mut off = 0usize;
        while off < buf.len() {
            let ptr = buf.as_mut_ptr() as usize + off;
            let n = sock.read(VirtAddr::Kernel(ptr), buf.len() - off, false)?;
            if n == 0 {
                return Err(NotConnected);
            }
            off += n;
        }
        Ok(())
    }

    fn offset_addr(addr: VirtAddr, off: usize) -> VirtAddr {
        match addr {
            VirtAddr::User(a) => VirtAddr::User(a + off),
            VirtAddr::Kernel(a) => VirtAddr::Kernel(a + off),
            VirtAddr::Physical(a) => VirtAddr::Physical(a + off),
        }
    }

    pub fn is_remote_path(path: &Path) -> bool {
        let s = path.as_str();
        s == DFS_PREFIX || s.starts_with(DFS_PREFIX_DIR)
    }

    fn remote_path(path: &Path) -> Option<&str> {
        let s = path.as_str();
        if s == DFS_PREFIX {
            Some("/")
        } else if s.starts_with(DFS_PREFIX_DIR) {
            Some(&s[DFS_PREFIX.len()..])
        } else {
            None
        }
    }

    fn check_status(resp: &DfsResp) -> Result<()> {
        if resp.status < 0 {
            return Err(crate::error::Error::from_isize(resp.status as isize));
        }
        Ok(())
    }

    pub fn open(path: &Path, flags: usize) -> Result<u32> {
        let rpath = remote_path(path).ok_or(InvalidArgument)?;
        let req = DfsReq::new(DfsOp::Open, flags as u32, 0, rpath.len() as u32, 0);
        let (resp, _) = call(&req, &[rpath.as_bytes()])?;
        check_status(&resp)?;
        Ok(resp.handle)
    }

    pub fn read(handle: u32, dst: VirtAddr, n: usize) -> Result<usize> {
        let mut total = 0usize;
        while total < n {
            let chunk = min(DFS_MAX_CHUNK, n - total);
            let req = DfsReq::new(DfsOp::Read, 0, handle, chunk as u32, 0);
            let (resp, data) = call(&req, &[])?;
            check_status(&resp)?;
            let got = resp.status as usize;
            if got == 0 {
                break;
            }
            let copy_len = min(got, data.len());
            let addr = offset_addr(dst, total);
            either_copyout(addr, &data[..copy_len])?;
            total += copy_len;
            if got < chunk {
                break;
            }
        }
        Ok(total)
    }

    pub fn write(handle: u32, src: VirtAddr, n: usize) -> Result<usize> {
        let mut total = 0usize;
        while total < n {
            let chunk = min(DFS_MAX_CHUNK, n - total);
            let mut buf = alloc::vec![0u8; chunk];
            let addr = offset_addr(src, total);
            either_copyin(&mut buf[..], addr)?;
            let req = DfsReq::new(DfsOp::Write, 0, handle, chunk as u32, 0);
            let (resp, _) = call(&req, &[&buf])?;
            check_status(&resp)?;
            let wrote = resp.status as usize;
            total += wrote;
            if wrote < chunk {
                break;
            }
        }
        Ok(total)
    }

    pub fn close(handle: u32) -> Result<()> {
        let req = DfsReq::new(DfsOp::Close, 0, handle, 0, 0);
        let (resp, _) = call(&req, &[])?;
        check_status(&resp)?;
        Ok(())
    }

    pub fn fsync(handle: u32) -> Result<()> {
        let req = DfsReq::new(DfsOp::Fsync, 0, handle, 0, 0);
        let (resp, _) = call(&req, &[])?;
        check_status(&resp)?;
        Ok(())
    }

    pub fn stat(handle: u32, dst: VirtAddr) -> Result<()> {
        let req = DfsReq::new(DfsOp::Stat, 0, handle, 0, 0);
        let (resp, data) = call(&req, &[])?;
        check_status(&resp)?;
        if data.len() < size_of::<Stat>() {
            return Err(InvalidArgument);
        }
        let mut stat: Stat = Default::default();
        stat.as_bytes_mut()
            .copy_from_slice(&data[..size_of::<Stat>()]);
        either_copyout(dst, &stat)
    }

    pub fn mkdir(path: &Path) -> Result<()> {
        let rpath = remote_path(path).ok_or(InvalidArgument)?;
        let req = DfsReq::new(DfsOp::Mkdir, 0, 0, rpath.len() as u32, 0);
        let (resp, _) = call(&req, &[rpath.as_bytes()])?;
        check_status(&resp)?;
        Ok(())
    }

    pub fn unlink(path: &Path) -> Result<()> {
        let rpath = remote_path(path).ok_or(InvalidArgument)?;
        let req = DfsReq::new(DfsOp::Unlink, 0, 0, rpath.len() as u32, 0);
        let (resp, _) = call(&req, &[rpath.as_bytes()])?;
        check_status(&resp)?;
        Ok(())
    }

    pub fn link(old: &Path, new: &Path) -> Result<()> {
        let oldp = remote_path(old).ok_or(InvalidArgument)?;
        let newp = remote_path(new).ok_or(InvalidArgument)?;
        let req = DfsReq::new(DfsOp::Link, 0, 0, oldp.len() as u32, newp.len() as u32);
        let (resp, _) = call(&req, &[oldp.as_bytes(), newp.as_bytes()])?;
        check_status(&resp)?;
        Ok(())
    }

    pub fn symlink(target: &str, linkpath: &Path) -> Result<()> {
        let linkp = remote_path(linkpath).ok_or(InvalidArgument)?;
        let tgt = if target.starts_with(DFS_PREFIX_DIR) {
            &target[DFS_PREFIX.len()..]
        } else if target == DFS_PREFIX {
            "/"
        } else {
            target
        };
        let req = DfsReq::new(DfsOp::Symlink, 0, 0, tgt.len() as u32, linkp.len() as u32);
        let (resp, _) = call(&req, &[tgt.as_bytes(), linkp.as_bytes()])?;
        check_status(&resp)?;
        Ok(())
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub use client::*;
