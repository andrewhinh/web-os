use alloc::vec::Vec;
use core::{
    convert::TryInto,
    sync::atomic::{Ordering, fence},
};

use crate::{
    array,
    error::{Error::*, Result},
    memlayout::VIRTIO1,
    net, proc,
    spinlock::Mutex,
};

// virtio-net mmio base at VIRTIO1
pub static NET: Mutex<NetDevice> = Mutex::new(NetDevice::new(), "virtio_net");

const NUM: usize = 8;
const RX_BUF_SIZE: usize = 2048;
const HDR_SIZE: usize = 10;
const RX_BUF_LEN: usize = RX_BUF_SIZE + HDR_SIZE;

// Memory mapped IO registers.
#[repr(usize)]
enum VirtioMMIO {
    MagicValue = 0x00,
    Version = 0x004,
    DeviceId = 0x008,
    VendorId = 0x00c,
    DeviceFeatures = 0x010,
    DriverFeatures = 0x020,
    QueueSel = 0x030,
    QueueNumMax = 0x034,
    QueueNum = 0x038,
    QueueReady = 0x044,
    QueueNotify = 0x050,
    InterruptStatus = 0x060,
    InterruptAck = 0x064,
    Status = 0x070,
    QueueDescLow = 0x080,
    QueueDescHigh = 0x084,
    DriverDescLow = 0x090,
    DriverDescHigh = 0x094,
    DeviceDescLow = 0x0a0,
    DeviceDescHigh = 0x0a4,
    Config = 0x100,
}

impl VirtioMMIO {
    fn read(self) -> u32 {
        unsafe { core::ptr::read_volatile((VIRTIO1 + self as usize) as *const u32) }
    }

    unsafe fn write(self, data: u32) {
        unsafe {
            core::ptr::write_volatile((VIRTIO1 + self as usize) as *mut u32, data);
        }
    }

    unsafe fn read_cfg8(off: usize) -> u8 {
        unsafe { core::ptr::read_volatile((VIRTIO1 + Self::Config as usize + off) as *const u8) }
    }
}

type VirtioStatus = u32;
mod virtio_status {
    pub(crate) const ACKNOWLEDGE: u32 = 0b0001;
    pub(crate) const DRIVER: u32 = 0b0010;
    pub(crate) const DRIVER_OK: u32 = 0b0100;
    pub(crate) const FEATURES_OK: u32 = 0b1000;
}

mod virtio_features {
    pub(crate) const NET_F_MAC: u32 = 1 << 5;
    pub(crate) const RING_F_INDIRECT_DESC: u32 = 1 << 28;
    pub(crate) const RING_F_EVENT_IDX: u32 = 1 << 29;
}

#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: VirtqDescFlags,
    next: u16,
}

type VirtqDescFlags = u16;
mod virtq_desc_flags {
    pub(crate) const WRITE: u16 = 0b10;
}

impl VirtqDesc {
    const fn new() -> Self {
        Self {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, align(2))]
struct VirtqAvail {
    flags: u16,
    idx: u16,
    ring: [u16; NUM],
    unused: u16,
}

impl VirtqAvail {
    const fn new() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [0; NUM],
            unused: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

impl VirtqUsedElem {
    const fn new() -> Self {
        Self { id: 0, len: 0 }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, align(4))]
struct VirtqUsed {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; NUM],
}

impl VirtqUsed {
    const fn new() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [VirtqUsedElem::new(); NUM],
        }
    }
}

pub struct NetDevice {
    rx_desc: [VirtqDesc; NUM],
    rx_avail: VirtqAvail,
    rx_used: VirtqUsed,
    tx_desc: [VirtqDesc; NUM],
    tx_avail: VirtqAvail,
    tx_used: VirtqUsed,
    rx_bufs: [[u8; RX_BUF_LEN]; NUM],
    tx_bufs: [[u8; RX_BUF_LEN]; NUM],
    tx_free: [bool; NUM],
    rx_used_idx: u16,
    tx_used_idx: u16,
    mac: [u8; 6],
}

