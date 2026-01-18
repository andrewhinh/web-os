pub const AF_UNIX: usize = 1;
pub const AF_INET: usize = 2;
pub const SOCK_STREAM: usize = 1;
pub const SOCK_DGRAM: usize = 2;

#[cfg(all(target_os = "none", feature = "kernel"))]
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::{Arc, Weak},
};
#[cfg(all(target_os = "none", feature = "kernel"))]
use core::net::{Ipv4Addr, SocketAddrV4};

#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::error::{Error::*, Result};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::mpmc::{Receiver, SyncSender, sync_channel};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::net::{self, TcpListener, TcpSocket, UdpSocket};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::proc::{either_copyin, either_copyout};
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::spinlock::Mutex;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::sync::LazyLock;
#[cfg(all(target_os = "none", feature = "kernel"))]
use crate::vm::VirtAddr;

#[cfg(all(target_os = "none", feature = "kernel"))]
const STREAM_BUF: isize = 512;
#[cfg(all(target_os = "none", feature = "kernel"))]
const BACKLOG_MAX: usize = 16;

#[cfg(all(target_os = "none", feature = "kernel"))]
static UNIX_REGISTRY: LazyLock<Mutex<BTreeMap<String, Weak<UnixSocket>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new(), "unixsock"));

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub struct UnixStream {
    rx: Receiver<u8>,
    tx: SyncSender<u8>,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl UnixStream {
    fn pair() -> (Self, Self) {
        let (tx_a, rx_a) = sync_channel::<u8>(STREAM_BUF, "unixsock");
        let (tx_b, rx_b) = sync_channel::<u8>(STREAM_BUF, "unixsock");
        let a = Self { rx: rx_a, tx: tx_b };
        let b = Self { rx: rx_b, tx: tx_a };
        (a, b)
    }

    fn read(&self, mut dst: VirtAddr, n: usize) -> Result<usize> {
        if n == 0 {
            return Ok(0);
        }
        let mut i = 0;
        match self.rx.recv() {
            Ok(ch) => {
                either_copyout(dst, &ch)?;
                dst += 1;
                i += 1;
            }
            Err(_) => return Ok(0),
        }
        while i < n {
            match self.rx.try_recv() {
                Ok(ch) => {
                    either_copyout(dst, &ch)?;
                    dst += 1;
                    i += 1;
                }
                Err(WouldBlock) => break,
                Err(_) => break,
            }
        }
        Ok(i)
    }

    fn read_nonblock(&self, mut dst: VirtAddr, n: usize) -> Result<usize> {
        let mut i = 0;
        while i < n {
            match self.rx.try_recv() {
                Ok(ch) => {
                    either_copyout(dst, &ch)?;
                    dst += 1;
                    i += 1;
                }
                Err(WouldBlock) => {
                    return if i == 0 { Err(WouldBlock) } else { Ok(i) };
                }
                Err(_) => break,
            }
        }
        Ok(i)
    }

    fn write(&self, mut src: VirtAddr, n: usize) -> Result<usize> {
        let mut i = 0;
        while i < n {
            let mut ch: u8 = 0;
            either_copyin(&mut ch, src)?;
            let Ok(()) = self.tx.send(ch) else {
                break;
            };
            src += 1;
            i += 1;
        }
        Ok(i)
    }

    fn write_nonblock(&self, mut src: VirtAddr, n: usize) -> Result<usize> {
        let mut i = 0;
        while i < n {
            let mut ch: u8 = 0;
            either_copyin(&mut ch, src)?;
            match self.tx.try_send(ch) {
                Ok(()) => {
                    src += 1;
                    i += 1;
                }
                Err(WouldBlock) => {
                    return if i == 0 { Err(WouldBlock) } else { Ok(i) };
                }
                Err(_) => break,
            }
        }
        Ok(i)
    }

    fn poll_readable(&self) -> bool {
        self.rx.has_data() || self.rx.is_closed()
    }

    fn poll_writable(&self) -> Result<bool> {
        if self.tx.is_closed() {
            return Ok(false);
        }
        self.tx.can_send()
    }

    fn poll_read_hup(&self) -> bool {
        self.rx.is_closed()
    }

    fn poll_write_hup(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub struct UnixListener {
    rx: Receiver<UnixStream>,
    tx: SyncSender<UnixStream>,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl UnixListener {
    fn new(backlog: usize) -> Self {
        let cap = backlog.clamp(1, BACKLOG_MAX) as isize;
        let (tx, rx) = sync_channel::<UnixStream>(cap, "unixlisten");
        Self { rx, tx }
    }

    fn enqueue(&self, stream: UnixStream, nonblock: bool) -> Result<()> {
        if nonblock {
            self.tx.try_send(stream)
        } else {
            self.tx.send(stream)
        }
    }

    fn accept(&self, nonblock: bool) -> Result<UnixStream> {
        if nonblock {
            self.rx.try_recv()
        } else {
            self.rx.recv()
        }
    }

    fn poll_readable(&self) -> bool {
        self.rx.has_data()
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug, Clone)]
enum SocketState {
    Unbound,
    Listening(Arc<UnixListener>),
    Connected(Arc<UnixStream>),
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
struct UnixSocketInner {
    name: Option<String>,
    state: SocketState,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub struct UnixSocket {
    inner: Mutex<UnixSocketInner>,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl UnixSocket {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(
                UnixSocketInner {
                    name: None,
                    state: SocketState::Unbound,
                },
                "unixsock",
            ),
        })
    }

    pub fn from_stream(stream: UnixStream) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(
                UnixSocketInner {
                    name: None,
                    state: SocketState::Connected(Arc::new(stream)),
                },
                "unixsock",
            ),
        })
    }

    pub fn bind(self: &Arc<Self>, path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(InvalidArgument);
        }
        let mut inner = self.inner.lock();
        if inner.name.is_some() {
            return Err(InvalidArgument);
        }
        if !matches!(inner.state, SocketState::Unbound) {
            return Err(InvalidArgument);
        }
        let mut reg = UNIX_REGISTRY.lock();
        if let Some(existing) = reg.get(path).and_then(|weak| weak.upgrade()) {
            let _ = existing;
            return Err(AlreadyExists);
        }
        reg.insert(path.to_string(), Arc::downgrade(self));
        inner.name = Some(path.to_string());
        Ok(())
    }

    pub fn listen(self: &Arc<Self>, backlog: usize) -> Result<()> {
        let mut inner = self.inner.lock();
        if inner.name.is_none() {
            return Err(InvalidArgument);
        }
        if !matches!(inner.state, SocketState::Unbound) {
            return Err(InvalidArgument);
        }
        if backlog == 0 {
            return Err(InvalidArgument);
        }
        let listener = Arc::new(UnixListener::new(backlog));
        inner.state = SocketState::Listening(listener);
        Ok(())
    }

    pub fn accept(&self, nonblock: bool) -> Result<UnixStream> {
        let listener = {
            let inner = self.inner.lock();
            match &inner.state {
                SocketState::Listening(listener) => Arc::clone(listener),
                _ => return Err(InvalidArgument),
            }
        };
        listener.accept(nonblock)
    }

    pub fn connect(&self, path: &str, nonblock: bool) -> Result<()> {
        if path.is_empty() {
            return Err(InvalidArgument);
        }
        {
            let inner = self.inner.lock();
            if !matches!(inner.state, SocketState::Unbound) {
                return Err(InvalidArgument);
            }
        }

        let listener = lookup_listener(path)?;
        let (client, server) = UnixStream::pair();
        listener.enqueue(server, nonblock)?;

        let mut inner = self.inner.lock();
        inner.state = SocketState::Connected(Arc::new(client));
        Ok(())
    }

    pub fn read(&self, dst: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        let stream = {
            let inner = self.inner.lock();
            match &inner.state {
                SocketState::Connected(stream) => Arc::clone(stream),
                SocketState::Listening(_) => return Err(InvalidArgument),
                SocketState::Unbound => return Err(NotConnected),
            }
        };
        if nonblock {
            stream.read_nonblock(dst, n)
        } else {
            stream.read(dst, n)
        }
    }

    pub fn write(&self, src: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        let stream = {
            let inner = self.inner.lock();
            match &inner.state {
                SocketState::Connected(stream) => Arc::clone(stream),
                SocketState::Listening(_) => return Err(InvalidArgument),
                SocketState::Unbound => return Err(NotConnected),
            }
        };
        if nonblock {
            stream.write_nonblock(src, n)
        } else {
            stream.write(src, n)
        }
    }

    pub fn poll(&self, events: usize, readable: bool, writable: bool) -> usize {
        let mut revents = 0;
        let state = {
            let inner = self.inner.lock();
            inner.state.clone()
        };
        match state {
            SocketState::Connected(stream) => {
                if readable && events & crate::poll::IN != 0 && stream.poll_readable() {
                    revents |= crate::poll::IN;
                }
                if writable
                    && events & crate::poll::OUT != 0
                    && stream.poll_writable().unwrap_or(false)
                {
                    revents |= crate::poll::OUT;
                }
                if readable && stream.poll_read_hup() {
                    revents |= crate::poll::HUP;
                }
                if writable && stream.poll_write_hup() {
                    revents |= crate::poll::HUP;
                }
            }
            SocketState::Listening(listener) => {
                if readable && events & crate::poll::IN != 0 && listener.poll_readable() {
                    revents |= crate::poll::IN;
                }
            }
            SocketState::Unbound => {}
        }
        revents
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl Drop for UnixSocket {
    fn drop(&mut self) {
        let name = {
            let mut inner = self.inner.lock();
            inner.name.take()
        };
        if let Some(name) = name {
            UNIX_REGISTRY.lock().remove(&name);
        }
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
pub struct InetSocket {
    inner: Mutex<InetSocketInner>,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
struct InetSocketInner {
    stype: usize,
    state: InetState,
}

#[cfg(all(target_os = "none", feature = "kernel"))]
#[derive(Debug)]
enum InetState {
    Unbound,
    Datagram(Arc<UdpSocket>),
    Stream(Arc<TcpSocket>),
    Listening(Arc<TcpListener>),
}

#[cfg(all(target_os = "none", feature = "kernel"))]
impl InetSocket {
    pub fn new(stype: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(
                InetSocketInner {
                    stype,
                    state: InetState::Unbound,
                },
                "inetsock",
            ),
        })
    }

    pub fn bind(self: &Arc<Self>, path: &str) -> Result<()> {
        let addr = parse_socket_addr(path)?;
        let ip = *addr.ip();
        if ip != Ipv4Addr::new(0, 0, 0, 0) && ip != net::local_ip() {
            return Err(InvalidArgument);
        }
        let mut inner = self.inner.lock();
        match inner.stype {
            SOCK_DGRAM => {
                let sock = match &inner.state {
                    InetState::Datagram(s) => Arc::clone(s),
                    InetState::Unbound => {
                        let s = UdpSocket::new();
                        inner.state = InetState::Datagram(Arc::clone(&s));
                        s
                    }
                    _ => return Err(InvalidArgument),
                };
                drop(inner);
                sock.bind(addr.port())
            }
            SOCK_STREAM => {
                let sock = match &inner.state {
                    InetState::Stream(s) => Arc::clone(s),
                    InetState::Unbound => {
                        let s = TcpSocket::new();
                        inner.state = InetState::Stream(Arc::clone(&s));
                        s
                    }
                    _ => return Err(InvalidArgument),
                };
                drop(inner);
                sock.bind(addr.port())
            }
            _ => Err(InvalidArgument),
        }
    }

    pub fn listen(self: &Arc<Self>, backlog: usize) -> Result<()> {
        let mut inner = self.inner.lock();
        if inner.stype != SOCK_STREAM {
            return Err(InvalidArgument);
        }
        let stream = match &inner.state {
            InetState::Stream(s) => Arc::clone(s),
            _ => return Err(InvalidArgument),
        };
        let port = stream.local_addr().ok_or(InvalidArgument)?.port();
        let listener = TcpListener::new(backlog, port);
        listener.register()?;
        inner.state = InetState::Listening(listener);
        Ok(())
    }

    pub fn accept(self: &Arc<Self>, nonblock: bool) -> Result<Arc<InetSocket>> {
        let inner = self.inner.lock();
        let listener = match &inner.state {
            InetState::Listening(l) => Arc::clone(l),
            _ => return Err(InvalidArgument),
        };
        drop(inner);
        let conn = listener.accept(nonblock)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(
                InetSocketInner {
                    stype: SOCK_STREAM,
                    state: InetState::Stream(conn),
                },
                "inetsock",
            ),
        }))
    }

    pub fn connect(self: &Arc<Self>, path: &str, nonblock: bool) -> Result<()> {
        let addr = parse_socket_addr(path)?;
        if *addr.ip() == Ipv4Addr::new(0, 0, 0, 0) {
            return Err(InvalidArgument);
        }
        let mut inner = self.inner.lock();
        match inner.stype {
            SOCK_DGRAM => {
                let sock = match &inner.state {
                    InetState::Datagram(s) => Arc::clone(s),
                    InetState::Unbound => {
                        let s = UdpSocket::new();
                        inner.state = InetState::Datagram(Arc::clone(&s));
                        s
                    }
                    _ => return Err(InvalidArgument),
                };
                drop(inner);
                sock.connect(addr)
            }
            SOCK_STREAM => {
                let sock = match &inner.state {
                    InetState::Stream(s) => Arc::clone(s),
                    InetState::Unbound => {
                        let s = TcpSocket::new();
                        inner.state = InetState::Stream(Arc::clone(&s));
                        s
                    }
                    _ => return Err(InvalidArgument),
                };
                drop(inner);
                sock.connect(addr, nonblock)
            }
            _ => Err(InvalidArgument),
        }
    }

    pub fn read(&self, dst: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        let inner = self.inner.lock();
        let sock = match &inner.state {
            InetState::Datagram(s) => InetRead::Datagram(Arc::clone(s)),
            InetState::Stream(s) => InetRead::Stream(Arc::clone(s)),
            _ => return Err(InvalidArgument),
        };
        drop(inner);
        match sock {
            InetRead::Datagram(s) => s.read(dst, n, nonblock),
            InetRead::Stream(s) => s.read(dst, n, nonblock),
        }
    }

    pub fn write(&self, src: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        let inner = self.inner.lock();
        let sock = match &inner.state {
            InetState::Datagram(s) => InetWrite::Datagram(Arc::clone(s)),
            InetState::Stream(s) => InetWrite::Stream(Arc::clone(s)),
            _ => return Err(InvalidArgument),
        };
        drop(inner);
        match sock {
            InetWrite::Datagram(s) => s.write(src, n, nonblock),
            InetWrite::Stream(s) => s.write(src, n, nonblock),
        }
    }

    pub fn poll(&self, events: usize, readable: bool, writable: bool) -> usize {
        let mut revents = 0;
        let inner = self.inner.lock();
        match &inner.state {
            InetState::Datagram(s) => {
                if readable && events & crate::poll::IN != 0 && s.poll_readable() {
                    revents |= crate::poll::IN;
                }
                if writable && events & crate::poll::OUT != 0 && s.poll_writable() {
                    revents |= crate::poll::OUT;
                }
            }
            InetState::Stream(s) => {
                if readable && events & crate::poll::IN != 0 && s.poll_readable() {
                    revents |= crate::poll::IN;
                }
                if writable && events & crate::poll::OUT != 0 && s.poll_writable() {
                    revents |= crate::poll::OUT;
                }
                if readable && s.is_closed() {
                    revents |= crate::poll::HUP;
                }
            }
            InetState::Listening(_) | InetState::Unbound => {}
        }
        revents
    }
}

