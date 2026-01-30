use core::{
    ptr,
    sync::atomic::{Ordering, fence},
};

use crate::{
    console::CONS,
    framebuffer,
    memlayout::{VIRTIO3, VIRTIO4},
    spinlock::Mutex,
    virtio_gpu::GPU,
};

pub static KBD: Mutex<Kbd> = Mutex::new(Kbd::new_uninit(), "virtio_kbd");
pub static MOUSE: Mutex<Mouse> = Mutex::new(Mouse::new_uninit(), "virtio_mouse");

// virtio-mmio reg offsets
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

struct Mmio<const BASE: usize>;

impl<const BASE: usize> Mmio<BASE> {
    fn read(reg: VirtioMMIO) -> u32 {
        unsafe { core::ptr::read_volatile((BASE + reg as usize) as *const u32) }
    }

    unsafe fn write(reg: VirtioMMIO, data: u32) {
        unsafe { core::ptr::write_volatile((BASE + reg as usize) as *mut u32, data) }
    }
}

type VirtioStatus = u32;
mod virtio_status {
    pub(crate) const ACKNOWLEDGE: u32 = 0b0001;
    pub(crate) const DRIVER: u32 = 0b0010;
    pub(crate) const DRIVER_OK: u32 = 0b0100;
    pub(crate) const FEATURES_OK: u32 = 0b1000;
}

// disable event index notification to ensure we get interrupts for each key
// event
const VIRTIO_RING_F_EVENT_IDX: u32 = 1 << 29;

mod virtq_desc_flags {
    pub(crate) const WRITE: u16 = 0b0010;
}

mod input_consts {
    pub(crate) const VIRTIO_ID_INPUT: u32 = 18;
    pub(crate) const EV_SYN: u16 = 0x0000;
    pub(crate) const EV_KEY: u16 = 0x0001;
    pub(crate) const EV_REL: u16 = 0x0002;
}

mod rel {
    pub(crate) const REL_X: u16 = 0;
    pub(crate) const REL_Y: u16 = 1;
    pub(crate) const REL_WHEEL: u16 = 8;
}