impl NetDevice {
    const fn new() -> Self {
        Self {
            rx_desc: [VirtqDesc::new(); NUM],
            rx_avail: VirtqAvail::new(),
            rx_used: VirtqUsed::new(),
            tx_desc: [VirtqDesc::new(); NUM],
            tx_avail: VirtqAvail::new(),
            tx_used: VirtqUsed::new(),
            rx_bufs: array![[0u8; RX_BUF_LEN]; NUM],
            tx_bufs: array![[0u8; RX_BUF_LEN]; NUM],
            tx_free: [false; NUM],
            rx_used_idx: 0,
            tx_used_idx: 0,
            mac: [0; 6],
        }
    }

    unsafe fn init(&mut self) {
        unsafe {
            let mut status: VirtioStatus = 0;

            if VirtioMMIO::MagicValue.read() != 0x74726976
                || VirtioMMIO::Version.read() != 2
                || VirtioMMIO::DeviceId.read() != 1
                || VirtioMMIO::VendorId.read() != 0x554d4551
            {
                panic!("could not find virtio net");
            }

            VirtioMMIO::Status.write(status);
            status |= virtio_status::ACKNOWLEDGE;
            VirtioMMIO::Status.write(status);
            status |= virtio_status::DRIVER;
            VirtioMMIO::Status.write(status);

            let mut features = VirtioMMIO::DeviceFeatures.read();
            features &= virtio_features::NET_F_MAC;
            features &= !(virtio_features::RING_F_EVENT_IDX);
            features &= !(virtio_features::RING_F_INDIRECT_DESC);
            VirtioMMIO::DriverFeatures.write(features);

            status |= virtio_status::FEATURES_OK;
            VirtioMMIO::Status.write(status);
            status = VirtioMMIO::Status.read();
            assert!(
                status & virtio_status::FEATURES_OK != 0,
                "virtio net FEATURES_OK unset"
            );

            for i in 0..6 {
                self.mac[i] = VirtioMMIO::read_cfg8(i);
            }

            self.init_queue(
                0,
                self.rx_desc.as_ptr(),
                &self.rx_avail as *const _,
                &self.rx_used as *const _,
            );
            self.init_queue(
                1,
                self.tx_desc.as_ptr(),
                &self.tx_avail as *const _,
                &self.tx_used as *const _,
            );

            for f in self.tx_free.iter_mut() {
                *f = true;
            }
            self.rx_used_idx = 0;
            self.tx_used_idx = 0;

            for i in 0..NUM {
                self.rx_desc[i].addr = self.rx_bufs[i].as_ptr() as u64;
                self.rx_desc[i].len = RX_BUF_LEN as u32;
                self.rx_desc[i].flags = virtq_desc_flags::WRITE;
                self.rx_avail.ring[i] = i as u16;
            }
            self.rx_avail.idx = NUM as u16;
            fence(Ordering::SeqCst);
            VirtioMMIO::QueueNotify.write(0);

            status |= virtio_status::DRIVER_OK;
            VirtioMMIO::Status.write(status);
        }
    }

    unsafe fn init_queue(
        &self,
        qidx: u32,
        desc: *const VirtqDesc,
        avail: *const VirtqAvail,
        used: *const VirtqUsed,
    ) {
        unsafe {
            VirtioMMIO::QueueSel.write(qidx);
        }
        assert!(VirtioMMIO::QueueReady.read() == 0, "virtio net queue ready");
        let max = VirtioMMIO::QueueNumMax.read();
        assert!(max != 0, "virtio net queue missing");
        assert!(max >= NUM as u32, "virtio net queue too short");
        unsafe {
            VirtioMMIO::QueueNum.write(NUM as _);
            VirtioMMIO::QueueDescLow.write(desc as u64 as u32);
            VirtioMMIO::QueueDescHigh.write((desc as u64 >> 32) as u32);
            VirtioMMIO::DriverDescLow.write(avail as u64 as u32);
            VirtioMMIO::DriverDescHigh.write((avail as u64 >> 32) as u32);
            VirtioMMIO::DeviceDescLow.write(used as u64 as u32);
            VirtioMMIO::DeviceDescHigh.write((used as u64 >> 32) as u32);
            VirtioMMIO::QueueReady.write(0x1);
        }
    }

