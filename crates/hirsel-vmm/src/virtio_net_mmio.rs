//! Modern Virtio 1.0 MMIO (v2) Network Device (`virtio-net`) for Tier 2 Linux MicroVMs.
//!
//! Exposed to Linux via `virtio_mmio.device=4K@0xd0000000:5`, providing a full L2
//! Ethernet interface (`eth0`) backed by an in-memory host/guest packet channel so
//! `hirsel-netd` (`smoltcp`) can bridge real TCP/SSH traffic into the guest.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Guest physical address of the Virtio-MMIO `virtio-net` register page (4 KiB).
pub const GPA_VIRTIO_NET_MMIO: u64 = 0xD000_0000;
/// ISA/8259A IRQ line wired to `virtio-net` MMIO.
pub const VIRTIO_NET_IRQ: u8 = 5;

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976; // "virt"
const VIRTIO_MMIO_VERSION: u32 = 2; // Virtio 1.0 modern MMIO
const VIRTIO_ID_NET: u32 = 1;
const VIRTIO_VENDOR_HIRSEL: u32 = 0x4852_534C; // "HRSL"

const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;
const VIRTIO_F_VERSION_1_HI: u32 = 1 << 0; // bit 32

const VIRTIO_NET_S_LINK_UP: u16 = 1;
const VIRTIO_NET_HDR_LEN: usize = 12;
const QUEUE_NUM_MAX: u32 = 128;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const MAX_QUEUED_FRAMES: usize = 256;

/// Thread-safe bidirectional L2 Ethernet channel between a running Linux MicroVM's
/// `virtio-net` device and Hirsel-OS's `smoltcp` virtual gateway.
pub struct VirtioNetChannel {
    /// Ethernet frames transmitted by the Linux guest (`eth0` TX -> host gateway RX).
    tx_from_guest: Mutex<VecDeque<Vec<u8>>>,
    /// Ethernet frames queued for delivery into the Linux guest (host gateway TX -> `eth0` RX).
    rx_to_guest: Mutex<VecDeque<Vec<u8>>>,
    /// Optional wakeup callback invoked whenever the host enqueues a frame for the guest.
    wake_guest: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    pub tx_packets: AtomicU64,
    pub rx_packets: AtomicU64,
}

