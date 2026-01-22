use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::cmp::min;
use core::net::{Ipv4Addr, SocketAddrV4};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    array,
    error::{Error::*, Result},
    mpmc::{Receiver, SyncSender, sync_channel},
    proc::{self, either_copyin, either_copyout},
    spinlock::Mutex,
    sync::LazyLock,
    virtio_net,
    vm::VirtAddr,
};

const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_IPV4: u16 = 0x0800;
const IP_PROTO_UDP: u8 = 17;
const IP_PROTO_TCP: u8 = 6;
const UDP_QUEUE: isize = 32;
const TCP_QUEUE: isize = 1024;
const TCP_BACKLOG_MAX: usize = 16;
const DEFAULT_TTL: u8 = 64;
const ARP_TABLE_SIZE: usize = 16;
const MSS: usize = 512;

static NET: LazyLock<Mutex<NetStack>> = LazyLock::new(|| Mutex::new(NetStack::default(), "net"));
static UDP_PORTS: LazyLock<Mutex<BTreeMap<u16, Vec<Weak<UdpSocket>>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new(), "udp_ports"));
static TCP_LISTENERS: LazyLock<Mutex<BTreeMap<u16, Weak<TcpListener>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new(), "tcp_listen"));
static TCP_CONNS: LazyLock<Mutex<BTreeMap<ConnKey, Weak<TcpSocket>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new(), "tcp_conns"));

static ARP_SEQ: AtomicUsize = AtomicUsize::new(0);
static TCP_SEQ: AtomicUsize = AtomicUsize::new(0);
static ARP_LOCK: Mutex<()> = Mutex::new((), "arp");
static TCP_LOCK: Mutex<()> = Mutex::new((), "tcp");

#[derive(Clone, Copy, Debug)]
struct ArpEntry {
    ip: Ipv4Addr,
    mac: [u8; 6],
    valid: bool,
}

impl ArpEntry {
    const fn empty() -> Self {
        Self {
            ip: Ipv4Addr::new(0, 0, 0, 0),
            mac: [0; 6],
            valid: false,
        }
    }
}

#[derive(Debug)]
struct NetStack {
    mac: [u8; 6],
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    gateway: Ipv4Addr,
    arp: [ArpEntry; ARP_TABLE_SIZE],
}

