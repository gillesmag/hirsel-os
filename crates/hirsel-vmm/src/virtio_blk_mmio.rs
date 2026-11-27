//! Modern Virtio 1.0 MMIO (v2) Block Device (`virtio-blk`) for Tier 2 Linux MicroVMs.
//!
//! Exposed to Linux via ACPI DSDT `LNRO0005` (`0xD000_1000`, 4 KiB, IRQ 6 -> `/dev/vda`).
//! Backed by a thread-safe `VmBlockBackend` (either a persistent slice on Hirsel-OS's
//! `virtio-blk` disk or an in-memory/uploaded ext4/squashfs image).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const GPA_VIRTIO_BLK_MMIO: u64 = 0xD000_1000;
pub const VIRTIO_BLK_IRQ: u8 = 6;
pub const BLK_SECTOR_SIZE: usize = 512;

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_VERSION: u32 = 2;
const VIRTIO_ID_BLOCK: u32 = 2;
const VIRTIO_VENDOR_HIRSEL: u32 = 0x4852_534C;

const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9;
const VIRTIO_F_VERSION_1_HI: u32 = 1 << 0;
const QUEUE_NUM_MAX: u32 = 128;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_T_GET_ID: u32 = 8;

const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// Pluggable sector-oriented storage backend for a guest `virtio-blk` device.
pub trait VmBlockBackend: Send {
    fn capacity_sectors(&self) -> u64;
    fn read_sectors(&mut self, sector: u64, dst: &mut [u8]) -> Result<(), String>;
    fn write_sectors(&mut self, sector: u64, src: &[u8]) -> Result<(), String>;
    fn flush(&mut self) -> Result<(), String>;
}

/// Simple RAM-backed block device (used for unit tests and ephemeral uploaded images).
pub struct MemBlockBackend {
    data: Vec<u8>,
}

