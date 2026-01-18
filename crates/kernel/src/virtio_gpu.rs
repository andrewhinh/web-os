use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{Ordering, fence};
use core::{cmp, mem};

use crate::{memlayout::VIRTIO2, spinlock::Mutex};

pub static GPU: Mutex<Gpu> = Mutex::new(Gpu::new_uninit(), "virtio_gpu");

#[repr(usize)]
enum VirtioMMIO {
    MagicValue = 0x00,
    Version = 0x004,
    DeviceId = 0x008,
    VenderId = 0x00c,
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
}

impl VirtioMMIO {
    fn read(self) -> u32 {
        unsafe { core::ptr::read_volatile((VIRTIO2 + self as usize) as *const u32) }
    }

    unsafe fn write(self, data: u32) {
        unsafe { core::ptr::write_volatile((VIRTIO2 + self as usize) as *mut u32, data) }
    }
}

type VirtioStatus = u32;
mod virtio_status {
    pub(crate) const ACKNOWLEDGE: u32 = 0b0001;
    pub(crate) const DRIVER: u32 = 0b0010;
    pub(crate) const DRIVER_OK: u32 = 0b0100;
    pub(crate) const FEATURES_OK: u32 = 0b1000;
}

mod virtq_desc_flags {
    pub(crate) const NEXT: u16 = 0b0001;
    pub(crate) const WRITE: u16 = 0b0010;
}
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 0x0001;

mod gpu_consts {
    pub(crate) const VIRTIO_ID_GPU: u32 = 16;

    pub(crate) const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
    pub(crate) const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
    pub(crate) const CMD_SET_SCANOUT: u32 = 0x0103;
    pub(crate) const CMD_RESOURCE_FLUSH: u32 = 0x0104;
    pub(crate) const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
    pub(crate) const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;

    pub(crate) const RESP_OK_NODATA: u32 = 0x1100;
    pub(crate) const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
    pub(crate) const RESP_ERR_UNSPEC: u32 = 0x1200;
    pub(crate) const RESP_ERR_INVALID_PARAMETER: u32 = 0x1203;

    pub(crate) const FORMAT_B8G8R8A8_UNORM: u32 = 1;
    pub(crate) const FORMAT_B8G8R8X8_UNORM: u32 = 2;
    pub(crate) const FORMAT_R8G8B8X8_UNORM: u32 = 3;
}

#[derive(Clone, Copy, Debug)]
pub enum PixelFormat {
    Bgrx8888,
    Rgbx8888,
    Bgra8888,
}

impl PixelFormat {
    fn from_virtio_format(fmt: u32) -> Option<Self> {
        match fmt {
            gpu_consts::FORMAT_B8G8R8X8_UNORM => Some(Self::Bgrx8888),
            gpu_consts::FORMAT_R8G8B8X8_UNORM => Some(Self::Rgbx8888),
            gpu_consts::FORMAT_B8G8R8A8_UNORM => Some(Self::Bgra8888),
            _ => None,
        }
    }

    pub fn pack_rgba(self, r: u8, g: u8, b: u8, a: u8) -> u32 {
        // little-endian u32 in memory -> bytes [LSB..MSB]
        match self {
            Self::Bgrx8888 => {
                ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
            }
            Self::Rgbx8888 => {
                ((a as u32) << 24) | ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
            }
            Self::Bgra8888 => {
                ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
            }
        }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
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

#[repr(C, align(2))]
#[derive(Clone, Copy)]
struct VirtqAvail<const N: usize> {
    flags: u16,
    idx: u16,
    ring: [u16; N],
    unused: u16,
}

impl<const N: usize> VirtqAvail<N> {
    const fn new() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [0; N],
            unused: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

impl VirtqUsedElem {
    const fn new() -> Self {
        Self { id: 0, len: 0 }
    }
}

#[repr(C, align(4))]
#[derive(Clone, Copy)]
struct VirtqUsed<const N: usize> {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; N],
}

impl<const N: usize> VirtqUsed<N> {
    const fn new() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [VirtqUsedElem::new(); N],
        }
    }
}

const QNUM: usize = 8;

#[repr(C)]
struct GpuQueue {
    desc: [VirtqDesc; QNUM],
    avail: VirtqAvail<QNUM>,
    used: VirtqUsed<QNUM>,
    free: [bool; QNUM],
    used_idx: u16,
}

impl GpuQueue {
    const fn new() -> Self {
        Self {
            desc: [VirtqDesc::new(); QNUM],
            avail: VirtqAvail::new(),
            used: VirtqUsed::new(),
            free: [true; QNUM],
            used_idx: 0,
        }
    }

    fn alloc_desc(&mut self) -> Option<usize> {
        self.free
            .iter_mut()
            .enumerate()
            .find(|(_, f)| **f)
            .map(|(i, f)| {
                *f = false;
                i
            })
    }

