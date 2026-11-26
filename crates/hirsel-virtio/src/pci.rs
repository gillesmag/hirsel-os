//! Ring-3 PCI configuration space scanner (`0xCF8`/`0xCFC`) and Virtio 1.0 Modern PCI transport.

#![cfg(target_os = "fuchsia")]

use crate::dma::{Bti, MmioRegion};
use crate::queue::SplitVirtqueue;
use core::arch::asm;
use core::sync::atomic::{Ordering, fence};
use zx_sys::*;

const PCI_CONFIG_ADDR: u16 = 0x0CF8;
const PCI_CONFIG_DATA: u16 = 0x0CFC;

pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
pub const VIRTIO_DEV_NET_TRANSITIONAL: u16 = 0x1000;
pub const VIRTIO_DEV_BLK_TRANSITIONAL: u16 = 0x1001;
pub const VIRTIO_DEV_NET_MODERN: u16 = 0x1041;
pub const VIRTIO_DEV_BLK_MODERN: u16 = 0x1042;

const PCI_CAP_ID_VNDR: u8 = 0x09;
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

// Virtio 1.0 common config register offsets.
const COMMON_DEV_FEAT_SEL: usize = 0x00;
const COMMON_DEV_FEAT: usize = 0x04;
const COMMON_DRV_FEAT_SEL: usize = 0x08;
const COMMON_DRV_FEAT: usize = 0x0C;
const COMMON_STATUS: usize = 0x14;
const COMMON_Q_SELECT: usize = 0x16;
const COMMON_Q_SIZE: usize = 0x18;
const COMMON_Q_ENABLE: usize = 0x1C;
const COMMON_Q_NOTIFY_OFF: usize = 0x1E;
const COMMON_Q_DESC_LO: usize = 0x20;
const COMMON_Q_DESC_HI: usize = 0x24;
const COMMON_Q_AVAIL_LO: usize = 0x28;
const COMMON_Q_AVAIL_HI: usize = 0x2C;
const COMMON_Q_USED_LO: usize = 0x30;
const COMMON_Q_USED_HI: usize = 0x34;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

#[inline]
unsafe fn outl(port: u16, val: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

#[derive(Clone, Copy, Debug)]
pub struct PciAddr {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciAddr {
    pub fn bti_id(self) -> u64 {
        ((self.bus as u64) << 8) | ((self.dev as u64) << 3) | (self.func as u64)
    }
}

/// Ring-3 access to legacy x86 PCI Type-1 configuration space (`0xCF8`/`0xCFC`).
pub struct PciBus;

impl PciBus {
    pub fn open(ioport_resource: zx_handle_t) -> Result<Self, String> {
        if ioport_resource == ZX_HANDLE_INVALID {
            return Err("ioport_resource is invalid".into());
        }
        let st = unsafe { zx_ioports_request(ioport_resource, PCI_CONFIG_ADDR, 8) };
        if st != ZX_OK {
            return Err(format!("zx_ioports_request(PCI 0xCF8) failed: zx_status={st}"));
        }
        Ok(Self)
    }

    pub fn read_u32(&self, addr: PciAddr, offset: u8) -> u32 {
        let cfg = 0x8000_0000u32
            | ((addr.bus as u32) << 16)
            | ((addr.dev as u32) << 11)
            | ((addr.func as u32) << 8)
            | ((offset as u32) & 0xFC);
        unsafe {
            outl(PCI_CONFIG_ADDR, cfg);
            inl(PCI_CONFIG_DATA)
        }
    }

    pub fn write_u32(&self, addr: PciAddr, offset: u8, val: u32) {
        let cfg = 0x8000_0000u32
            | ((addr.bus as u32) << 16)
            | ((addr.dev as u32) << 11)
            | ((addr.func as u32) << 8)
            | ((offset as u32) & 0xFC);
        unsafe {
            outl(PCI_CONFIG_ADDR, cfg);
            outl(PCI_CONFIG_DATA, val);
        }
    }

    pub fn read_u16(&self, addr: PciAddr, offset: u8) -> u16 {
        let shift = ((offset & 2) as u32) * 8;
        ((self.read_u32(addr, offset) >> shift) & 0xFFFF) as u16
    }

    pub fn read_u8(&self, addr: PciAddr, offset: u8) -> u8 {
        let shift = ((offset & 3) as u32) * 8;
        ((self.read_u32(addr, offset) >> shift) & 0xFF) as u8
    }

    /// Scans bus 0 for a PCI device matching `vendor_id` and any ID in `device_ids`.
    pub fn find_device(&self, vendor_id: u16, device_ids: &[u16]) -> Option<(PciAddr, u16)> {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let addr = PciAddr { bus: 0, dev, func };
                let id = self.read_u32(addr, 0x00);
                let vid = (id & 0xFFFF) as u16;
                if vid == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let did = ((id >> 16) & 0xFFFF) as u16;
                if vid == vendor_id && device_ids.contains(&did) {
                    return Some((addr, did));
                }
                if func == 0 && (self.read_u8(addr, 0x0E) & 0x80) == 0 {
                    break;
                }
            }
        }
        None
    }

    fn read_bar_paddr(&self, addr: PciAddr, bar_idx: u8) -> Result<u64, String> {
        if bar_idx >= 6 {
            return Err(format!("invalid PCI BAR index {bar_idx}"));
        }
        let off = 0x10 + bar_idx * 4;
        let lo = self.read_u32(addr, off);
        if (lo & 1) != 0 {
            return Err(format!("PCI BAR{bar_idx} is I/O space, expected MMIO"));
        }
        let is_64 = ((lo >> 1) & 0x3) == 2;
        let base_lo = (lo & !0xFu32) as u64;
        let base_hi = if is_64 {
            (self.read_u32(addr, off + 4) as u64) << 32
        } else {
            0
        };
        let paddr = base_hi | base_lo;
        if paddr == 0 {
            return Err(format!("PCI BAR{bar_idx} has unassigned address 0"));
        }
        Ok(paddr)
    }
}

