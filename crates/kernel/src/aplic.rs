use core::ptr;

use crate::memlayout::{
    APLIC_M, APLIC_S, IMSIC_M, IMSIC_S, UART0_HART, UART0_IRQ, VIRTIO0_HART, VIRTIO0_IRQ,
    VIRTIO1_HART, VIRTIO1_IRQ,
};

// Register offsets
const DOMAINCFG: usize = 0x0000;
const SOURCECFG_BASE: usize = 0x0004; // sourcecfg[irq-1]

// AIA spec v1.0
const MMSIADDRCFG: usize = 0x1BC0;
const MMSIADDRCFGH: usize = 0x1BC4;
const SMSIADDRCFG: usize = 0x1BC8;
const SMSIADDRCFGH: usize = 0x1BCC;

const SETIE_BASE: usize = 0x1E00; // setie[idx]
const CLRIE_BASE: usize = 0x1F00; // clrie[idx]

const TARGET_BASE: usize = 0x3004; // target[irq-1]

#[repr(u32)]
#[derive(Clone, Copy)]
pub enum SourceMode {
    Inactive = 0,
    Detached = 1,
    RisingEdge = 4,
    FallingEdge = 5,
    LevelHigh = 6,
    LevelLow = 7,
}

#[derive(Clone, Copy)]
struct Aplic {
    base: usize,
}

impl Aplic {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline]
    fn reg32(&self, off: usize) -> *mut u32 {
        (self.base + off) as *mut u32
    }

    #[inline]
    fn write32(&self, off: usize, v: u32) {
        unsafe { ptr::write_volatile(self.reg32(off), v) }
    }

    #[inline]
    fn set_domaincfg(&self, bigendian: bool, msimode: bool, enabled: bool) {
        let enabled = u32::from(enabled);
        let msimode = u32::from(msimode);
        let bigendian = u32::from(bigendian);
        self.write32(DOMAINCFG, (enabled << 8) | (msimode << 2) | bigendian);
    }

    #[inline]
    fn set_sourcecfg(&self, irq: u32, mode: SourceMode) {
        assert!(irq > 0 && irq < 1024);
        let off = SOURCECFG_BASE + (irq as usize - 1) * 4;
        self.write32(off, mode as u32);
    }

    #[inline]
    fn sourcecfg_delegate(&self, irq: u32, child: u32) {
        assert!(irq > 0 && irq < 1024);
        let off = SOURCECFG_BASE + (irq as usize - 1) * 4;
        self.write32(off, (1 << 10) | (child & 0x3ff));
    }

    // Configure machine MSI base + hart-index packing.
    //
    // QEMU virt IMSIC interrupt files are 4KiB apart (1 page). In PPN units this
    // is stride 1, so LHXS=0. With 8 harts, LHXW=3 is sufficient.
    #[inline]
    fn set_msiaddr_m_virt(&self, imsic_m_base: usize) {
        let base_ppn = (imsic_m_base as u64) >> 12;
        let low = (base_ppn & 0xffff_ffff) as u32;
        let high_ppn = ((base_ppn >> 32) & 0x0fff) as u32;

        let lhxw: u32 = 3;
        let lhxs: u32 = 0;

        // mmsiaddrcfgh:
        // - bits 15:12 LHXW
        // - bits 22:20 LHXS
        // - bits 11:0 High Base PPN
        // Leave HHXW/HHXS = 0 (no hart groups), L=0 (unlocked).
        let mmsiaddrcfgh = (lhxs << 20) | (lhxw << 12) | high_ppn;

        self.write32(MMSIADDRCFG, low);
        self.write32(MMSIADDRCFGH, mmsiaddrcfgh);
    }

    // Configure supervisor MSI base (Base PPN + LHXS).
    #[inline]
    fn set_msiaddr_s_virt(&self, imsic_s_base: usize) {
        let base_ppn = (imsic_s_base as u64) >> 12;
        let low = (base_ppn & 0xffff_ffff) as u32;
        let high_ppn = ((base_ppn >> 32) & 0x0fff) as u32;

        let lhxs: u32 = 0;

        // smsiaddrcfgh:
        // - bits 22:20 LHXS
        // - bits 11:0 High Base PPN
        let smsiaddrcfgh = (lhxs << 20) | high_ppn;

        self.write32(SMSIADDRCFG, low);
        self.write32(SMSIADDRCFGH, smsiaddrcfgh);
    }

    #[inline]
    fn set_ie(&self, irq: u32, enabled: bool) {
        assert!(irq > 0 && irq < 1024);
        let idx = (irq / 32) as usize;
        let bit = (irq % 32) as usize;
        let off = if enabled { SETIE_BASE } else { CLRIE_BASE };
        self.write32(off + idx * 4, 1u32 << bit);
    }

    #[inline]
    fn set_target_msi(&self, irq: u32, hart: u32, guest: u32, eiid: u32) {
        assert!(irq > 0 && irq < 1024);
        let off = TARGET_BASE + (irq as usize - 1) * 4;
        self.write32(off, (hart << 18) | (guest << 12) | (eiid & 0x0fff));
    }
}

pub fn init() {
    let root = Aplic::new(APLIC_M);
    let sup = Aplic::new(APLIC_S);

    // Enable APLICs, MSI delivery mode.
    root.set_domaincfg(false, true, true);
    sup.set_domaincfg(false, true, true);

    // Root domain MSI address configuration (AIA spec 4.5.3/4.5.4).
    // Needed for APLIC_S (supervisor domain) MSI delivery address construction.
    root.set_msiaddr_m_virt(IMSIC_M);
    root.set_msiaddr_s_virt(IMSIC_S);

    // Root delegates wired sources to the supervisor (child) domain.
    // Without this, the child domain won't see the device IRQs.
    for irq in [UART0_IRQ, VIRTIO0_IRQ, VIRTIO1_IRQ] {
        root.sourcecfg_delegate(irq, 0);
    }

    // Devices are wired to the delegated (S) APLIC on QEMU virt,aia=aplic-imsic.
    // Configure sources to deliver MSIs with EIID == irq.
    // Route all device MSIs to hart0 so early boot doesn't depend on other harts
    // having IMSIC/trap fully initialized yet.
    for irq in [UART0_IRQ, VIRTIO0_IRQ, VIRTIO1_IRQ] {
        let guest = match irq {
            UART0_IRQ => UART0_HART as u32,
            VIRTIO0_IRQ => VIRTIO0_HART as u32,
            VIRTIO1_IRQ => VIRTIO1_HART as u32,
            _ => 0,
        };
        sup.set_target_msi(irq, UART0_HART as u32, guest, irq);
        sup.set_sourcecfg(irq, SourceMode::LevelHigh);
        sup.set_ie(irq, true);
    }
}