#[cfg(all(target_os = "none", feature = "kernel"))]
enum InetRead {
    Datagram(Arc<UdpSocket>),
    Stream(Arc<TcpSocket>),
}

#[cfg(all(target_os = "none", feature = "kernel"))]
enum InetWrite {
    Datagram(Arc<UdpSocket>),
    Stream(Arc<TcpSocket>),
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn parse_socket_addr(path: &str) -> Result<SocketAddrV4> {
    let mut parts = path.split(':');
    let ip_str = parts.next().ok_or(InvalidArgument)?;
    let port_str = parts.next().ok_or(InvalidArgument)?;
    if parts.next().is_some() {
        return Err(InvalidArgument);
    }
    let port: u16 = port_str.parse().map_err(|_| InvalidArgument)?;
    let ip = if ip_str.is_empty() {
        Ipv4Addr::new(0, 0, 0, 0)
    } else {
        parse_ipv4(ip_str)?
    };
    Ok(SocketAddrV4::new(ip, port))
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn parse_ipv4(s: &str) -> Result<Ipv4Addr> {
    let mut octets = [0u8; 4];
    let mut idx = 0;
    for part in s.split('.') {
        if idx >= 4 {
            return Err(InvalidArgument);
        }
        let val: u8 = part.parse().map_err(|_| InvalidArgument)?;
        octets[idx] = val;
        idx += 1;
    }
    if idx != 4 {
        return Err(InvalidArgument);
    }
    Ok(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]))
}

#[cfg(all(target_os = "none", feature = "kernel"))]
pub fn validate(domain: usize, stype: usize, _protocol: usize) -> Result<()> {
    match domain {
        AF_UNIX => {
            if stype != SOCK_STREAM {
                return Err(InvalidArgument);
            }
        }
        AF_INET => {
            if stype != SOCK_STREAM && stype != SOCK_DGRAM {
                return Err(InvalidArgument);
            }
        }
        _ => return Err(InvalidArgument),
    }
    Ok(())
}

#[cfg(all(target_os = "none", feature = "kernel"))]
fn lookup_listener(path: &str) -> Result<Arc<UnixListener>> {
    let mut reg = UNIX_REGISTRY.lock();
    let Some(entry) = reg.get(path).cloned() else {
        return Err(NotFound);
    };
    let Some(sock) = entry.upgrade() else {
        reg.remove(path);
        return Err(NotFound);
    };
    drop(reg);
    let inner = sock.inner.lock();
    match &inner.state {
        SocketState::Listening(listener) => Ok(Arc::clone(listener)),
        _ => Err(NotConnected),
    }
}