impl Default for NetStack {
    fn default() -> Self {
        Self {
            mac: [0; 6],
            ip: Ipv4Addr::new(0, 0, 0, 0),
            netmask: Ipv4Addr::new(0, 0, 0, 0),
            gateway: Ipv4Addr::new(0, 0, 0, 0),
            arp: array![ArpEntry::empty(); ARP_TABLE_SIZE],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ConnKey {
    local_port: u16,
    remote_port: u16,
    remote_ip: [u8; 4],
}

impl ConnKey {
    fn new(local_port: u16, remote: SocketAddrV4) -> Self {
        Self {
            local_port,
            remote_port: remote.port(),
            remote_ip: remote.ip().octets(),
        }
    }
}

#[derive(Debug)]
pub struct UdpSocket {
    inner: Mutex<UdpInner>,
    rx: Receiver<UdpDatagram>,
    tx: SyncSender<UdpDatagram>,
}

#[derive(Debug)]
struct UdpInner {
    local_port: Option<u16>,
    peer: Option<SocketAddrV4>,
    last_peer: Option<SocketAddrV4>,
}

#[derive(Debug)]
struct UdpDatagram {
    src: SocketAddrV4,
    data: Vec<u8>,
}

#[derive(Debug)]
pub struct TcpSocket {
    inner: Mutex<TcpInner>,
    rx: Receiver<u8>,
    tx: SyncSender<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TcpState {
    Closed,
    SynSent,
    SynReceived,
    Established,
}

#[derive(Debug)]
struct TcpInner {
    local: Option<SocketAddrV4>,
    peer: Option<SocketAddrV4>,
    state: TcpState,
    snd_nxt: u32,
    rcv_nxt: u32,
    listener: Option<Weak<TcpListener>>,
}

#[derive(Debug)]
pub struct TcpListener {
    inner: Mutex<ListenerInner>,
    rx: Receiver<Arc<TcpSocket>>,
    tx: SyncSender<Arc<TcpSocket>>,
}

#[derive(Debug)]
struct ListenerInner {
    port: u16,
}

pub fn init() {
    let mac = virtio_net::NET.mac_addr();
    let mut net = NET.lock();
    net.mac = mac;
    net.ip = Ipv4Addr::new(10, 0, 2, 15);
    net.netmask = Ipv4Addr::new(255, 255, 255, 0);
    net.gateway = Ipv4Addr::new(10, 0, 2, 2);
}

pub fn local_ip() -> Ipv4Addr {
    NET.lock().ip
}

pub fn handle_frame(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let payload = &frame[14..];
    match ethertype {
        ETHERTYPE_ARP => handle_arp(payload),
        ETHERTYPE_IPV4 => handle_ipv4(payload),
        _ => {}
    }
}

fn handle_arp(payload: &[u8]) {
    if payload.len() < 28 {
        return;
    }
    let opcode = u16::from_be_bytes([payload[6], payload[7]]);
    let sender_mac: [u8; 6] = payload[8..14].try_into().unwrap_or([0; 6]);
    let sender_ip = Ipv4Addr::new(payload[14], payload[15], payload[16], payload[17]);
    let target_ip = Ipv4Addr::new(payload[24], payload[25], payload[26], payload[27]);
    insert_arp(sender_ip, sender_mac);

    let local_ip = NET.lock().ip;
    if opcode == 1 && target_ip == local_ip {
        let _ = send_arp_reply(sender_mac, sender_ip);
    }
}

fn handle_ipv4(payload: &[u8]) {
    if payload.len() < 20 {
        return;
    }
    let ver_ihl = payload[0];
    if ver_ihl >> 4 != 4 {
        return;
    }
    let ihl = ((ver_ihl & 0x0f) as usize) * 4;
    if payload.len() < ihl {
        return;
    }
    let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
    if total_len < ihl || payload.len() < total_len {
        return;
    }
    let proto = payload[9];
    let src = Ipv4Addr::new(payload[12], payload[13], payload[14], payload[15]);
    let dst = Ipv4Addr::new(payload[16], payload[17], payload[18], payload[19]);
    if dst != NET.lock().ip {
        return;
    }
    let data = &payload[ihl..total_len];
    match proto {
        IP_PROTO_UDP => udp_input(src, data),
        IP_PROTO_TCP => tcp_input(src, dst, data),
        _ => {}
    }
}

fn udp_input(src_ip: Ipv4Addr, data: &[u8]) {
    if data.len() < 8 {
        return;
    }
    let src_port = u16::from_be_bytes([data[0], data[1]]);
    let dst_port = u16::from_be_bytes([data[2], data[3]]);
    let len = u16::from_be_bytes([data[4], data[5]]) as usize;
    if len < 8 || data.len() < len {
        return;
    }
    let payload = &data[8..len];
    let src = SocketAddrV4::new(src_ip, src_port);
    let mut guard = UDP_PORTS.lock();
    let Some(list) = guard.get_mut(&dst_port) else {
        return;
    };
    list.retain(|w| w.upgrade().is_some());
    for weak in list.iter() {
        if let Some(sock) = weak.upgrade() {
            sock.enqueue(src, payload);
        }
    }
}

fn tcp_input(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, data: &[u8]) {
    if data.len() < 20 {
        return;
    }
    let src_port = u16::from_be_bytes([data[0], data[1]]);
    let dst_port = u16::from_be_bytes([data[2], data[3]]);
    let seq = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let ack = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let data_offset = (data[12] >> 4) as usize;
    let hdr_len = data_offset * 4;
    if hdr_len < 20 || data.len() < hdr_len {
        return;
    }
    let flags = data[13] as u16;
    let payload = &data[hdr_len..];
    let remote = SocketAddrV4::new(src_ip, src_port);
    let key = ConnKey::new(dst_port, remote);
    let conn = {
        let guard = TCP_CONNS.lock();
        guard.get(&key).and_then(|w| w.upgrade())
    };
    if let Some(sock) = conn {
        sock.on_segment(remote, seq, ack, flags, payload);
        return;
    }
    if flags & 0x02 == 0 {
        return;
    }
    let listener = TCP_LISTENERS
        .lock()
        .get(&dst_port)
        .and_then(|w| w.upgrade());
    let Some(listener) = listener else { return };
    let conn = TcpSocket::new();
    conn.init_passive(dst_ip, dst_port, remote, Arc::downgrade(&listener), seq);
    TCP_CONNS.lock().insert(key, Arc::downgrade(&conn));
    let syn_seq = conn.snd_nxt();
    conn.bump_snd(1);
    let _ = send_tcp_segment_nonblock(
        SocketAddrV4::new(dst_ip, dst_port),
        remote,
        syn_seq,
        conn.rcv_nxt(),
        0x12,
        &[],
    );
}

fn insert_arp(ip: Ipv4Addr, mac: [u8; 6]) {
    let mut net = NET.lock();
    if let Some(ent) = net.arp.iter_mut().find(|e| e.valid && e.ip == ip) {
        ent.mac = mac;
        notify_arp();
        return;
    }
    if let Some(ent) = net.arp.iter_mut().find(|e| !e.valid) {
        *ent = ArpEntry {
            ip,
            mac,
            valid: true,
        };
        notify_arp();
        return;
    }
    net.arp[0] = ArpEntry {
        ip,
        mac,
        valid: true,
    };
    notify_arp();
}

fn lookup_arp(ip: Ipv4Addr) -> Option<[u8; 6]> {
    let net = NET.lock();
    net.arp
        .iter()
        .find(|e| e.valid && e.ip == ip)
        .map(|e| e.mac)
}

fn notify_arp() {
    ARP_SEQ.fetch_add(1, Ordering::Release);
    proc::wakeup(&ARP_SEQ as *const _ as usize);
}

fn wait_arp() {
    let guard = ARP_LOCK.lock();
    let _ = proc::sleep(&ARP_SEQ as *const _ as usize, guard);
}

fn notify_tcp() {
    TCP_SEQ.fetch_add(1, Ordering::Release);
    proc::wakeup(&TCP_SEQ as *const _ as usize);
}

fn wait_tcp(seq: &mut usize) {
    let guard = TCP_LOCK.lock();
    let cur = TCP_SEQ.load(Ordering::Acquire);
    if cur != *seq {
        *seq = cur;
        return;
    }
    let _ = proc::sleep(&TCP_SEQ as *const _ as usize, guard);
    *seq = TCP_SEQ.load(Ordering::Acquire);
}

fn ipv4_u32(ip: Ipv4Addr) -> u32 {
    u32::from_be_bytes(ip.octets())
}

fn route_ip(dst: Ipv4Addr) -> Ipv4Addr {
    let net = NET.lock();
    let mask = ipv4_u32(net.netmask);
    if (ipv4_u32(dst) & mask) == (ipv4_u32(net.ip) & mask) {
        dst
    } else {
        net.gateway
    }
}

fn build_eth_frame(dst_mac: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let src_mac = NET.lock().mac;
    let mut buf = Vec::with_capacity(14 + payload.len());
    buf.extend_from_slice(&dst_mac);
    buf.extend_from_slice(&src_mac);
    buf.extend_from_slice(&ethertype.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn checksum_parts(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    for part in parts {
        sum = checksum_add(sum, part);
    }
    finalize_checksum(sum)
}

fn checksum_add(mut sum: u32, data: &[u8]) -> u32 {
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    sum
}

fn finalize_checksum(mut sum: u32) -> u16 {
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn build_ipv4_packet(proto: u8, src: Ipv4Addr, dst: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
    let total_len = 20 + payload.len();
    let mut buf = Vec::with_capacity(total_len);
    buf.push(0x45);
    buf.push(0);
    buf.extend_from_slice(&(total_len as u16).to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.push(DEFAULT_TTL);
    buf.push(proto);
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&src.octets());
    buf.extend_from_slice(&dst.octets());
    let csum = checksum_parts(&[&buf]);
    buf[10..12].copy_from_slice(&csum.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn send_arp_request(target_ip: Ipv4Addr) -> Result<()> {
    let src_mac = NET.lock().mac;
    let src_ip = NET.lock().ip;
    let mut buf = Vec::with_capacity(28);
    buf.extend_from_slice(&1u16.to_be_bytes());
    buf.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    buf.push(6);
    buf.push(4);
    buf.extend_from_slice(&1u16.to_be_bytes());
    buf.extend_from_slice(&src_mac);
    buf.extend_from_slice(&src_ip.octets());
    buf.extend_from_slice(&[0; 6]);
    buf.extend_from_slice(&target_ip.octets());
    let frame = build_eth_frame([0xff; 6], ETHERTYPE_ARP, &buf);
    virtio_net::NET.try_send_frame(&frame)
}

fn send_arp_reply(target_mac: [u8; 6], target_ip: Ipv4Addr) -> Result<()> {
    let src_mac = NET.lock().mac;
    let src_ip = NET.lock().ip;
    let mut buf = Vec::with_capacity(28);
    buf.extend_from_slice(&1u16.to_be_bytes());
    buf.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    buf.push(6);
    buf.push(4);
    buf.extend_from_slice(&2u16.to_be_bytes());
    buf.extend_from_slice(&src_mac);
    buf.extend_from_slice(&src_ip.octets());
    buf.extend_from_slice(&target_mac);
    buf.extend_from_slice(&target_ip.octets());
    let frame = build_eth_frame(target_mac, ETHERTYPE_ARP, &buf);
    virtio_net::NET.try_send_frame(&frame)
}

fn send_ipv4_packet(
    proto: u8,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    payload: &[u8],
    nonblock: bool,
) -> Result<()> {
    if dst == NET.lock().ip {
        match proto {
            IP_PROTO_UDP => udp_input(src, payload),
            IP_PROTO_TCP => tcp_input(src, dst, payload),
            _ => {}
        }
        return Ok(());
    }
    let next_hop = route_ip(dst);
    let Some(mac) = lookup_arp(next_hop) else {
        let _ = send_arp_request(next_hop);
        return Err(WouldBlock);
    };
    let pkt = build_ipv4_packet(proto, src, dst, payload);
    let frame = build_eth_frame(mac, ETHERTYPE_IPV4, &pkt);
    if nonblock {
        virtio_net::NET.try_send_frame(&frame)
    } else {
        virtio_net::NET.send_frame(&frame)
    }
}

fn send_ipv4_blocking(
    proto: u8,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    payload: &[u8],
    nonblock: bool,
) -> Result<()> {
    loop {
        match send_ipv4_packet(proto, src, dst, payload, nonblock) {
            Ok(()) => return Ok(()),
            Err(WouldBlock) if nonblock => return Err(WouldBlock),
            Err(WouldBlock) => wait_arp(),
            Err(e) => return Err(e),
        }
    }
}

fn send_udp_segment(
    src: SocketAddrV4,
    dst: SocketAddrV4,
    payload: &[u8],
    nonblock: bool,
) -> Result<()> {
    let mut udp = Vec::with_capacity(8 + payload.len());
    udp.extend_from_slice(&src.port().to_be_bytes());
    udp.extend_from_slice(&dst.port().to_be_bytes());
    udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&0u16.to_be_bytes());
    udp.extend_from_slice(payload);
    send_ipv4_blocking(IP_PROTO_UDP, *src.ip(), *dst.ip(), &udp, nonblock)
}

fn send_tcp_segment(
    src: SocketAddrV4,
    dst: SocketAddrV4,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
    nonblock: bool,
) -> Result<()> {
    let mut tcp = Vec::with_capacity(20 + payload.len());
    tcp.extend_from_slice(&src.port().to_be_bytes());
    tcp.extend_from_slice(&dst.port().to_be_bytes());
    tcp.extend_from_slice(&seq.to_be_bytes());
    tcp.extend_from_slice(&ack.to_be_bytes());
    let data_off_flags = ((5u16) << 12) | (flags & 0x01ff);
    tcp.extend_from_slice(&data_off_flags.to_be_bytes());
    tcp.extend_from_slice(&1024u16.to_be_bytes());
    tcp.extend_from_slice(&0u16.to_be_bytes());
    tcp.extend_from_slice(&0u16.to_be_bytes());
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&src.ip().octets());
    pseudo[4..8].copy_from_slice(&dst.ip().octets());
    pseudo[8] = 0;
    pseudo[9] = IP_PROTO_TCP;
    let tcp_len = (tcp.len() + payload.len()) as u16;
    pseudo[10..12].copy_from_slice(&tcp_len.to_be_bytes());
    let csum = checksum_parts(&[&pseudo, &tcp, payload]);
    tcp[16..18].copy_from_slice(&csum.to_be_bytes());
    tcp.extend_from_slice(payload);
    send_ipv4_blocking(IP_PROTO_TCP, *src.ip(), *dst.ip(), &tcp, nonblock)
}

fn send_tcp_segment_nonblock(
    src: SocketAddrV4,
    dst: SocketAddrV4,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
) -> Result<()> {
    send_tcp_segment(src, dst, seq, ack, flags, payload, true)
}

fn alloc_ephemeral_port() -> u16 {
    static NEXT_PORT: AtomicUsize = AtomicUsize::new(49152);
    let mut port = NEXT_PORT.fetch_add(1, Ordering::Relaxed) as u16;
    if port < 49152 {
        port = 49152;
    }
    port
}

impl UdpSocket {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = sync_channel::<UdpDatagram>(UDP_QUEUE, "udp");
        Arc::new(Self {
            inner: Mutex::new(
                UdpInner {
                    local_port: None,
                    peer: None,
                    last_peer: None,
                },
                "udp",
            ),
            rx,
            tx,
        })
    }

    pub fn bind(self: &Arc<Self>, port: u16) -> Result<()> {
        if port == 0 {
            return Err(InvalidArgument);
        }
        let mut guard = UDP_PORTS.lock();
        let list = guard.entry(port).or_default();
        list.retain(|w| w.upgrade().is_some());
        if !list.is_empty() {
            return Err(ResourceBusy);
        }
        list.push(Arc::downgrade(self));
        self.inner.lock().local_port = Some(port);
        Ok(())
    }

    pub fn connect(&self, peer: SocketAddrV4) -> Result<()> {
        self.inner.lock().peer = Some(peer);
        Ok(())
    }

    fn enqueue(&self, src: SocketAddrV4, data: &[u8]) {
        let _ = self.tx.try_send(UdpDatagram {
            src,
            data: data.to_vec(),
        });
    }

    pub fn read(self: &Arc<Self>, dst: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        let datagram = if nonblock {
            self.rx.try_recv()?
        } else {
            self.rx.recv()?
        };
        self.inner.lock().last_peer = Some(datagram.src);
        let copy_len = min(n, datagram.data.len());
        either_copyout(dst, &datagram.data[..copy_len])?;
        Ok(copy_len)
    }

    pub fn write(self: &Arc<Self>, src: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        if n == 0 {
            return Ok(0);
        }
        let mut buf = vec![0u8; n];
        either_copyin(&mut buf[..], src)?;
        let (port, peer) = {
            let mut inner = self.inner.lock();
            let peer = inner.peer.or(inner.last_peer).ok_or(NotConnected)?;
            let port = match inner.local_port {
                Some(p) => p,
                None => {
                    let p = alloc_ephemeral_port();
                    inner.local_port = Some(p);
                    drop(inner);
                    self.bind(p)?;
                    p
                }
            };
            (port, peer)
        };
        let local = SocketAddrV4::new(local_ip(), port);
        send_udp_segment(local, peer, &buf, nonblock)?;
        Ok(n)
    }

    pub fn poll_readable(&self) -> bool {
        self.rx.has_data()
    }

    pub fn poll_writable(&self) -> bool {
        let inner = self.inner.lock();
        inner.peer.is_some() || inner.last_peer.is_some()
    }

    pub fn local_port(&self) -> Option<u16> {
        self.inner.lock().local_port
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let port = self.inner.lock().local_port;
        let Some(port) = port else { return };
        let self_ptr = self as *const UdpSocket;
        let mut guard = UDP_PORTS.lock();
        if let Some(list) = guard.get_mut(&port) {
            list.retain(|w| w.upgrade().is_some());
            list.retain(|w| {
                w.upgrade()
                    .is_some_and(|sock| Arc::as_ptr(&sock) != self_ptr)
            });
            if list.is_empty() {
                guard.remove(&port);
            }
        }
    }
}

impl TcpSocket {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = sync_channel::<u8>(TCP_QUEUE, "tcp");
        Arc::new(Self {
            inner: Mutex::new(
                TcpInner {
                    local: None,
                    peer: None,
                    state: TcpState::Closed,
                    snd_nxt: 0,
                    rcv_nxt: 0,
                    listener: None,
                },
                "tcp",
            ),
            rx,
            tx,
        })
    }

    fn init_passive(
        &self,
        local_ip: Ipv4Addr,
        local_port: u16,
        remote: SocketAddrV4,
        listener: Weak<TcpListener>,
        seq: u32,
    ) {
        let mut inner = self.inner.lock();
        inner.local = Some(SocketAddrV4::new(local_ip, local_port));
        inner.peer = Some(remote);
        inner.state = TcpState::SynReceived;
        inner.rcv_nxt = seq.wrapping_add(1);
        inner.snd_nxt = 1;
        inner.listener = Some(listener);
    }

    pub fn bind(&self, port: u16) -> Result<()> {
        if port == 0 {
            return Err(InvalidArgument);
        }
        let mut inner = self.inner.lock();
        inner.local = Some(SocketAddrV4::new(local_ip(), port));
        Ok(())
    }

    pub fn connect(self: &Arc<Self>, peer: SocketAddrV4, nonblock: bool) -> Result<()> {
        let local_port = {
            let mut inner = self.inner.lock();
            if inner.state == TcpState::Established {
                return Ok(());
            }
            if inner.local.is_none() {
                inner.local = Some(SocketAddrV4::new(local_ip(), alloc_ephemeral_port()));
            }
            inner.peer = Some(peer);
            inner.state = TcpState::SynSent;
            inner.snd_nxt = 1;
            inner.rcv_nxt = 0;
            inner.local.unwrap().port()
        };
        let local = SocketAddrV4::new(local_ip(), local_port);
        let key = ConnKey::new(local_port, peer);
        TCP_CONNS.lock().insert(key, Arc::downgrade(self));
        let _ = send_tcp_segment(local, peer, 0, 0, 0x02, &[], nonblock);
        if nonblock {
            return Err(WouldBlock);
        }
        let mut seq = TCP_SEQ.load(Ordering::Acquire);
        loop {
            if self.state() == TcpState::Established {
                return Ok(());
            }
            if self.state() == TcpState::Closed {
                return Err(NotConnected);
            }
            if let Some(p) = proc::Cpus::myproc()
                && p.inner.lock().killed
            {
                return Err(Interrupted);
            }
            wait_tcp(&mut seq);
        }
    }

    pub fn read(&self, mut dst: VirtAddr, n: usize, nonblock: bool) -> Result<usize> {
        if n == 0 {
            return Ok(0);
        }
        let first = if nonblock {
            self.rx.try_recv()?
        } else {
            self.rx.recv()?
        };
        let mut buf = [0u8; 1];
        buf[0] = first;
        either_copyout(dst, &buf)?;
        dst += 1;
        let mut count = 1;
        while count < n {
            match self.rx.try_recv() {
                Ok(b) => {
                    buf[0] = b;
                    either_copyout(dst, &buf)?;
                    dst += 1;
                    count += 1;
                }
                Err(WouldBlock) => break,
                Err(_) => break,
            }
        }
        Ok(count)
    }

    pub fn write(&self, mut src: VirtAddr, mut n: usize, nonblock: bool) -> Result<usize> {
        if self.state() != TcpState::Established {
            return Err(NotConnected);
        }
        let (local, peer, mut seq, ack) = {
            let inner = self.inner.lock();
            let local = inner.local.ok_or(NotConnected)?;
            let peer = inner.peer.ok_or(NotConnected)?;
            (local, peer, inner.snd_nxt, inner.rcv_nxt)
        };
        let mut sent = 0;
        while n > 0 {
            let chunk = min(n, MSS);
            let mut buf = vec![0u8; chunk];
            either_copyin(&mut buf[..], src)?;
            src += chunk;
            send_tcp_segment(local, peer, seq, ack, 0x18, &buf, nonblock)?;
            seq = seq.wrapping_add(chunk as u32);
            sent += chunk;
            n -= chunk;
        }
        self.set_snd(seq);
        Ok(sent)
    }

    pub fn poll_readable(&self) -> bool {
        self.rx.has_data()
    }

    pub fn poll_writable(&self) -> bool {
        self.state() == TcpState::Established
    }

    fn on_segment(
        self: &Arc<Self>,
        remote: SocketAddrV4,
        seq: u32,
        ack: u32,
        flags: u16,
        payload: &[u8],
    ) {
        let mut inner = self.inner.lock();
        match inner.state {
            TcpState::SynSent => {
                if flags & 0x12 == 0x12 && ack == inner.snd_nxt {
                    inner.peer = Some(remote);
                    inner.rcv_nxt = seq.wrapping_add(1);
                    inner.state = TcpState::Established;
                    let Some(local) = inner.local else {
                        return;
                    };
                    let snd_nxt = inner.snd_nxt;
                    let rcv_nxt = inner.rcv_nxt;
                    drop(inner);
                    let _ = send_tcp_segment_nonblock(local, remote, snd_nxt, rcv_nxt, 0x10, &[]);
                    notify_tcp();
                }
            }
            TcpState::SynReceived => {
                if flags & 0x10 != 0 && ack == inner.snd_nxt {
                    inner.state = TcpState::Established;
                    let listener = inner.listener.take();
                    drop(inner);
                    if let Some(l) = listener.and_then(|w| w.upgrade()) {
                        let _ = l.enqueue(Arc::clone(self));
                    }
                    notify_tcp();
                }
            }
            TcpState::Established => {
                if !payload.is_empty() && seq == inner.rcv_nxt {
                    inner.rcv_nxt = inner.rcv_nxt.wrapping_add(payload.len() as u32);
                    for b in payload {
                        if self.tx.try_send(*b).is_err() {
                            break;
                        }
                    }
                    let local = inner.local.unwrap();
                    let peer = inner.peer.unwrap();
                    let ack = inner.rcv_nxt;
                    let snd_nxt = inner.snd_nxt;
                    drop(inner);
                    let _ = send_tcp_segment_nonblock(local, peer, snd_nxt, ack, 0x10, &[]);
                }
            }
            TcpState::Closed => {}
        }
    }

    fn state(&self) -> TcpState {
        self.inner.lock().state
    }

    pub fn is_established(&self) -> bool {
        self.state() == TcpState::Established
    }

    pub fn is_closed(&self) -> bool {
        self.state() == TcpState::Closed
    }

    pub fn local_addr(&self) -> Option<SocketAddrV4> {
        self.inner.lock().local
    }

    fn set_snd(&self, next: u32) {
        self.inner.lock().snd_nxt = next;
    }

    fn bump_snd(&self, delta: u32) {
        let mut inner = self.inner.lock();
        inner.snd_nxt = inner.snd_nxt.wrapping_add(delta);
    }

    fn snd_nxt(&self) -> u32 {
        self.inner.lock().snd_nxt
    }

    fn rcv_nxt(&self) -> u32 {
        self.inner.lock().rcv_nxt
    }
}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        let key = {
            let inner = self.inner.lock();
            let Some(peer) = inner.peer else { return };
            let Some(local) = inner.local else { return };
            ConnKey::new(local.port(), peer)
        };
        let self_ptr = self as *const TcpSocket;
        let mut guard = TCP_CONNS.lock();
        if let Some(existing) = guard.get(&key).and_then(|w| w.upgrade())
            && Arc::as_ptr(&existing) == self_ptr
        {
            guard.remove(&key);
        }
    }
}

impl TcpListener {
    pub fn new(backlog: usize, port: u16) -> Arc<Self> {
        let cap = backlog.clamp(1, TCP_BACKLOG_MAX) as isize;
        let (tx, rx) = sync_channel::<Arc<TcpSocket>>(cap, "tcplisten");
        Arc::new(Self {
            inner: Mutex::new(ListenerInner { port }, "tcplisten"),
            rx,
            tx,
        })
    }

    pub fn register(self: &Arc<Self>) -> Result<()> {
        let port = self.inner.lock().port;
        let mut guard = TCP_LISTENERS.lock();
        if guard.get(&port).and_then(|w| w.upgrade()).is_some() {
            return Err(ResourceBusy);
        }
        guard.insert(port, Arc::downgrade(self));
        Ok(())
    }

    fn enqueue(&self, sock: Arc<TcpSocket>) -> Result<()> {
        self.tx.try_send(sock)
    }

    pub fn accept(&self, nonblock: bool) -> Result<Arc<TcpSocket>> {
        if nonblock {
            self.rx.try_recv()
        } else {
            self.rx.recv()
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let port = self.inner.lock().port;
        let self_ptr = self as *const TcpListener;
        let mut guard = TCP_LISTENERS.lock();
        if let Some(existing) = guard.get(&port).and_then(|w| w.upgrade())
            && Arc::as_ptr(&existing) == self_ptr
        {
            guard.remove(&port);
        }
    }
}