mod key {
    pub(crate) const KEY_ESC: u16 = 1;
    pub(crate) const KEY_1: u16 = 2;
    pub(crate) const KEY_2: u16 = 3;
    pub(crate) const KEY_3: u16 = 4;
    pub(crate) const KEY_4: u16 = 5;
    pub(crate) const KEY_5: u16 = 6;
    pub(crate) const KEY_6: u16 = 7;
    pub(crate) const KEY_7: u16 = 8;
    pub(crate) const KEY_8: u16 = 9;
    pub(crate) const KEY_9: u16 = 10;
    pub(crate) const KEY_0: u16 = 11;
    pub(crate) const KEY_MINUS: u16 = 12;
    pub(crate) const KEY_EQUAL: u16 = 13;
    pub(crate) const KEY_BACKSPACE: u16 = 14;
    pub(crate) const KEY_TAB: u16 = 15;
    pub(crate) const KEY_Q: u16 = 16;
    pub(crate) const KEY_W: u16 = 17;
    pub(crate) const KEY_E: u16 = 18;
    pub(crate) const KEY_R: u16 = 19;
    pub(crate) const KEY_T: u16 = 20;
    pub(crate) const KEY_Y: u16 = 21;
    pub(crate) const KEY_U: u16 = 22;
    pub(crate) const KEY_I: u16 = 23;
    pub(crate) const KEY_O: u16 = 24;
    pub(crate) const KEY_P: u16 = 25;
    pub(crate) const KEY_LEFTBRACE: u16 = 26;
    pub(crate) const KEY_RIGHTBRACE: u16 = 27;
    pub(crate) const KEY_ENTER: u16 = 28;
    pub(crate) const KEY_LEFTCTRL: u16 = 29;
    pub(crate) const KEY_A: u16 = 30;
    pub(crate) const KEY_S: u16 = 31;
    pub(crate) const KEY_D: u16 = 32;
    pub(crate) const KEY_F: u16 = 33;
    pub(crate) const KEY_G: u16 = 34;
    pub(crate) const KEY_H: u16 = 35;
    pub(crate) const KEY_J: u16 = 36;
    pub(crate) const KEY_K: u16 = 37;
    pub(crate) const KEY_L: u16 = 38;
    pub(crate) const KEY_SEMICOLON: u16 = 39;
    pub(crate) const KEY_APOSTROPHE: u16 = 40;
    pub(crate) const KEY_GRAVE: u16 = 41;
    pub(crate) const KEY_LEFTSHIFT: u16 = 42;
    pub(crate) const KEY_BACKSLASH: u16 = 43;
    pub(crate) const KEY_Z: u16 = 44;
    pub(crate) const KEY_X: u16 = 45;
    pub(crate) const KEY_C: u16 = 46;
    pub(crate) const KEY_V: u16 = 47;
    pub(crate) const KEY_B: u16 = 48;
    pub(crate) const KEY_N: u16 = 49;
    pub(crate) const KEY_M: u16 = 50;
    pub(crate) const KEY_COMMA: u16 = 51;
    pub(crate) const KEY_DOT: u16 = 52;
    pub(crate) const KEY_SLASH: u16 = 53;
    pub(crate) const KEY_RIGHTSHIFT: u16 = 54;
    pub(crate) const KEY_LEFTALT: u16 = 56;
    pub(crate) const KEY_SPACE: u16 = 57;
    pub(crate) const KEY_CAPSLOCK: u16 = 58;
    pub(crate) const KEY_F1: u16 = 59;
    pub(crate) const KEY_F2: u16 = 60;
    pub(crate) const KEY_F3: u16 = 61;
    pub(crate) const KEY_F4: u16 = 62;
    pub(crate) const KEY_F5: u16 = 63;
    pub(crate) const KEY_F6: u16 = 64;
    pub(crate) const KEY_F7: u16 = 65;
    pub(crate) const KEY_F8: u16 = 66;
    pub(crate) const KEY_F9: u16 = 67;
    pub(crate) const KEY_F10: u16 = 68;
    pub(crate) const KEY_RIGHTCTRL: u16 = 97;
    pub(crate) const KEY_RIGHTALT: u16 = 100;
    pub(crate) const KEY_HOME: u16 = 102;
    pub(crate) const KEY_UP: u16 = 103;
    pub(crate) const KEY_PAGEUP: u16 = 104;
    pub(crate) const KEY_LEFT: u16 = 105;
    pub(crate) const KEY_RIGHT: u16 = 106;
    pub(crate) const KEY_END: u16 = 107;
    pub(crate) const KEY_DOWN: u16 = 108;
    pub(crate) const KEY_PAGEDOWN: u16 = 109;
    pub(crate) const KEY_INSERT: u16 = 110;
    pub(crate) const KEY_DELETE: u16 = 111;
    pub(crate) const KEY_F11: u16 = 87;
    pub(crate) const KEY_F12: u16 = 88;
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

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioInputEvent {
    type_: u16,
    code: u16,
    value: u32,
}

const QNUM: usize = 32;

pub struct Kbd {
    inited: bool,
    qnum: u16,
    // single receive queue
    desc: [VirtqDesc; QNUM],
    avail: VirtqAvail<QNUM>,
    used: VirtqUsed<QNUM>,
    used_idx: u16,
    events: [VirtioInputEvent; QNUM],

    shift: bool,
    ctrl: bool,
    alt: bool,
    caps: bool,
}

impl Kbd {
    const fn new_uninit() -> Self {
        Self {
            inited: false,
            qnum: 0,
            desc: [VirtqDesc::new(); QNUM],
            avail: VirtqAvail::new(),
            used: VirtqUsed::new(),
            used_idx: 0,
            events: [VirtioInputEvent {
                type_: 0,
                code: 0,
                value: 0,
            }; QNUM],
            shift: false,
            ctrl: false,
            alt: false,
            caps: false,
        }
    }

    fn virtio_ok() -> bool {
        Mmio::<VIRTIO3>::read(VirtioMMIO::MagicValue) == 0x7472_6976
            && Mmio::<VIRTIO3>::read(VirtioMMIO::Version) == 2
            && Mmio::<VIRTIO3>::read(VirtioMMIO::VenderId) == 0x554d_4551
            && Mmio::<VIRTIO3>::read(VirtioMMIO::DeviceId) == input_consts::VIRTIO_ID_INPUT
    }

