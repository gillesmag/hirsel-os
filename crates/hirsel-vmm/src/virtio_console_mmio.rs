//! Modern Virtio 1.0 MMIO (v2) Console Device (`virtio-console`) for Tier 2 Linux MicroVMs.
//!
//! Exposed to Linux via ACPI DSDT `LNRO0005` (`0xD000_2000`, 4 KiB, IRQ 7 -> `/dev/hvc0`).
//! Provides a dedicated, zero-network guest-agent exec channel so `hirsel-os-ctl vm exec`
//! works even on fully air-gapped (`--no-net`) MicroVMs without SSH or TCP/IP!

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const GPA_VIRTIO_CON_MMIO: u64 = 0xD000_2000;
pub const VIRTIO_CON_IRQ: u8 = 3;

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_VERSION: u32 = 2;
const VIRTIO_ID_CONSOLE: u32 = 3;
const VIRTIO_VENDOR_HIRSEL: u32 = 0x4852_534C;

const VIRTIO_F_VERSION_1_HI: u32 = 1 << 0;
const QUEUE_NUM_MAX: u32 = 64;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Thread-safe bidirectional byte stream between host `vm exec` and guest `/dev/hvc0`.
pub struct VirtioConsoleChannel {
    /// Bytes queued by host for guest `/dev/hvc0` read.
    to_guest: Mutex<VecDeque<u8>>,
    /// Bytes written by guest to `/dev/hvc0`, readable by host.
    from_guest: Mutex<VecDeque<u8>>,
    wake_guest: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    pub rx_bytes: AtomicU64,
    pub tx_bytes: AtomicU64,
}