#[derive(Clone, Copy)]
struct CapLoc {
    bar: u8,
    offset: usize,
    length: usize,
}

/// Mapped Virtio 1.0 Modern PCI transport for one device.
pub struct VirtioPciTransport {
    pub addr: PciAddr,
    pub device_id: u16,
    common: MmioRegion,
    notify: MmioRegion,
    device: MmioRegion,
    notify_off_multiplier: usize,
}

impl VirtioPciTransport {
    pub fn open(
        bus: &PciBus,
        addr: PciAddr,
        device_id: u16,
        mmio_resource: zx_handle_t,
    ) -> Result<Self, String> {
        // Enable I/O Space (0), Memory Space (1), and Bus Master (2) in PCI Command register.
        let cmd_status = bus.read_u32(addr, 0x04);
        bus.write_u32(addr, 0x04, cmd_status | 0x07);

        let status = (cmd_status >> 16) as u16;
        if (status & 0x10) == 0 {
            return Err("PCI device has no capability list".into());
        }

        let mut common_cap: Option<CapLoc> = None;
        let mut notify_cap: Option<CapLoc> = None;
        let mut device_cap: Option<CapLoc> = None;
        let mut notify_off_multiplier: usize = 0;

        let mut ptr = bus.read_u8(addr, 0x34) & !3;
        for _ in 0..64 {
            if ptr == 0 {
                break;
            }
            let cap_id = bus.read_u8(addr, ptr);
            let next = bus.read_u8(addr, ptr + 1) & !3;
            if cap_id == PCI_CAP_ID_VNDR {
                let cfg_type = bus.read_u8(addr, ptr + 3);
                let bar = bus.read_u8(addr, ptr + 4);
                let offset = bus.read_u32(addr, ptr + 8) as usize;
                let length = bus.read_u32(addr, ptr + 12) as usize;
                let loc = CapLoc {
                    bar,
                    offset,
                    length: length.max(64),
                };
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => common_cap = Some(loc),
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        notify_cap = Some(loc);
                        notify_off_multiplier = bus.read_u32(addr, ptr + 16) as usize;
                    }
                    VIRTIO_PCI_CAP_DEVICE_CFG => device_cap = Some(loc),
                    _ => {}
                }
            }
            ptr = next;
        }

        let c = common_cap.ok_or_else(|| "missing Virtio COMMON_CFG capability".to_string())?;
        let n = notify_cap.ok_or_else(|| "missing Virtio NOTIFY_CFG capability".to_string())?;
        let d = device_cap.ok_or_else(|| "missing Virtio DEVICE_CFG capability".to_string())?;

        let map_cap = |cap: CapLoc| -> Result<MmioRegion, String> {
            let bar_paddr = bus.read_bar_paddr(addr, cap.bar)?;
            MmioRegion::map(mmio_resource, bar_paddr + cap.offset as u64, cap.length)
        };

        Ok(Self {
            addr,
            device_id,
            common: map_cap(c)?,
            notify: map_cap(n)?,
            device: map_cap(d)?,
            notify_off_multiplier,
        })
    }

    /// Runs the Virtio 1.0 reset + feature negotiation handshake up to `FEATURES_OK`.
    pub fn negotiate_features(&self, req_lo: u32, req_hi: u32) -> Result<(u32, u32), String> {
        // 1. Reset device.
        self.common.write_u8(COMMON_STATUS, 0);
        for _ in 0..10_000 {
            if self.common.read_u8(COMMON_STATUS) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // 2. ACKNOWLEDGE | DRIVER.
        self.common
            .write_u8(COMMON_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // 3. Read offered features and select intersection with requested features.
        self.common.write_u32(COMMON_DEV_FEAT_SEL, 0);
        let dev_lo = self.common.read_u32(COMMON_DEV_FEAT);
        self.common.write_u32(COMMON_DEV_FEAT_SEL, 1);
        let dev_hi = self.common.read_u32(COMMON_DEV_FEAT);

        // VIRTIO_F_VERSION_1 is bit 32 => bit 0 of high word.
        if (dev_hi & 1) == 0 {
            return Err("device does not offer VIRTIO_F_VERSION_1".into());
        }
        let drv_lo = dev_lo & req_lo;
        let drv_hi = (dev_hi & req_hi) | 1;

        self.common.write_u32(COMMON_DRV_FEAT_SEL, 0);
        self.common.write_u32(COMMON_DRV_FEAT, drv_lo);
        self.common.write_u32(COMMON_DRV_FEAT_SEL, 1);
        self.common.write_u32(COMMON_DRV_FEAT, drv_hi);

        // 4. Set FEATURES_OK and verify acceptance.
        self.common.write_u8(
            COMMON_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        if (self.common.read_u8(COMMON_STATUS) & STATUS_FEATURES_OK) == 0 {
            return Err("Virtio device rejected negotiated features".into());
        }
        Ok((drv_lo, drv_hi))
    }

    /// Configures and enables queue `q_idx` with size up to `want_size`.
    pub fn setup_queue(
        &self,
        q_idx: u16,
        want_size: u16,
        bti: &Bti,
    ) -> Result<SplitVirtqueue, String> {
        self.common.write_u16(COMMON_Q_SELECT, q_idx);
        let max_size = self.common.read_u16(COMMON_Q_SIZE);
        if max_size == 0 {
            return Err(format!("Virtio queue {q_idx} unavailable (size=0)"));
        }
        let size = max_size.min(want_size);
        let notify_off = self.common.read_u16(COMMON_Q_NOTIFY_OFF);
        let vq = SplitVirtqueue::new(bti, size, notify_off)?;

        self.common.write_u16(COMMON_Q_SIZE, size);
        let desc = vq.desc_paddr();
        let avail = vq.avail_paddr();
        let used = vq.used_paddr();
        self.common.write_u32(COMMON_Q_DESC_LO, desc as u32);
        self.common.write_u32(COMMON_Q_DESC_HI, (desc >> 32) as u32);
        self.common.write_u32(COMMON_Q_AVAIL_LO, avail as u32);
        self.common
            .write_u32(COMMON_Q_AVAIL_HI, (avail >> 32) as u32);
        self.common.write_u32(COMMON_Q_USED_LO, used as u32);
        self.common.write_u32(COMMON_Q_USED_HI, (used >> 32) as u32);
        fence(Ordering::SeqCst);
        self.common.write_u16(COMMON_Q_ENABLE, 1);
        Ok(vq)
    }

    pub fn set_driver_ok(&self) {
        let cur = self.common.read_u8(COMMON_STATUS);
        self.common.write_u8(COMMON_STATUS, cur | STATUS_DRIVER_OK);
        fence(Ordering::SeqCst);
    }

    #[inline]
    pub fn notify_queue(&self, q_idx: u16, notify_off: u16) {
        fence(Ordering::SeqCst);
        let off = (notify_off as usize) * self.notify_off_multiplier;
        self.notify.write_u16(off, q_idx);
    }

    pub fn device_cfg(&self) -> &MmioRegion {
        &self.device
    }
}