    pub fn init(&mut self) {
        if self.inited {
            return;
        }
        if !Self::virtio_ok() {
            return;
        }

        unsafe {
            let mut status: VirtioStatus = 0;
            Mmio::<VIRTIO3>::write(VirtioMMIO::Status, status);

            status |= virtio_status::ACKNOWLEDGE;
            Mmio::<VIRTIO3>::write(VirtioMMIO::Status, status);
            status |= virtio_status::DRIVER;
            Mmio::<VIRTIO3>::write(VirtioMMIO::Status, status);

            // accept device low 32-bit features, but disable EVENT_IDX
            let mut features = Mmio::<VIRTIO3>::read(VirtioMMIO::DeviceFeatures);
            features &= !VIRTIO_RING_F_EVENT_IDX;
            Mmio::<VIRTIO3>::write(VirtioMMIO::DriverFeatures, features);

            status |= virtio_status::FEATURES_OK;
            Mmio::<VIRTIO3>::write(VirtioMMIO::Status, status);
            status = Mmio::<VIRTIO3>::read(VirtioMMIO::Status);
            if status & virtio_status::FEATURES_OK == 0 {
                return;
            }

            Mmio::<VIRTIO3>::write(VirtioMMIO::QueueSel, 0);
            if Mmio::<VIRTIO3>::read(VirtioMMIO::QueueReady) != 0 {
                return;
            }
            let max = Mmio::<VIRTIO3>::read(VirtioMMIO::QueueNumMax);
            let qnum = core::cmp::min(QNUM as u32, max) as usize;
            if qnum == 0 {
                return;
            }
            self.qnum = qnum as u16;
            Mmio::<VIRTIO3>::write(VirtioMMIO::QueueNum, self.qnum as u32);

            let desc_pa = &self.desc as *const _ as u64;
            let avail_pa = &self.avail as *const _ as u64;
            let used_pa = &self.used as *const _ as u64;

            Mmio::<VIRTIO3>::write(VirtioMMIO::QueueDescLow, desc_pa as u32);
            Mmio::<VIRTIO3>::write(VirtioMMIO::QueueDescHigh, (desc_pa >> 32) as u32);
            Mmio::<VIRTIO3>::write(VirtioMMIO::DriverDescLow, avail_pa as u32);
            Mmio::<VIRTIO3>::write(VirtioMMIO::DriverDescHigh, (avail_pa >> 32) as u32);
            Mmio::<VIRTIO3>::write(VirtioMMIO::DeviceDescLow, used_pa as u32);
            Mmio::<VIRTIO3>::write(VirtioMMIO::DeviceDescHigh, (used_pa >> 32) as u32);

            Mmio::<VIRTIO3>::write(VirtioMMIO::QueueReady, 0x1);

            status |= virtio_status::DRIVER_OK;
            Mmio::<VIRTIO3>::write(VirtioMMIO::Status, status);
        }

        // post receive buffers
        let qnum = self.qnum as usize;
        for i in 0..qnum {
            self.desc[i].addr = (&mut self.events[i] as *mut VirtioInputEvent) as u64;
            self.desc[i].len = core::mem::size_of::<VirtioInputEvent>() as u32;
            self.desc[i].flags = virtq_desc_flags::WRITE;
            self.desc[i].next = 0;
            self.avail.ring[i] = i as u16;
        }
        fence(Ordering::SeqCst);
        self.avail.idx = self.qnum;
        fence(Ordering::SeqCst);
        unsafe {
            Mmio::<VIRTIO3>::write(VirtioMMIO::QueueNotify, 0);
        }

        self.inited = true;
    }

    fn inject_bytes(bytes: &[u8]) {
        for &b in bytes {
            CONS.intr(b);
        }
    }

    fn inject_byte(b: u8) {
        CONS.intr(b);
    }