impl VirtioNetChannel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tx_from_guest: Mutex::new(VecDeque::with_capacity(64)),
            rx_to_guest: Mutex::new(VecDeque::with_capacity(64)),
            wake_guest: Mutex::new(None),
            tx_packets: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
        })
    }

    /// Registers a wakeup callback (e.g. `zx_vcpu_kick`) invoked when host packets arrive.
    pub fn set_wake_guest(&self, f: Option<Box<dyn Fn() + Send + Sync>>) {
        *self.wake_guest.lock().unwrap() = f;
    }

    /// Host side: pops the next Ethernet frame sent by the guest, if any.
    pub fn host_pop_rx(&self) -> Option<Vec<u8>> {
        self.tx_from_guest.lock().unwrap().pop_front()
    }

    /// Host side: enqueues an Ethernet frame to be delivered to the guest's `eth0`.
    pub fn host_push_tx(&self, frame: Vec<u8>) {
        {
            let mut q = self.rx_to_guest.lock().unwrap();
            if q.len() >= MAX_QUEUED_FRAMES {
                q.pop_front();
            }
            q.push_back(frame);
        }
        if let Some(cb) = self.wake_guest.lock().unwrap().as_ref() {
            cb();
        }
    }

    /// Guest side: returns true if the host has queued at least one frame for guest RX.
    pub fn guest_has_rx(&self) -> bool {
        !self.rx_to_guest.lock().unwrap().is_empty()
    }

    fn guest_push_tx(&self, frame: Vec<u8>) {
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
        let mut q = self.tx_from_guest.lock().unwrap();
        if q.len() >= MAX_QUEUED_FRAMES {
            q.pop_front();
        }
        q.push_back(frame);
    }

    fn guest_pop_rx(&self) -> Option<Vec<u8>> {
        let frame = self.rx_to_guest.lock().unwrap().pop_front()?;
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        Some(frame)
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

impl VirtQueueState {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Virtio 1.0 MMIO v2 network device state owned by the VCPU loop.
pub struct VirtioNetMmio {
    mac: [u8; 6],
    dev_features_sel: u32,
    drv_features_sel: u32,
    drv_features_lo: u32,
    drv_features_hi: u32,
    queue_sel: u32,
    queues: [VirtQueueState; 2], // 0 = RX, 1 = TX
    interrupt_status: u32,
    status: u32,
    channel: Arc<VirtioNetChannel>,
}

impl VirtioNetMmio {
    pub fn new(mac: [u8; 6], channel: Arc<VirtioNetChannel>) -> Self {
        Self {
            mac,
            dev_features_sel: 0,
            drv_features_sel: 0,
            drv_features_lo: 0,
            drv_features_hi: 0,
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

    /// Reads `size` bytes (1, 2, or 4) from MMIO register `offset`.
    pub fn read(&self, offset: u64, size: u8) -> u32 {
        // Config space [0x100 .. 0x108): 6-byte MAC + 2-byte link status.
        if (0x100..0x108).contains(&offset) {
            let mut cfg = [0u8; 8];
            cfg[0..6].copy_from_slice(&self.mac);
            cfg[6..8].copy_from_slice(&VIRTIO_NET_S_LINK_UP.to_le_bytes());
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
            0x008 => VIRTIO_ID_NET,
            0x00C => VIRTIO_VENDOR_HIRSEL,
            0x010 => match self.dev_features_sel {
                0 => VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS,
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
            0x0FC => 0, // ConfigGeneration
            _ => 0,
        }
    }

    /// Writes `val` to MMIO register `offset`, processing virtqueues against `ram`.
    pub fn write(&mut self, offset: u64, val: u32, ram: &mut [u8]) {
        match offset {
            0x014 => self.dev_features_sel = val,
            0x020 => {
                if self.drv_features_sel == 0 {
                    self.drv_features_lo = val;
                } else if self.drv_features_sel == 1 {
                    self.drv_features_hi = val;
                }
            }
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
                // QueueNotify
                if val == 0 {
                    self.process_rx(ram);
                } else if val == 1 {
                    self.process_tx(ram);
                }
            }
            0x064 => {
                // InterruptACK
                self.interrupt_status &= !val;
            }
            0x070 => {
                if val == 0 {
                    self.status = 0;
                    self.interrupt_status = 0;
                    self.queues[0].reset();
                    self.queues[1].reset();
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

    /// Delivers any queued host->guest Ethernet frames into the guest's Virtio RX queue.
    /// Returns `true` if at least one frame was delivered and an interrupt should fire.
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
        while q.last_avail_idx != avail_idx {
            let Some(frame) = self.channel.guest_pop_rx() else {
                break;
            };
            let ring_slot = (q.last_avail_idx % qsize) as u64;
            let Some(head) = read_u16(ram, q.driver_addr + 4 + ring_slot * 2) else {
                break;
            };
            q.last_avail_idx = q.last_avail_idx.wrapping_add(1);

            // Build [12-byte zeroed virtio_net_hdr_v1 | Ethernet frame].
            let mut packet = Vec::with_capacity(VIRTIO_NET_HDR_LEN + frame.len());
            packet.extend_from_slice(&[0u8; VIRTIO_NET_HDR_LEN]);
            packet.extend_from_slice(&frame);

            let mut written = 0usize;
            let mut desc_idx = head;
            let mut hops = 0u32;
            while written < packet.len() && hops < q.num {
                hops += 1;
                let Some((addr, len, flags, next)) = read_desc(ram, q.desc_addr, desc_idx) else {
                    break;
                };
                if (flags & VIRTQ_DESC_F_WRITE) != 0 {
                    let take = (len as usize).min(packet.len() - written);
                    let dst_off = addr as usize;
                    if dst_off.saturating_add(take) <= ram.len() {
                        ram[dst_off..dst_off + take]
                            .copy_from_slice(&packet[written..written + take]);
                        written += take;
                    }
                }
                if (flags & VIRTQ_DESC_F_NEXT) == 0 {
                    break;
                }
                desc_idx = next;
            }

            push_used(ram, q.device_addr, qsize, head, written as u32);
            delivered = true;
        }

        if delivered {
            self.interrupt_status |= 1;
        }
        delivered
    }

    /// Drains all available TX chains submitted by the Linux guest and pushes
    /// their Ethernet frames onto the host channel.
    pub fn process_tx(&mut self, ram: &mut [u8]) -> bool {
        let q = &mut self.queues[1];
        if !q.ready || q.num == 0 {
            return false;
        }
        let qsize = q.num as u16;
        let Some(avail_idx) = read_u16(ram, q.driver_addr + 2) else {
            return false;
        };

        let mut sent_any = false;
        while q.last_avail_idx != avail_idx {
            let ring_slot = (q.last_avail_idx % qsize) as u64;
            let Some(head) = read_u16(ram, q.driver_addr + 4 + ring_slot * 2) else {
                break;
            };
            q.last_avail_idx = q.last_avail_idx.wrapping_add(1);

            let mut raw = Vec::with_capacity(1600);
            let mut desc_idx = head;
            let mut hops = 0u32;
            while hops < q.num {
                hops += 1;
                let Some((addr, len, flags, next)) = read_desc(ram, q.desc_addr, desc_idx) else {
                    break;
                };
                if (flags & VIRTQ_DESC_F_WRITE) == 0 {
                    let src_off = addr as usize;
                    let n = len as usize;
                    if src_off.saturating_add(n) <= ram.len() {
                        raw.extend_from_slice(&ram[src_off..src_off + n]);
                    }
                }
                if (flags & VIRTQ_DESC_F_NEXT) == 0 {
                    break;
                }
                desc_idx = next;
            }

            if raw.len() > VIRTIO_NET_HDR_LEN {
                self.channel
                    .guest_push_tx(raw[VIRTIO_NET_HDR_LEN..].to_vec());
            }

            push_used(ram, q.device_addr, qsize, head, 0);
            sent_any = true;
        }

        if sent_any {
            self.interrupt_status |= 1;
        }
        sent_any
    }
}

fn read_u16(ram: &[u8], gpa: u64) -> Option<u16> {
    let off = usize::try_from(gpa).ok()?;
    let s = ram.get(off..off + 2)?;
    Some(u16::from_le_bytes(s.try_into().unwrap()))
}

fn read_u32(ram: &[u8], gpa: u64) -> Option<u32> {
    let off = usize::try_from(gpa).ok()?;
    let s = ram.get(off..off + 4)?;
    Some(u32::from_le_bytes(s.try_into().unwrap()))
}

fn read_u64(ram: &[u8], gpa: u64) -> Option<u64> {
    let off = usize::try_from(gpa).ok()?;
    let s = ram.get(off..off + 8)?;
    Some(u64::from_le_bytes(s.try_into().unwrap()))
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
    let addr = read_u64(ram, off)?;
    let len = read_u32(ram, off + 8)?;
    let flags = read_u16(ram, off + 12)?;
    let next = read_u16(ram, off + 14)?;
    Some((addr, len, flags, next))
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
    fn virtio_net_mmio_registers_and_ring_roundtrip() {
        let mac = [0x02, 0x48, 0x52, 0x53, 0x4c, 0x01];
        let ch = VirtioNetChannel::new();
        let mut dev = VirtioNetMmio::new(mac, Arc::clone(&ch));
        let mut ram = vec![0u8; 64 * 1024];

        assert_eq!(dev.read(0x000, 4), VIRTIO_MMIO_MAGIC);
        assert_eq!(dev.read(0x004, 4), 2);
        assert_eq!(dev.read(0x008, 4), 1);
        for (i, &b) in mac.iter().enumerate() {
            assert_eq!(dev.read(0x100 + i as u64, 1) as u8, b);
        }
        assert_eq!(dev.read(0x106, 2) as u16, VIRTIO_NET_S_LINK_UP);

        // Configure RX queue (0) at 0x1000 / 0x2000 / 0x3000.
        dev.write(0x030, 0, &mut ram);
        dev.write(0x038, 16, &mut ram);
        dev.write(0x080, 0x1000, &mut ram);
        dev.write(0x090, 0x2000, &mut ram);
        dev.write(0x0A0, 0x3000, &mut ram);
        dev.write(0x044, 1, &mut ram);

        // Post one writable RX descriptor pointing to 0x4000 (len 256).
        ram[0x1000..0x1008].copy_from_slice(&0x4000u64.to_le_bytes());
        ram[0x1008..0x100C].copy_from_slice(&256u32.to_le_bytes());
        ram[0x100C..0x100E].copy_from_slice(&VIRTQ_DESC_F_WRITE.to_le_bytes());
        write_u16(&mut ram, 0x2000 + 4, 0);
        write_u16(&mut ram, 0x2000 + 2, 1);

        // Host sends an Ethernet frame -> guest receives it with 12-byte virtio_net_hdr_v1.
        ch.host_push_tx(b"HELLO-ETH-FRAME".to_vec());
        assert!(dev.process_rx(&mut ram));
        assert!(dev.irq_pending());
        assert_eq!(read_u16(&ram, 0x3000 + 2), Some(1));
        assert_eq!(
            &ram[0x4000 + VIRTIO_NET_HDR_LEN..0x4000 + VIRTIO_NET_HDR_LEN + 15],
            b"HELLO-ETH-FRAME"
        );

        // Configure TX queue (1) at 0x5000 / 0x6000 / 0x7000 and transmit a frame.
        dev.write(0x064, 1, &mut ram);
        assert!(!dev.irq_pending());
        dev.write(0x030, 1, &mut ram);
        dev.write(0x038, 16, &mut ram);
        dev.write(0x080, 0x5000, &mut ram);
        dev.write(0x090, 0x6000, &mut ram);
        dev.write(0x0A0, 0x7000, &mut ram);
        dev.write(0x044, 1, &mut ram);

        let tx_buf = 0x8000usize;
        ram[tx_buf..tx_buf + VIRTIO_NET_HDR_LEN].fill(0);
        ram[tx_buf + VIRTIO_NET_HDR_LEN..tx_buf + VIRTIO_NET_HDR_LEN + 10]
            .copy_from_slice(b"GUEST-TX!!");
        ram[0x5000..0x5008].copy_from_slice(&(tx_buf as u64).to_le_bytes());
        ram[0x5008..0x500C].copy_from_slice(&((VIRTIO_NET_HDR_LEN + 10) as u32).to_le_bytes());
        ram[0x500C..0x500E].copy_from_slice(&0u16.to_le_bytes());
        write_u16(&mut ram, 0x6000 + 4, 0);
        write_u16(&mut ram, 0x6000 + 2, 1);

        dev.write(0x050, 1, &mut ram);
        assert_eq!(ch.host_pop_rx().as_deref(), Some(b"GUEST-TX!!".as_slice()));
    }
}