impl VirtioConsoleChannel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            to_guest: Mutex::new(VecDeque::with_capacity(4096)),
            from_guest: Mutex::new(VecDeque::with_capacity(16 * 1024)),
            wake_guest: Mutex::new(None),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
        })
    }

    pub fn set_wake_guest(&self, f: Option<Box<dyn Fn() + Send + Sync>>) {
        *self.wake_guest.lock().unwrap() = f;
    }

    /// Host -> Guest: writes bytes to `/dev/hvc0` RX queue and kicks the guest VCPU.
    pub fn host_write(&self, data: &[u8]) {
        {
            let mut q = self.to_guest.lock().unwrap();
            for &b in data {
                q.push_back(b);
            }
        }
        if let Some(cb) = self.wake_guest.lock().unwrap().as_ref() {
            cb();
        }
    }

    /// Host <- Guest: reads one `\n`-terminated line from `/dev/hvc0` if available.
    pub fn host_try_read_line(&self) -> Option<String> {
        let mut q = self.from_guest.lock().unwrap();
        let pos = q.iter().position(|&b| b == b'\n')?;
        let mut line = Vec::with_capacity(pos);
        for _ in 0..=pos {
            if let Some(b) = q.pop_front() {
                if b != b'\n' && b != b'\r' {
                    line.push(b);
                }
            }
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// Host: clears any stale buffered guest output before issuing a new exec command.
    pub fn host_clear_rx(&self) {
        self.from_guest.lock().unwrap().clear();
    }

    pub fn guest_has_rx(&self) -> bool {
        !self.to_guest.lock().unwrap().is_empty()
    }

    fn guest_pop_rx(&self, max_len: usize) -> Vec<u8> {
        let mut q = self.to_guest.lock().unwrap();
        let n = q.len().min(max_len);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(b) = q.pop_front() {
                out.push(b);
            }
        }
        self.rx_bytes.fetch_add(out.len() as u64, Ordering::Relaxed);
        out
    }

    fn guest_push_tx(&self, data: &[u8]) {
        self.tx_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        let mut q = self.from_guest.lock().unwrap();
        for &b in data {
            if q.len() >= 256 * 1024 {
                q.pop_front();
            }
            q.push_back(b);
        }
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

/// Virtio 1.0 MMIO v2 console device (`queue 0 = receiveq`, `queue 1 = transmitq`).
pub struct VirtioConsoleMmio {
    dev_features_sel: u32,
    drv_features_sel: u32,
    queue_sel: u32,
    queues: [VirtQueueState; 2],
    interrupt_status: u32,
    status: u32,
    channel: Arc<VirtioConsoleChannel>,
}

impl VirtioConsoleMmio {
    pub fn new(channel: Arc<VirtioConsoleChannel>) -> Self {
        Self {
            dev_features_sel: 0,
            drv_features_sel: 0,
            queue_sel: 0,
            queues: [VirtQueueState::default(), VirtQueueState::default()],
            interrupt_status: 0,
            status: 0,
            channel,
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.interrupt_status != 0
    }

    pub fn read(&self, offset: u64, _size: u8) -> u32 {
        match offset {
            0x000 => VIRTIO_MMIO_MAGIC,
            0x004 => VIRTIO_MMIO_VERSION,
            0x008 => VIRTIO_ID_CONSOLE,
            0x00C => VIRTIO_VENDOR_HIRSEL,
            0x010 => match self.dev_features_sel {
                0 => 0,
                1 => VIRTIO_F_VERSION_1_HI,
                _ => 0,
            },
            0x034 => {
                if (self.queue_sel as usize) < self.queues.len() {
                    QUEUE_NUM_MAX
                } else {
                    0
                }
            }
            0x044 => self
                .queues
                .get(self.queue_sel as usize)
                .map_or(0, |q| u32::from(q.ready)),
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
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.num = val.min(QUEUE_NUM_MAX);
                }
            }
            0x044 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.ready = val != 0;
                }
            }
            0x050 => {
                if val == 0 {
                    self.process_rx(ram);
                } else if val == 1 {
                    self.process_tx(ram);
                }
            }
            0x064 => self.interrupt_status &= !val,
            0x070 => {
                if val == 0 {
                    self.status = 0;
                    self.interrupt_status = 0;
                    self.queues[0] = VirtQueueState::default();
                    self.queues[1] = VirtQueueState::default();
                } else {
                    self.status = val;
                }
            }
            0x080 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.desc_addr = (q.desc_addr & 0xffff_ffff_0000_0000) | u64::from(val);
                }
            }
            0x084 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.desc_addr = (q.desc_addr & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
                }
            }
            0x090 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.driver_addr = (q.driver_addr & 0xffff_ffff_0000_0000) | u64::from(val);
                }
            }
            0x094 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.driver_addr =
                        (q.driver_addr & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
                }
            }
            0x0A0 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.device_addr = (q.device_addr & 0xffff_ffff_0000_0000) | u64::from(val);
                }
            }
            0x0A4 => {
                if let Some(q) = self.queues.get_mut(self.queue_sel as usize) {
                    q.device_addr =
                        (q.device_addr & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
                }
            }
            _ => {}
        }
    }

    pub fn process_rx(&mut self, ram: &mut [u8]) -> bool {
        let q = &mut self.queues[0];
        if !q.ready || q.num == 0 || !self.channel.guest_has_rx() {
            return false;
        }
        let qsize = q.num as u16;
        let Some(avail_idx) = read_u16(ram, q.driver_addr + 2) else {
            return false;
        };
        let mut delivered = false;
        while q.last_avail_idx != avail_idx && self.channel.guest_has_rx() {
            let slot = (q.last_avail_idx % qsize) as u64;
            let Some(head) = read_u16(ram, q.driver_addr + 4 + slot * 2) else {
                break;
            };
            q.last_avail_idx = q.last_avail_idx.wrapping_add(1);

            let mut written = 0usize;
            let mut idx = head;
            let mut hops = 0u32;
            while hops < q.num && self.channel.guest_has_rx() {
                hops += 1;
                let Some((addr, len, flags, next)) = read_desc(ram, q.desc_addr, idx) else {
                    break;
                };
                if (flags & VIRTQ_DESC_F_WRITE) != 0 {
                    let chunk = self.channel.guest_pop_rx(len as usize);
                    let off = addr as usize;
                    if off.saturating_add(chunk.len()) <= ram.len() {
                        ram[off..off + chunk.len()].copy_from_slice(&chunk);
                        written += chunk.len();
                    }
                }
                if (flags & VIRTQ_DESC_F_NEXT) == 0 {
                    break;
                }
                idx = next;
            }
            push_used(ram, q.device_addr, qsize, head, written as u32);
            delivered = true;
        }
        if delivered {
            self.interrupt_status |= 1;
        }
        delivered
    }

    fn process_tx(&mut self, ram: &mut [u8]) {
        let q = &mut self.queues[1];
        if !q.ready || q.num == 0 {
            return;
        }
        let qsize = q.num as u16;
        let Some(avail_idx) = read_u16(ram, q.driver_addr + 2) else {
            return;
        };
        let mut sent = false;
        while q.last_avail_idx != avail_idx {
            let slot = (q.last_avail_idx % qsize) as u64;
            let Some(head) = read_u16(ram, q.driver_addr + 4 + slot * 2) else {
                break;
            };
            q.last_avail_idx = q.last_avail_idx.wrapping_add(1);

            let mut idx = head;
            let mut hops = 0u32;
            while hops < q.num {
                hops += 1;
                let Some((addr, len, flags, next)) = read_desc(ram, q.desc_addr, idx) else {
                    break;
                };
                if (flags & VIRTQ_DESC_F_WRITE) == 0 {
                    let off = addr as usize;
                    let n = len as usize;
                    if off.saturating_add(n) <= ram.len() {
                        self.channel.guest_push_tx(&ram[off..off + n]);
                    }
                }
                if (flags & VIRTQ_DESC_F_NEXT) == 0 {
                    break;
                }
                idx = next;
            }
            push_used(ram, q.device_addr, qsize, head, 0);
            sent = true;
        }
        if sent {
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
    fn virtio_console_mmio_rx_tx_roundtrip() {
        let ch = VirtioConsoleChannel::new();
        let mut dev = VirtioConsoleMmio::new(Arc::clone(&ch));
        let mut ram = vec![0u8; 64 * 1024];

        assert_eq!(dev.read(0x008, 4), VIRTIO_ID_CONSOLE);
        // Setup RX queue 0
        dev.write(0x030, 0, &mut ram);
        dev.write(0x038, 16, &mut ram);
        dev.write(0x080, 0x1000, &mut ram);
        dev.write(0x090, 0x2000, &mut ram);
        dev.write(0x0A0, 0x3000, &mut ram);
        dev.write(0x044, 1, &mut ram);

        ram[0x1000..0x1008].copy_from_slice(&0x4000u64.to_le_bytes());
        ram[0x1008..0x100C].copy_from_slice(&64u32.to_le_bytes());
        ram[0x100C..0x100E].copy_from_slice(&VIRTQ_DESC_F_WRITE.to_le_bytes());
        write_u16(&mut ram, 0x2004, 0);
        write_u16(&mut ram, 0x2002, 1);

        ch.host_write(b"HREXEC dW5hbWU=\n");
        assert!(dev.process_rx(&mut ram));
        assert_eq!(&ram[0x4000..0x4010], b"HREXEC dW5hbWU=\n");

        // Setup TX queue 1
        dev.write(0x030, 1, &mut ram);
        dev.write(0x038, 16, &mut ram);
        dev.write(0x080, 0x5000, &mut ram);
        dev.write(0x090, 0x6000, &mut ram);
        dev.write(0x0A0, 0x7000, &mut ram);
        dev.write(0x044, 1, &mut ram);
        ram[0x8000..0x800C].copy_from_slice(b"HRDONE 0 ok\n");
        ram[0x5000..0x5008].copy_from_slice(&0x8000u64.to_le_bytes());
        ram[0x5008..0x500C].copy_from_slice(&12u32.to_le_bytes());
        ram[0x500C..0x500E].copy_from_slice(&0u16.to_le_bytes());
        write_u16(&mut ram, 0x6004, 0);
        write_u16(&mut ram, 0x6002, 1);
        dev.write(0x050, 1, &mut ram);
        assert_eq!(ch.host_try_read_line().as_deref(), Some("HRDONE 0 ok"));
    }
}