    fn is_letter_key(code: u16) -> bool {
        matches!(
            code,
            key::KEY_A
                | key::KEY_B
                | key::KEY_C
                | key::KEY_D
                | key::KEY_E
                | key::KEY_F
                | key::KEY_G
                | key::KEY_H
                | key::KEY_I
                | key::KEY_J
                | key::KEY_K
                | key::KEY_L
                | key::KEY_M
                | key::KEY_N
                | key::KEY_O
                | key::KEY_P
                | key::KEY_Q
                | key::KEY_R
                | key::KEY_S
                | key::KEY_T
                | key::KEY_U
                | key::KEY_V
                | key::KEY_W
                | key::KEY_X
                | key::KEY_Y
                | key::KEY_Z
        )
    }

    fn key_to_ascii(&self, code: u16) -> Option<u8> {
        // caps XOR shift
        let shift = self.shift;
        let caps = self.caps;

        let (lo, hi) = match code {
            key::KEY_A => (b'a', b'A'),
            key::KEY_B => (b'b', b'B'),
            key::KEY_C => (b'c', b'C'),
            key::KEY_D => (b'd', b'D'),
            key::KEY_E => (b'e', b'E'),
            key::KEY_F => (b'f', b'F'),
            key::KEY_G => (b'g', b'G'),
            key::KEY_H => (b'h', b'H'),
            key::KEY_I => (b'i', b'I'),
            key::KEY_J => (b'j', b'J'),
            key::KEY_K => (b'k', b'K'),
            key::KEY_L => (b'l', b'L'),
            key::KEY_M => (b'm', b'M'),
            key::KEY_N => (b'n', b'N'),
            key::KEY_O => (b'o', b'O'),
            key::KEY_P => (b'p', b'P'),
            key::KEY_Q => (b'q', b'Q'),
            key::KEY_R => (b'r', b'R'),
            key::KEY_S => (b's', b'S'),
            key::KEY_T => (b't', b'T'),
            key::KEY_U => (b'u', b'U'),
            key::KEY_V => (b'v', b'V'),
            key::KEY_W => (b'w', b'W'),
            key::KEY_X => (b'x', b'X'),
            key::KEY_Y => (b'y', b'Y'),
            key::KEY_Z => (b'z', b'Z'),

            key::KEY_1 => (b'1', b'!'),
            key::KEY_2 => (b'2', b'@'),
            key::KEY_3 => (b'3', b'#'),
            key::KEY_4 => (b'4', b'$'),
            key::KEY_5 => (b'5', b'%'),
            key::KEY_6 => (b'6', b'^'),
            key::KEY_7 => (b'7', b'&'),
            key::KEY_8 => (b'8', b'*'),
            key::KEY_9 => (b'9', b'('),
            key::KEY_0 => (b'0', b')'),

            key::KEY_SPACE => (b' ', b' '),
            key::KEY_ENTER => (b'\n', b'\n'),
            key::KEY_TAB => (b'\t', b'\t'),
            key::KEY_BACKSPACE => (0x08, 0x08),
            key::KEY_ESC => (0x1b, 0x1b),

            key::KEY_MINUS => (b'-', b'_'),
            key::KEY_EQUAL => (b'=', b'+'),
            key::KEY_LEFTBRACE => (b'[', b'{'),
            key::KEY_RIGHTBRACE => (b']', b'}'),
            key::KEY_BACKSLASH => (b'\\', b'|'),
            key::KEY_SEMICOLON => (b';', b':'),
            key::KEY_APOSTROPHE => (b'\'', b'"'),
            key::KEY_GRAVE => (b'`', b'~'),
            key::KEY_COMMA => (b',', b'<'),
            key::KEY_DOT => (b'.', b'>'),
            key::KEY_SLASH => (b'/', b'?'),

            _ => return None,
        };

        let is_letter = Self::is_letter_key(code);
        let use_shift = if is_letter { shift ^ caps } else { shift };
        Some(if use_shift { hi } else { lo })
    }