    fn free_desc(&mut self, i: usize) {
        self.desc[i] = VirtqDesc::new();
        self.free[i] = true;
    }

    fn free_chain(&mut self, mut i: usize) {
        loop {
            let flags = self.desc[i].flags;
            let next = self.desc[i].next as usize;
            self.free_desc(i);
            if (flags & virtq_desc_flags::NEXT) != 0 {
                i = next;
            } else {
                break;
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CtrlHdr {
    type_: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    padding: u32,
}

impl CtrlHdr {
    const fn new(type_: u32) -> Self {
        Self {
            type_,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            padding: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RespHdr {
    hdr: CtrlHdr,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayOne {
    r: Rect,
    enabled: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RespDisplayInfo {
    hdr: CtrlHdr,
    pmodes: [DisplayOne; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceCreate2D {
    hdr: CtrlHdr,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceAttachBacking {
    hdr: CtrlHdr,
    resource_id: u32,
    nr_entries: u32,
    entry: MemEntry,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SetScanout {
    hdr: CtrlHdr,
    r: Rect,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TransferToHost2D {
    hdr: CtrlHdr,
    r: Rect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceFlush {
    hdr: CtrlHdr,
    r: Rect,
    resource_id: u32,
    padding: u32,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct CmdBuf<const N: usize> {
    buf: [u8; N],
}

impl<const N: usize> CmdBuf<N> {
    const fn new() -> Self {
        Self { buf: [0; N] }
    }
}

pub struct Gpu {
    inited: bool,
    q: GpuQueue,

    width: usize,
    height: usize,
    stride: usize,
    bytes_per_pixel: usize,
    pixel_format: PixelFormat,

    resource_id: u32,
    scanout_id: u32,

    fb: Vec<u32>,

    // DMA-safe command buffers
    cmd_req: CmdBuf<256>,
    cmd_resp: CmdBuf<512>,
}

impl Gpu {
    const fn new_uninit() -> Self {
        Self {
            inited: false,
            q: GpuQueue::new(),
            width: 0,
            height: 0,
            stride: 0,
            bytes_per_pixel: 4,
            pixel_format: PixelFormat::Bgrx8888,
            resource_id: 1,
            scanout_id: 0,
            fb: Vec::new(),
            cmd_req: CmdBuf::new(),
            cmd_resp: CmdBuf::new(),
        }
    }

    pub fn is_inited(&self) -> bool {
        self.inited
    }

    pub fn info(&self) -> Option<(usize, usize, usize, PixelFormat)> {
        if !self.inited {
            return None;
        }
        Some((self.width, self.height, self.stride, self.pixel_format))
    }

    pub fn fb_mut(&mut self) -> Option<&mut [u32]> {
        if !self.inited {
            return None;
        }
        Some(&mut self.fb)
    }

    fn virtio_ok() -> bool {
        VirtioMMIO::MagicValue.read() == 0x7472_6976
            && VirtioMMIO::Version.read() == 2
            && VirtioMMIO::VenderId.read() == 0x554d_4551
            && VirtioMMIO::DeviceId.read() == gpu_consts::VIRTIO_ID_GPU
    }

    pub fn try_init(&mut self) -> bool {
        if self.inited {
            return true;
        }
        if !Self::virtio_ok() {
            return false;
        }

        unsafe {
            let mut status: VirtioStatus = 0;
            VirtioMMIO::Status.write(status);

            status |= virtio_status::ACKNOWLEDGE;
            VirtioMMIO::Status.write(status);

            status |= virtio_status::DRIVER;
            VirtioMMIO::Status.write(status);

            // accept whatever low 32-bit features the device offers
            let features = VirtioMMIO::DeviceFeatures.read();
            VirtioMMIO::DriverFeatures.write(features);

            status |= virtio_status::FEATURES_OK;
            VirtioMMIO::Status.write(status);

            status = VirtioMMIO::Status.read();
            if status & virtio_status::FEATURES_OK == 0 {
                return false;
            }

            // init queue 0 (controlq)
            VirtioMMIO::QueueSel.write(0);
            if VirtioMMIO::QueueReady.read() != 0 {
                return false;
            }
            let max = VirtioMMIO::QueueNumMax.read();
            if max < QNUM as u32 || max == 0 {
                return false;
            }
            VirtioMMIO::QueueNum.write(QNUM as u32);

            let desc_pa = &self.q.desc as *const _ as u64;
            let avail_pa = &self.q.avail as *const _ as u64;
            let used_pa = &self.q.used as *const _ as u64;

            VirtioMMIO::QueueDescLow.write(desc_pa as u32);
            VirtioMMIO::QueueDescHigh.write((desc_pa >> 32) as u32);
            VirtioMMIO::DriverDescLow.write(avail_pa as u32);
            VirtioMMIO::DriverDescHigh.write((avail_pa >> 32) as u32);
            VirtioMMIO::DeviceDescLow.write(used_pa as u32);
            VirtioMMIO::DeviceDescHigh.write((used_pa >> 32) as u32);

            // suppress interrupts and poll for completion (trap.rs doesn't route VIRTIO1)
            self.q.avail.flags = VIRTQ_AVAIL_F_NO_INTERRUPT;
            VirtioMMIO::QueueReady.write(0x1);

            status |= virtio_status::DRIVER_OK;
            VirtioMMIO::Status.write(status);
        }

        // query display info
        let (w, h) = self.get_display_size().unwrap_or((640, 480));
        self.width = w as usize;
        self.height = h as usize;
        self.stride = self.width;
        self.bytes_per_pixel = 4;

        self.fb = alloc::vec![0u32; self.width * self.height];

        // create a resource using a format the device accepts
        let chosen_fmt = [
            gpu_consts::FORMAT_B8G8R8X8_UNORM,
            gpu_consts::FORMAT_R8G8B8X8_UNORM,
            gpu_consts::FORMAT_B8G8R8A8_UNORM,
        ]
        .into_iter()
        .find(|fmt| self.resource_create_2d(*fmt).is_ok());

        let Some(fmt) = chosen_fmt else {
            return false;
        };
        self.pixel_format = PixelFormat::from_virtio_format(fmt).unwrap();

        if self.resource_attach_backing().is_err() {
            return false;
        }
        if self.set_scanout().is_err() {
            return false;
        }

        self.inited = true;

        // clear + flush full screen once
        self.flush_rect(0, 0, self.width, self.height);
        true
    }

    fn send_cmd<Req: Copy, Resp: Copy>(&mut self, req: &Req, resp: &mut Resp) -> Result<(), ()> {
        let req_sz = mem::size_of::<Req>();
        let resp_sz = mem::size_of::<Resp>();
        if req_sz > self.cmd_req.buf.len() || resp_sz > self.cmd_resp.buf.len() {
            return Err(());
        }

        // requests/responses must be in DMA-safe memory
        // stage into fixed, aligned buffers to avoid taking references to stack
        // temporaries
        unsafe {
            ptr::copy_nonoverlapping(
                req as *const _ as *const u8,
                self.cmd_req.buf.as_mut_ptr(),
                req_sz,
            );
            ptr::write_bytes(self.cmd_resp.buf.as_mut_ptr(), 0, resp_sz);
        }

        let Some(d0) = self.q.alloc_desc() else {
            return Err(());
        };
        let Some(d1) = self.q.alloc_desc() else {
            self.q.free_desc(d0);
            return Err(());
        };

        self.q.desc[d0].addr = self.cmd_req.buf.as_ptr() as u64;
        self.q.desc[d0].len = req_sz as u32;
        self.q.desc[d0].flags = virtq_desc_flags::NEXT;
        self.q.desc[d0].next = d1 as u16;

        self.q.desc[d1].addr = self.cmd_resp.buf.as_mut_ptr() as u64;
        self.q.desc[d1].len = resp_sz as u32;
        self.q.desc[d1].flags = virtq_desc_flags::WRITE;
        self.q.desc[d1].next = 0;

        let ring_i = (self.q.avail.idx as usize) % QNUM;
        self.q.avail.ring[ring_i] = d0 as u16;

        fence(Ordering::SeqCst);
        self.q.avail.idx = self.q.avail.idx.wrapping_add(1);
        fence(Ordering::SeqCst);

        unsafe {
            VirtioMMIO::QueueNotify.write(0);
        }

        // poll for completion (interrupts suppressed)
        loop {
            // used.idx is device-updated; must be reloaded each iteration
            let used_idx = unsafe { ptr::read_volatile(&self.q.used.idx) };
            if self.q.used_idx != used_idx {
                break;
            }
            core::hint::spin_loop();
        }
        fence(Ordering::SeqCst);

        let used_slot = (self.q.used_idx as usize) % QNUM;
        let id = self.q.used.ring[used_slot].id as usize;
        self.q.used_idx = self.q.used_idx.wrapping_add(1);
        self.q.free_chain(id);

        // clear any pending device interrupt state
        let intr_stat = VirtioMMIO::InterruptStatus.read();
        if intr_stat != 0 {
            unsafe { VirtioMMIO::InterruptAck.write(intr_stat & 0x3) };
        }

        unsafe {
            ptr::copy_nonoverlapping(
                self.cmd_resp.buf.as_ptr(),
                resp as *mut _ as *mut u8,
                resp_sz,
            );
        }
        Ok(())
    }

    fn get_display_size(&mut self) -> Option<(u32, u32)> {
        let req = CtrlHdr::new(gpu_consts::CMD_GET_DISPLAY_INFO);
        let mut resp = RespDisplayInfo {
            hdr: CtrlHdr::new(0),
            pmodes: [DisplayOne {
                r: Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                enabled: 0,
                flags: 0,
            }; 16],
        };

        self.send_cmd(&req, &mut resp).ok()?;
        if resp.hdr.type_ != gpu_consts::RESP_OK_DISPLAY_INFO {
            return None;
        }

        let mode0 = resp.pmodes[0];
        if mode0.enabled == 0 || mode0.r.width == 0 || mode0.r.height == 0 {
            return None;
        }
        Some((mode0.r.width, mode0.r.height))
    }

    fn resource_create_2d(&mut self, fmt: u32) -> Result<(), ()> {
        let req = ResourceCreate2D {
            hdr: CtrlHdr::new(gpu_consts::CMD_RESOURCE_CREATE_2D),
            resource_id: self.resource_id,
            format: fmt,
            width: self.width as u32,
            height: self.height as u32,
        };
        let mut resp = RespHdr {
            hdr: CtrlHdr::new(0),
        };
        self.send_cmd(&req, &mut resp)?;
        match resp.hdr.type_ {
            gpu_consts::RESP_OK_NODATA => Ok(()),
            gpu_consts::RESP_ERR_INVALID_PARAMETER | gpu_consts::RESP_ERR_UNSPEC => Err(()),
            _ => Err(()),
        }
    }

    fn resource_attach_backing(&mut self) -> Result<(), ()> {
        let fb_ptr = self.fb.as_ptr() as u64;
        let fb_len = (self.fb.len() * mem::size_of::<u32>()) as u32;
        let req = ResourceAttachBacking {
            hdr: CtrlHdr::new(gpu_consts::CMD_RESOURCE_ATTACH_BACKING),
            resource_id: self.resource_id,
            nr_entries: 1,
            entry: MemEntry {
                addr: fb_ptr,
                length: fb_len,
                padding: 0,
            },
        };
        let mut resp = RespHdr {
            hdr: CtrlHdr::new(0),
        };
        self.send_cmd(&req, &mut resp)?;
        if resp.hdr.type_ != gpu_consts::RESP_OK_NODATA {
            return Err(());
        }
        Ok(())
    }

    fn set_scanout(&mut self) -> Result<(), ()> {
        let req = SetScanout {
            hdr: CtrlHdr::new(gpu_consts::CMD_SET_SCANOUT),
            r: Rect {
                x: 0,
                y: 0,
                width: self.width as u32,
                height: self.height as u32,
            },
            scanout_id: self.scanout_id,
            resource_id: self.resource_id,
        };
        let mut resp = RespHdr {
            hdr: CtrlHdr::new(0),
        };
        self.send_cmd(&req, &mut resp)?;
        if resp.hdr.type_ != gpu_consts::RESP_OK_NODATA {
            return Err(());
        }
        Ok(())
    }

    pub fn flush_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if self.fb.is_empty() {
            return;
        }

        let x = cmp::min(x, self.width);
        let y = cmp::min(y, self.height);
        let w = cmp::min(w, self.width - x);
        let h = cmp::min(h, self.height - y);
        if w == 0 || h == 0 {
            return;
        }

        let offset_bytes = ((y * self.stride + x) * self.bytes_per_pixel) as u64;
        let req = TransferToHost2D {
            hdr: CtrlHdr::new(gpu_consts::CMD_TRANSFER_TO_HOST_2D),
            r: Rect {
                x: x as u32,
                y: y as u32,
                width: w as u32,
                height: h as u32,
            },
            offset: offset_bytes,
            resource_id: self.resource_id,
            padding: 0,
        };
        let mut resp = RespHdr {
            hdr: CtrlHdr::new(0),
        };
        if self.send_cmd(&req, &mut resp).is_err() {
            return;
        }

        let req2 = ResourceFlush {
            hdr: CtrlHdr::new(gpu_consts::CMD_RESOURCE_FLUSH),
            r: Rect {
                x: x as u32,
                y: y as u32,
                width: w as u32,
                height: h as u32,
            },
            resource_id: self.resource_id,
            padding: 0,
        };
        let mut resp2 = RespHdr {
            hdr: CtrlHdr::new(0),
        };
        let _ = self.send_cmd(&req2, &mut resp2);
    }

    pub fn intr(&mut self) {
        let intr_stat = VirtioMMIO::InterruptStatus.read();
        if intr_stat != 0 {
            unsafe {
                VirtioMMIO::InterruptAck.write(intr_stat & 0x3);
            }
            fence(Ordering::SeqCst);
        }
    }
}