impl MemBlockBackend {
    pub fn new(mut data: Vec<u8>) -> Self {
        let rem = data.len() % BLK_SECTOR_SIZE;
        if rem != 0 {
            data.resize(data.len() + (BLK_SECTOR_SIZE - rem), 0);
        }
        Self { data }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

impl VmBlockBackend for MemBlockBackend {
    fn capacity_sectors(&self) -> u64 {
        (self.data.len() / BLK_SECTOR_SIZE) as u64
    }

    fn read_sectors(&mut self, sector: u64, dst: &mut [u8]) -> Result<(), String> {
        let off = (sector as usize).saturating_mul(BLK_SECTOR_SIZE);
        let end = off.saturating_add(dst.len());
        if end > self.data.len() {
            return Err("read out of block device bounds".into());
        }
        dst.copy_from_slice(&self.data[off..end]);
        Ok(())
    }

    fn write_sectors(&mut self, sector: u64, src: &[u8]) -> Result<(), String> {
        let off = (sector as usize).saturating_mul(BLK_SECTOR_SIZE);
        let end = off.saturating_add(src.len());
        if end > self.data.len() {
            return Err("write out of block device bounds".into());
        }
        self.data[off..end].copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct VirtQueueState {
    num: u32,
    ready: bool,
    desc_addr: u64,
    driver_addr: u64,
    device_addr: u64,
    last_avail_idx: u16,
}

/// Virtio 1.0 MMIO v2 block device state owned by the VMM.
pub struct VirtioBlkMmio {
    backend: Arc<Mutex<dyn VmBlockBackend>>,
    capacity_sectors: u64,
    dev_features_sel: u32,
    drv_features_sel: u32,
    queue_sel: u32,
    queue: VirtQueueState,
    interrupt_status: u32,
    status: u32,
    pub reads: AtomicU64,
    pub writes: AtomicU64,
}

impl VirtioBlkMmio {
    pub fn new(backend: Arc<Mutex<dyn VmBlockBackend>>) -> Self {
        let capacity_sectors = backend.lock().unwrap().capacity_sectors();
        Self {
            backend,
            capacity_sectors,
            dev_features_sel: 0,
            drv_features_sel: 0,
            queue_sel: 0,
            queue: VirtQueueState::default(),
            interrupt_status: 0,
            status: 0,
            reads: AtomicU64::new(0),
            writes: AtomicU64::new(0),
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.interrupt_status != 0
    }

    pub fn read(&self, offset: u64, size: u8) -> u32 {
        // Config space [0x100..0x108): u64 capacity in 512-byte sectors.
        if (0x100..0x108).contains(&offset) {
            let cfg = self.capacity_sectors.to_le_bytes();
            let off = (offset - 0x100) as usize;
            let mut v = 0u32;
            for b in 0..usize::from(size).min(4) {
                if off + b < cfg.len() {
                    v |= u32::from(cfg[off + b]) << (b * 8);
                }
            }
            return v;
        }
        match offset {
            0x000 => VIRTIO_MMIO_MAGIC,
            0x004 => VIRTIO_MMIO_VERSION,
            0x008 => VIRTIO_ID_BLOCK,
            0x00C => VIRTIO_VENDOR_HIRSEL,
            0x010 => match self.dev_features_sel {
                0 => VIRTIO_BLK_F_FLUSH,
                1 => VIRTIO_F_VERSION_1_HI,
                _ => 0,
            },
            0x034 => if self.queue_sel == 0 { QUEUE_NUM_MAX } else { 0 },
            0x044 => if self.queue_sel == 0 { u32::from(self.queue.ready) } else { 0 },
            0x060 => self.interrupt_status,
            0x070 => self.status,
            0x0FC => 0,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u64, val: u32, ram: &mut [u8]) {
        match offset {
            0x014 => self.dev_features_sel = val,
            0x024 => self.drv_features_sel = val,
            0x030 => self.queue_sel = val,
            0x038 => {
                if self.queue_sel == 0 {
                    self.queue.num = val.min(QUEUE_NUM_MAX);
                }
            }
            0x044 => {
                if self.queue_sel == 0 {
                    self.queue.ready = val != 0;
                }
            }
            0x050 => {
                if val == 0 {
                    self.process_requests(ram);
                }
            }
            0x064 => self.interrupt_status &= !val,
            0x070 => {
                if val == 0 {
                    self.status = 0;
                    self.interrupt_status = 0;
                    self.queue = VirtQueueState::default();
                } else {
                    self.status = val;
                }
            }
            0x080 if self.queue_sel == 0 => {
                self.queue.desc_addr = (self.queue.desc_addr & 0xffff_ffff_0000_0000) | u64::from(val);
            }
            0x084 if self.queue_sel == 0 => {
                self.queue.desc_addr =
                    (self.queue.desc_addr & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
            }
            0x090 if self.queue_sel == 0 => {
                self.queue.driver_addr =
                    (self.queue.driver_addr & 0xffff_ffff_0000_0000) | u64::from(val);
            }
            0x094 if self.queue_sel == 0 => {
                self.queue.driver_addr =
                    (self.queue.driver_addr & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
            }
            0x0A0 if self.queue_sel == 0 => {
                self.queue.device_addr =
                    (self.queue.device_addr & 0xffff_ffff_0000_0000) | u64::from(val);
            }
            0x0A4 if self.queue_sel == 0 => {
                self.queue.device_addr =
                    (self.queue.device_addr & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
            }
            _ => {}
        }
    }

    fn process_requests(&mut self, ram: &mut [u8]) {
        if !self.queue.ready || self.queue.num == 0 {
            return;
        }
        let qsize = self.queue.num as u16;
        let Some(avail_idx) = read_u16(ram, self.queue.driver_addr + 2) else {
            return;
        };

        let mut completed = false;
        while self.queue.last_avail_idx != avail_idx {
            let slot = (self.queue.last_avail_idx % qsize) as u64;
            let Some(head) = read_u16(ram, self.queue.driver_addr + 4 + slot * 2) else {
                break;
            };
            self.queue.last_avail_idx = self.queue.last_avail_idx.wrapping_add(1);

            // Collect descriptor chain: [hdr, data0, ..., dataN, status].
            let mut chain: Vec<(u64, u32, u16)> = Vec::with_capacity(4);
            let mut idx = head;
            let mut hops = 0u32;
            while hops < self.queue.num {
                hops += 1;
                let Some((addr, len, flags, next)) = read_desc(ram, self.queue.desc_addr, idx) else {
                    break;
                };
                chain.push((addr, len, flags));
                if (flags & VIRTQ_DESC_F_NEXT) == 0 {
                    break;
                }
                idx = next;
            }
            if chain.len() < 2 {
                push_used(ram, self.queue.device_addr, qsize, head, 0);
                completed = true;
                continue;
            }

            let (hdr_addr, hdr_len, _) = chain[0];
            let (status_addr, _, _) = *chain.last().unwrap();
            let data_descs = &chain[1..chain.len() - 1];

            let mut used_len: u32 = 1; // status byte
            let status_byte = if hdr_len < 16 {
                VIRTIO_BLK_S_IOERR
            } else {
                let req_type = read_u32(ram, hdr_addr).unwrap_or(u32::MAX);
                let sector = read_u64(ram, hdr_addr + 8).unwrap_or(0);
                match req_type {
                    VIRTIO_BLK_T_IN => {
                        let mut cur_sector = sector;
                        let mut ok = true;
                        let mut bk = self.backend.lock().unwrap();
                        for &(addr, len, flags) in data_descs {
                            if (flags & VIRTQ_DESC_F_WRITE) == 0 {
                                continue;
                            }
                            let off = addr as usize;
                            let n = len as usize;
                            if off.saturating_add(n) > ram.len() || n % BLK_SECTOR_SIZE != 0 {
                                ok = false;
                                break;
                            }
                            if bk.read_sectors(cur_sector, &mut ram[off..off + n]).is_err() {
                                ok = false;
                                break;
                            }
                            cur_sector += (n / BLK_SECTOR_SIZE) as u64;
                            used_len = used_len.saturating_add(len);
                        }
                        if ok {
                            self.reads.fetch_add(1, Ordering::Relaxed);
                            VIRTIO_BLK_S_OK
                        } else {
                            VIRTIO_BLK_S_IOERR
                        }
                    }
                    VIRTIO_BLK_T_OUT => {
                        let mut cur_sector = sector;
                        let mut ok = true;
                        let mut bk = self.backend.lock().unwrap();
                        for &(addr, len, _) in data_descs {
                            let off = addr as usize;
                            let n = len as usize;
                            if off.saturating_add(n) > ram.len() || n % BLK_SECTOR_SIZE != 0 {
                                ok = false;
                                break;
                            }
                            if bk.write_sectors(cur_sector, &ram[off..off + n]).is_err() {
                                ok = false;
                                break;
                            }
                            cur_sector += (n / BLK_SECTOR_SIZE) as u64;
                        }
                        if ok {
                            self.writes.fetch_add(1, Ordering::Relaxed);
                            VIRTIO_BLK_S_OK
                        } else {
                            VIRTIO_BLK_S_IOERR
                        }
                    }
                    VIRTIO_BLK_T_FLUSH => {
                        let mut bk = self.backend.lock().unwrap();
                        if bk.flush().is_ok() {
                            VIRTIO_BLK_S_OK
                        } else {
                            VIRTIO_BLK_S_IOERR
                        }
                    }
                    VIRTIO_BLK_T_GET_ID => {
                        if let Some(&(addr, len, _)) = data_descs.first() {
                            let id = b"hirsel-vblk0";
                            let n = id.len().min(len as usize);
                            let off = addr as usize;
                            if off.saturating_add(n) <= ram.len() {
                                ram[off..off + n].copy_from_slice(&id[..n]);
                                used_len = used_len.saturating_add(n as u32);
                            }
                        }
                        VIRTIO_BLK_S_OK
                    }
                    _ => VIRTIO_BLK_S_UNSUPP,
                }
            };

            if let Ok(off) = usize::try_from(status_addr) {
                if off < ram.len() {
                    ram[off] = status_byte;
                }
            }
            push_used(ram, self.queue.device_addr, qsize, head, used_len);
            completed = true;
        }

        if completed {
            self.interrupt_status |= 1;
        }
    }
}

fn read_u16(ram: &[u8], gpa: u64) -> Option<u16> {
    let off = usize::try_from(gpa).ok()?;
    Some(u16::from_le_bytes(ram.get(off..off + 2)?.try_into().unwrap()))
}

fn read_u32(ram: &[u8], gpa: u64) -> Option<u32> {
    let off = usize::try_from(gpa).ok()?;
    Some(u32::from_le_bytes(ram.get(off..off + 4)?.try_into().unwrap()))
}

fn read_u64(ram: &[u8], gpa: u64) -> Option<u64> {
    let off = usize::try_from(gpa).ok()?;
    Some(u64::from_le_bytes(ram.get(off..off + 8)?.try_into().unwrap()))
}

fn write_u16(ram: &mut [u8], gpa: u64, val: u16) {
    if let Ok(off) = usize::try_from(gpa) {
        if let Some(dst) = ram.get_mut(off..off + 2) {
            dst.copy_from_slice(&val.to_le_bytes());
        }
    }
}

fn write_u32(ram: &mut [u8], gpa: u64, val: u32) {
    if let Ok(off) = usize::try_from(gpa) {
        if let Some(dst) = ram.get_mut(off..off + 4) {
            dst.copy_from_slice(&val.to_le_bytes());
        }
    }
}

fn read_desc(ram: &[u8], desc_base: u64, idx: u16) -> Option<(u64, u32, u16, u16)> {
    let off = desc_base.checked_add(u64::from(idx) * 16)?;
    Some((
        read_u64(ram, off)?,
        read_u32(ram, off + 8)?,
        read_u16(ram, off + 12)?,
        read_u16(ram, off + 14)?,
    ))
}

fn push_used(ram: &mut [u8], device_addr: u64, qsize: u16, id: u16, len: u32) {
    let Some(used_idx) = read_u16(ram, device_addr + 2) else {
        return;
    };
    let slot = (used_idx % qsize) as u64;
    let elem = device_addr + 4 + slot * 8;
    write_u32(ram, elem, u32::from(id));
    write_u32(ram, elem + 4, len);
    core::sync::atomic::fence(Ordering::Release);
    write_u16(ram, device_addr + 2, used_idx.wrapping_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_blk_mmio_write_and_read_roundtrip() {
        let bk = Arc::new(Mutex::new(MemBlockBackend::new(vec![0u8; 64 * 1024])));
        let mut dev = VirtioBlkMmio::new(bk);
        let mut ram = vec![0u8; 64 * 1024];

        assert_eq!(dev.read(0x000, 4), VIRTIO_MMIO_MAGIC);
        assert_eq!(dev.read(0x008, 4), VIRTIO_ID_BLOCK);
        assert_eq!(dev.read(0x100, 4), 128); // 64 KiB / 512 = 128 sectors

        // Setup queue 0 at desc=0x1000, driver=0x2000, device=0x3000.
        dev.write(0x030, 0, &mut ram);
        dev.write(0x038, 16, &mut ram);
        dev.write(0x080, 0x1000, &mut ram);
        dev.write(0x090, 0x2000, &mut ram);
        dev.write(0x0A0, 0x3000, &mut ram);
        dev.write(0x044, 1, &mut ram);

        // Request 1: WRITE 1 sector at sector 3.
        ram[0x4000..0x4004].copy_from_slice(&VIRTIO_BLK_T_OUT.to_le_bytes());
        ram[0x4008..0x4010].copy_from_slice(&3u64.to_le_bytes());
        ram[0x5000..0x500C].copy_from_slice(b"HIRSEL-DISK!");
        ram[0x6000] = 0xff;
        // desc 0: hdr -> 1
        ram[0x1000..0x1008].copy_from_slice(&0x4000u64.to_le_bytes());
        ram[0x1008..0x100C].copy_from_slice(&16u32.to_le_bytes());
        ram[0x100C..0x100E].copy_from_slice(&VIRTQ_DESC_F_NEXT.to_le_bytes());
        ram[0x100E..0x1010].copy_from_slice(&1u16.to_le_bytes());
        // desc 1: data -> 2
        ram[0x1010..0x1018].copy_from_slice(&0x5000u64.to_le_bytes());
        ram[0x1018..0x101C].copy_from_slice(&512u32.to_le_bytes());
        ram[0x101C..0x101E].copy_from_slice(&VIRTQ_DESC_F_NEXT.to_le_bytes());
        ram[0x101E..0x1020].copy_from_slice(&2u16.to_le_bytes());
        // desc 2: status
        ram[0x1020..0x1028].copy_from_slice(&0x6000u64.to_le_bytes());
        ram[0x1028..0x102C].copy_from_slice(&1u32.to_le_bytes());
        ram[0x102C..0x102E].copy_from_slice(&VIRTQ_DESC_F_WRITE.to_le_bytes());
        write_u16(&mut ram, 0x2004, 0);
        write_u16(&mut ram, 0x2002, 1);
        dev.write(0x050, 0, &mut ram);
        assert_eq!(ram[0x6000], VIRTIO_BLK_S_OK);
        assert!(dev.irq_pending());

        // Request 2: READ sector 3 into 0x7000.
        ram[0x4000..0x4004].copy_from_slice(&VIRTIO_BLK_T_IN.to_le_bytes());
        ram[0x1010..0x1018].copy_from_slice(&0x7000u64.to_le_bytes());
        ram[0x101C..0x101E]
            .copy_from_slice(&(VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE).to_le_bytes());
        write_u16(&mut ram, 0x2006, 0);
        write_u16(&mut ram, 0x2002, 2);
        dev.write(0x050, 0, &mut ram);
        assert_eq!(ram[0x6000], VIRTIO_BLK_S_OK);
        assert_eq!(&ram[0x7000..0x700C], b"HIRSEL-DISK!");
    }
}