    pub fn intr(&mut self) {
        if !self.inited {
            // allow hot-plug style init if device appears later
            self.init();
            if !self.inited {
                return;
            }
        }
        let qnum = self.qnum as usize;

        let intr_stat = Mmio::<VIRTIO3>::read(VirtioMMIO::InterruptStatus);
        unsafe {
            Mmio::<VIRTIO3>::write(VirtioMMIO::InterruptAck, intr_stat & 0x3);
        }
        fence(Ordering::SeqCst);

        let mut need_notify = false;
        // used.idx is device-updated MMIO/DMAs
        while self.used_idx != unsafe { ptr::read_volatile(&self.used.idx) } {
            fence(Ordering::SeqCst);
            let slot = (self.used_idx as usize) % qnum;
            let id = self.used.ring[slot].id as usize;

            let ev = self.events[id];

            if ev.type_ == input_consts::EV_KEY {
                let pressed = ev.value == 1 || ev.value == 2;
                match ev.code {
                    key::KEY_LEFTSHIFT | key::KEY_RIGHTSHIFT => self.shift = pressed,
                    key::KEY_LEFTCTRL | key::KEY_RIGHTCTRL => self.ctrl = pressed,
                    key::KEY_LEFTALT | key::KEY_RIGHTALT => self.alt = pressed,
                    key::KEY_CAPSLOCK if pressed => self.caps = !self.caps,
                    _ if pressed => {
                        if matches!(
                            ev.code,
                            key::KEY_UP
                                | key::KEY_DOWN
                                | key::KEY_LEFT
                                | key::KEY_RIGHT
                                | key::KEY_HOME
                                | key::KEY_END
                                | key::KEY_PAGEUP
                                | key::KEY_PAGEDOWN
                                | key::KEY_INSERT
                                | key::KEY_DELETE
                                | key::KEY_F1
                                | key::KEY_F2
                                | key::KEY_F3
                                | key::KEY_F4
                                | key::KEY_F5
                                | key::KEY_F6
                                | key::KEY_F7
                                | key::KEY_F8
                                | key::KEY_F9
                                | key::KEY_F10
                                | key::KEY_F11
                                | key::KEY_F12
                        ) {
                            // ANSI escape sequences
                            match ev.code {
                                key::KEY_UP => Self::inject_bytes(b"\x1b[A"),
                                key::KEY_DOWN => Self::inject_bytes(b"\x1b[B"),
                                key::KEY_RIGHT => Self::inject_bytes(b"\x1b[C"),
                                key::KEY_LEFT => Self::inject_bytes(b"\x1b[D"),
                                key::KEY_HOME => Self::inject_bytes(b"\x1b[H"),
                                key::KEY_END => Self::inject_bytes(b"\x1b[F"),
                                key::KEY_PAGEUP => Self::inject_bytes(b"\x1b[5~"),
                                key::KEY_PAGEDOWN => Self::inject_bytes(b"\x1b[6~"),
                                key::KEY_INSERT => Self::inject_bytes(b"\x1b[2~"),
                                key::KEY_DELETE => Self::inject_bytes(b"\x1b[3~"),
                                key::KEY_F1 => Self::inject_bytes(b"\x1bOP"),
                                key::KEY_F2 => Self::inject_bytes(b"\x1bOQ"),
                                key::KEY_F3 => Self::inject_bytes(b"\x1bOR"),
                                key::KEY_F4 => Self::inject_bytes(b"\x1bOS"),
                                key::KEY_F5 => Self::inject_bytes(b"\x1b[15~"),
                                key::KEY_F6 => Self::inject_bytes(b"\x1b[17~"),
                                key::KEY_F7 => Self::inject_bytes(b"\x1b[18~"),
                                key::KEY_F8 => Self::inject_bytes(b"\x1b[19~"),
                                key::KEY_F9 => Self::inject_bytes(b"\x1b[20~"),
                                key::KEY_F10 => Self::inject_bytes(b"\x1b[21~"),
                                key::KEY_F11 => Self::inject_bytes(b"\x1b[23~"),
                                key::KEY_F12 => Self::inject_bytes(b"\x1b[24~"),
                                _ => {}
                            }
                        } else if let Some(b) = self.key_to_ascii(ev.code) {
                            if self.ctrl && b.is_ascii_alphabetic() {
                                let lo = b.to_ascii_lowercase();
                                let ctrl_code = (lo - b'a') + 1;
                                Self::inject_byte(ctrl_code);
                            } else if self.alt {
                                Self::inject_byte(0x1b);
                                Self::inject_byte(b);
                            } else {
                                Self::inject_byte(b);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // recycle buffer
            let ring_i = (self.avail.idx as usize) % qnum;
            self.avail.ring[ring_i] = id as u16;
            fence(Ordering::SeqCst);
            self.avail.idx = self.avail.idx.wrapping_add(1);
            need_notify = true;

            self.used_idx = self.used_idx.wrapping_add(1);
        }

        if need_notify {
            fence(Ordering::SeqCst);
            unsafe { Mmio::<VIRTIO3>::write(VirtioMMIO::QueueNotify, 0) };
        }
    }
}

pub struct Mouse {
    inited: bool,
    qnum: u16,
    desc: [VirtqDesc; QNUM],
    avail: VirtqAvail<QNUM>,
    used: VirtqUsed<QNUM>,
    used_idx: u16,
    events: [VirtioInputEvent; QNUM],
    x: usize,
    y: usize,

    cursor_under: [u32; Self::CUR_W * Self::CUR_H],
    cursor_ox: usize,
    cursor_oy: usize,
    cursor_w: usize,
    cursor_h: usize,
    cursor_drawn: bool,
}

impl Mouse {
    const CUR_CX: usize = 2;
    const CUR_CY: usize = 2;
    const CUR_H: usize = 16;
    const CUR_W: usize = 16;

    const fn new_uninit() -> Self {
        Self {
            inited: false,
            qnum: 0,
            desc: [VirtqDesc::new(); QNUM],
            avail: VirtqAvail::new(),
            used: VirtqUsed::new(),
            used_idx: 0,
            events: [VirtioInputEvent {
                type_: 0,
                code: 0,
                value: 0,
            }; QNUM],
            x: 0,
            y: 0,
            cursor_under: [0; Self::CUR_W * Self::CUR_H],
            cursor_ox: 0,
            cursor_oy: 0,
            cursor_w: 0,
            cursor_h: 0,
            cursor_drawn: false,
        }
    }

    fn virtio_ok() -> bool {
        Mmio::<VIRTIO4>::read(VirtioMMIO::MagicValue) == 0x7472_6976
            && Mmio::<VIRTIO4>::read(VirtioMMIO::Version) == 2
            && Mmio::<VIRTIO4>::read(VirtioMMIO::VenderId) == 0x554d_4551
            && Mmio::<VIRTIO4>::read(VirtioMMIO::DeviceId) == input_consts::VIRTIO_ID_INPUT
    }

    pub fn init(&mut self) {
        if self.inited {
            return;
        }
        if !Self::virtio_ok() {
            return;
        }

        unsafe {
            let mut status: VirtioStatus = 0;
            Mmio::<VIRTIO4>::write(VirtioMMIO::Status, status);

            status |= virtio_status::ACKNOWLEDGE;
            Mmio::<VIRTIO4>::write(VirtioMMIO::Status, status);
            status |= virtio_status::DRIVER;
            Mmio::<VIRTIO4>::write(VirtioMMIO::Status, status);

            let mut features = Mmio::<VIRTIO4>::read(VirtioMMIO::DeviceFeatures);
            features &= !VIRTIO_RING_F_EVENT_IDX;
            Mmio::<VIRTIO4>::write(VirtioMMIO::DriverFeatures, features);

            status |= virtio_status::FEATURES_OK;
            Mmio::<VIRTIO4>::write(VirtioMMIO::Status, status);
            status = Mmio::<VIRTIO4>::read(VirtioMMIO::Status);
            if status & virtio_status::FEATURES_OK == 0 {
                return;
            }

            Mmio::<VIRTIO4>::write(VirtioMMIO::QueueSel, 0);
            if Mmio::<VIRTIO4>::read(VirtioMMIO::QueueReady) != 0 {
                return;
            }
            let max = Mmio::<VIRTIO4>::read(VirtioMMIO::QueueNumMax);
            let qnum = core::cmp::min(QNUM as u32, max) as usize;
            if qnum == 0 {
                return;
            }
            self.qnum = qnum as u16;
            Mmio::<VIRTIO4>::write(VirtioMMIO::QueueNum, self.qnum as u32);

            let desc_pa = &self.desc as *const _ as u64;
            let avail_pa = &self.avail as *const _ as u64;
            let used_pa = &self.used as *const _ as u64;

            Mmio::<VIRTIO4>::write(VirtioMMIO::QueueDescLow, desc_pa as u32);
            Mmio::<VIRTIO4>::write(VirtioMMIO::QueueDescHigh, (desc_pa >> 32) as u32);
            Mmio::<VIRTIO4>::write(VirtioMMIO::DriverDescLow, avail_pa as u32);
            Mmio::<VIRTIO4>::write(VirtioMMIO::DriverDescHigh, (avail_pa >> 32) as u32);
            Mmio::<VIRTIO4>::write(VirtioMMIO::DeviceDescLow, used_pa as u32);
            Mmio::<VIRTIO4>::write(VirtioMMIO::DeviceDescHigh, (used_pa >> 32) as u32);

            Mmio::<VIRTIO4>::write(VirtioMMIO::QueueReady, 0x1);

            status |= virtio_status::DRIVER_OK;
            Mmio::<VIRTIO4>::write(VirtioMMIO::Status, status);
        }

        let qnum = self.qnum as usize;
        for i in 0..qnum {
            self.desc[i].addr = (&mut self.events[i] as *mut VirtioInputEvent) as u64;
            self.desc[i].len = core::mem::size_of::<VirtioInputEvent>() as u32;
            self.desc[i].flags = virtq_desc_flags::WRITE;
            self.desc[i].next = 0;
            self.avail.ring[i] = i as u16;
        }
        fence(Ordering::SeqCst);
        self.avail.idx = self.qnum;
        fence(Ordering::SeqCst);
        unsafe {
            Mmio::<VIRTIO4>::write(VirtioMMIO::QueueNotify, 0);
        }

        self.inited = true;
    }

    fn draw_cursor(&mut self) {
        let mut g = GPU.lock();
        if !g.is_inited() {
            return;
        }
        let Some((w, h, stride, fmt)) = g.info() else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }

        // cursor is drawn by overlaying into the framebuffer
        // save the pixels underneath so we can restore them on the next move
        let nx = self.x.saturating_sub(Self::CUR_CX);
        let ny = self.y.saturating_sub(Self::CUR_CY);
        let ox = nx.min(w.saturating_sub(1));
        let oy = ny.min(h.saturating_sub(1));
        let cw = Self::CUR_W.min(w.saturating_sub(ox));
        let ch = Self::CUR_H.min(h.saturating_sub(oy));

        let flush_rect = {
            let Some(fb) = g.fb_mut() else {
                return;
            };

            let old = if self.cursor_drawn && self.cursor_w != 0 && self.cursor_h != 0 {
                for yy in 0..self.cursor_h {
                    for xx in 0..self.cursor_w {
                        let dst = (self.cursor_oy + yy) * stride + (self.cursor_ox + xx);
                        let src = yy * Self::CUR_W + xx;
                        fb[dst] = self.cursor_under[src];
                    }
                }
                Some((self.cursor_ox, self.cursor_oy, self.cursor_w, self.cursor_h))
            } else {
                None
            };

            for yy in 0..ch {
                for xx in 0..cw {
                    let src = (oy + yy) * stride + (ox + xx);
                    let dst = yy * Self::CUR_W + xx;
                    self.cursor_under[dst] = fb[src];
                }
            }

            // keep cursor visible even if backend interprets the top byte as alpha
            let main = fmt.pack_rgba(0, 255, 64, 0xff);
            let dim = fmt.pack_rgba(0, 90, 24, 0xff);
            const OUTLINE: [u16; 16] = [
                0x0001, 0x0003, 0x0005, 0x0009, 0x0011, 0x0021, 0x0041, 0x0081, 0x0101, 0x03E1,
                0x0049, 0x0043, 0x0040, 0x0040, 0x0040, 0x0000,
            ];
            const FILL: [u16; 16] = [
                0x0000, 0x0000, 0x0002, 0x0006, 0x000E, 0x001E, 0x003E, 0x007E, 0x00FE, 0x001E,
                0x0006, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            ];
            for yy in 0..ch {
                for xx in 0..cw {
                    let bit = 1u16 << xx;
                    let o = (OUTLINE[yy] & bit) != 0;
                    let f = (FILL[yy] & bit) != 0;
                    if !(o || f) {
                        continue;
                    }
                    let idx = (oy + yy) * stride + (ox + xx);
                    fb[idx] = if f { main } else { dim };
                }
            }

            self.cursor_ox = ox;
            self.cursor_oy = oy;
            self.cursor_w = cw;
            self.cursor_h = ch;
            self.cursor_drawn = true;

            let new = (ox, oy, cw, ch);
            match old {
                None => new,
                Some(old) => {
                    let x0 = old.0.min(new.0);
                    let y0 = old.1.min(new.1);
                    let x1 = (old.0 + old.2).max(new.0 + new.2);
                    let y1 = (old.1 + old.3).max(new.1 + new.3);
                    (x0, y0, x1 - x0, y1 - y0)
                }
            }
        };

        g.flush_rect(flush_rect.0, flush_rect.1, flush_rect.2, flush_rect.3);
    }

    fn erase_cursor(&mut self) {
        if !self.cursor_drawn || self.cursor_w == 0 || self.cursor_h == 0 {
            return;
        }
        let mut g = GPU.lock();
        if !g.is_inited() {
            return;
        }
        let Some((w, h, stride, _fmt)) = g.info() else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        let Some(fb) = g.fb_mut() else {
            return;
        };
        for yy in 0..self.cursor_h {
            for xx in 0..self.cursor_w {
                let dst = (self.cursor_oy + yy) * stride + (self.cursor_ox + xx);
                let src = yy * Self::CUR_W + xx;
                fb[dst] = self.cursor_under[src];
            }
        }
        g.flush_rect(self.cursor_ox, self.cursor_oy, self.cursor_w, self.cursor_h);
        self.cursor_drawn = false;
    }

    pub fn intr(&mut self) {
        if !self.inited {
            self.init();
            if !self.inited {
                return;
            }
        }
        let qnum = self.qnum as usize;

        let intr_stat = Mmio::<VIRTIO4>::read(VirtioMMIO::InterruptStatus);
        unsafe { Mmio::<VIRTIO4>::write(VirtioMMIO::InterruptAck, intr_stat & 0x3) };
        fence(Ordering::SeqCst);

        let mut need_notify = false;
        let mut moved = false;

        while self.used_idx != unsafe { ptr::read_volatile(&self.used.idx) } {
            fence(Ordering::SeqCst);
            let slot = (self.used_idx as usize) % qnum;
            let id = self.used.ring[slot].id as usize;
            let ev = self.events[id];

            match ev.type_ {
                input_consts::EV_REL => {
                    let delta = ev.value as i32 as isize;
                    match ev.code {
                        rel::REL_X => {
                            self.x = self.x.saturating_add_signed(delta);
                            moved = true;
                        }
                        rel::REL_Y => {
                            self.y = self.y.saturating_add_signed(delta);
                            moved = true;
                        }
                        rel::REL_WHEEL => {
                            self.erase_cursor();
                            framebuffer::scrollback(delta);
                            self.draw_cursor();
                        }
                        _ => {}
                    }
                }
                input_consts::EV_KEY => {
                    // ignore mouse buttons for now
                }
                input_consts::EV_SYN => {}
                _ => {}
            }

            let ring_i = (self.avail.idx as usize) % qnum;
            self.avail.ring[ring_i] = id as u16;
            fence(Ordering::SeqCst);
            self.avail.idx = self.avail.idx.wrapping_add(1);
            need_notify = true;

            self.used_idx = self.used_idx.wrapping_add(1);
        }

        if need_notify {
            fence(Ordering::SeqCst);
            unsafe { Mmio::<VIRTIO4>::write(VirtioMMIO::QueueNotify, 0) };
        }

        if moved {
            self.draw_cursor();
        }
    }
}

pub fn init() {
    KBD.lock().init();
    MOUSE.lock().init();
}

impl Mutex<Kbd> {
    pub fn intr(&self) {
        self.lock().intr()
    }
}

impl Mutex<Mouse> {
    pub fn intr(&self) {
        self.lock().intr()
    }
}