    fn alloc_tx(&mut self) -> Option<usize> {
        self.tx_free
            .iter_mut()
            .enumerate()
            .find(|(_, f)| **f)
            .map(|(i, f)| {
                *f = false;
                i
            })
    }

    fn free_tx(&mut self, idx: usize) {
        if idx < NUM {
            self.tx_free[idx] = true;
            proc::wakeup(&self.tx_free[0] as *const _ as usize);
        }
    }

    fn handle_tx(&mut self) {
        while self.tx_used_idx != self.tx_used.idx {
            let id = self.tx_used.ring[self.tx_used_idx as usize % NUM].id as usize;
            self.free_tx(id);
            self.tx_used_idx = self.tx_used_idx.wrapping_add(1);
        }
    }

    fn collect_rx(&mut self, frames: &mut Vec<Vec<u8>>) {
        while self.rx_used_idx != self.rx_used.idx {
            let used = self.rx_used.ring[self.rx_used_idx as usize % NUM];
            let id = used.id as usize;
            let len = (used.len as usize).min(RX_BUF_LEN);
            if len >= HDR_SIZE && id < NUM {
                frames.push(self.rx_bufs[id][HDR_SIZE..len].to_vec());
            }
            let slot = self.rx_avail.idx as usize % NUM;
            self.rx_avail.ring[slot] = id as u16;
            self.rx_avail.idx = self.rx_avail.idx.wrapping_add(1);
            fence(Ordering::SeqCst);
            unsafe {
                VirtioMMIO::QueueNotify.write(0);
            }
            self.rx_used_idx = self.rx_used_idx.wrapping_add(1);
        }
    }
}

impl Mutex<NetDevice> {
    fn send_frame_inner(&self, frame: &[u8], nonblock: bool) -> Result<()> {
        if frame.len() > RX_BUF_SIZE {
            return Err(InvalidArgument);
        }
        let mut guard = self.lock();
        let idx = loop {
            if let Some(idx) = guard.alloc_tx() {
                break idx;
            }
            if nonblock {
                return Err(WouldBlock);
            }
            guard = proc::sleep(&guard.tx_free[0] as *const _ as usize, guard);
        };
        guard.tx_bufs[idx][..HDR_SIZE].fill(0);
        guard.tx_bufs[idx][HDR_SIZE..HDR_SIZE + frame.len()].copy_from_slice(frame);
        guard.tx_desc[idx].addr = guard.tx_bufs[idx].as_ptr() as u64;
        guard.tx_desc[idx].len = (HDR_SIZE + frame.len()).try_into().unwrap();
        guard.tx_desc[idx].flags = 0;
        guard.tx_desc[idx].next = 0;

        let ring_idx = guard.tx_avail.idx as usize % NUM;
        guard.tx_avail.ring[ring_idx] = idx as u16;
        fence(Ordering::SeqCst);
        guard.tx_avail.idx = guard.tx_avail.idx.wrapping_add(1);
        fence(Ordering::SeqCst);
        unsafe {
            VirtioMMIO::QueueNotify.write(1);
        }
        while !guard.tx_free[idx] {
            if nonblock {
                return Ok(());
            }
            guard = proc::sleep(&guard.tx_free[0] as *const _ as usize, guard);
        }
        Ok(())
    }

    pub fn send_frame(&self, frame: &[u8]) -> Result<()> {
        self.send_frame_inner(frame, false)
    }

    pub fn try_send_frame(&self, frame: &[u8]) -> Result<()> {
        self.send_frame_inner(frame, true)
    }

    pub fn intr(&self) {
        let intr_stat = VirtioMMIO::InterruptStatus.read();
        unsafe {
            VirtioMMIO::InterruptAck.write(intr_stat & 0x3);
        }
        fence(Ordering::SeqCst);
        let mut frames = Vec::new();
        {
            let mut guard = self.lock();
            guard.handle_tx();
            guard.collect_rx(&mut frames);
        }
        for frame in frames {
            net::handle_frame(&frame);
        }
    }

    pub fn mac_addr(&self) -> [u8; 6] {
        self.lock().mac
    }
}

pub fn init() {
    unsafe {
        NET.get_mut().init();
    }
}

pub fn spawn_tasks() {}
