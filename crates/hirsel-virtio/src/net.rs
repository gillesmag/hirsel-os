//! Ring-3 Virtio 1.0 Network Device driver (`virtio-net-pci`) + `smoltcp::phy::Device`.

#![cfg(target_os = "fuchsia")]

use crate::dma::{Bti, DmaBuf, ZirconIommu};
use crate::pci::{
    PciBus, VIRTIO_DEV_NET_MODERN, VIRTIO_DEV_NET_TRANSITIONAL, VIRTIO_VENDOR_ID,
    VirtioPciTransport,
};
use crate::queue::SplitVirtqueue;
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use zx_sys::*;

const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_HDR_LEN: usize = 12; // Virtio 1.0 always uses 12-byte virtio_net_hdr_v1.
const SLOT_BYTES: usize = 2048;
const RX_SLOTS: usize = 32;
const TX_SLOTS: usize = 16;

pub struct VirtioNet {
    transport: VirtioPciTransport,
    _bti: Bti,
    rx_q: SplitVirtqueue,
    tx_q: SplitVirtqueue,
    rx_buf: DmaBuf, // RX_SLOTS * SLOT_BYTES
    tx_buf: DmaBuf, // TX_SLOTS * SLOT_BYTES
    rx_desc_to_slot: [u16; 128],
    tx_in_use: [bool; TX_SLOTS],
    tx_desc_to_slot: [u16; 128],
    pub mac: [u8; 6],
}

impl VirtioNet {
    pub fn probe(
        bus: &PciBus,
        iommu: &ZirconIommu,
        mmio_resource: zx_handle_t,
    ) -> Result<Option<Self>, String> {
        let Some((addr, did)) = bus.find_device(
            VIRTIO_VENDOR_ID,
            &[VIRTIO_DEV_NET_TRANSITIONAL, VIRTIO_DEV_NET_MODERN],
        ) else {
            return Ok(None);
        };

        let transport = VirtioPciTransport::open(bus, addr, did, mmio_resource)?;
        let (feat_lo, _) = transport.negotiate_features(VIRTIO_NET_F_MAC, 0)?;

        let mut mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        if (feat_lo & VIRTIO_NET_F_MAC) != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = transport.device_cfg().read_u8(i);
            }
        }

        let bti = iommu.create_bti(addr.bti_id())?;
        let rx_q = transport.setup_queue(0, 64, &bti)?;
        let tx_q = transport.setup_queue(1, 64, &bti)?;
        let rx_buf = DmaBuf::alloc(&bti, RX_SLOTS * SLOT_BYTES)?;
        let tx_buf = DmaBuf::alloc(&bti, TX_SLOTS * SLOT_BYTES)?;

        transport.set_driver_ok();

        let mut dev = Self {
            transport,
            _bti: bti,
            rx_q,
            tx_q,
            rx_buf,
            tx_buf,
            rx_desc_to_slot: [0; 128],
            tx_in_use: [false; TX_SLOTS],
            tx_desc_to_slot: [0; 128],
            mac,
        };

        // Pre-post all RX slots so the NIC immediately delivers incoming Ethernet frames.
        for slot in 0..RX_SLOTS {
            dev.post_rx_slot(slot);
        }
        dev.transport.notify_queue(0, dev.rx_q.notify_off);

        Ok(Some(dev))
    }

    fn post_rx_slot(&mut self, slot: usize) {
        let paddr = self.rx_buf.paddr + (slot * SLOT_BYTES) as u64;
        if let Some(head) = self.rx_q.submit_chain(&[(paddr, SLOT_BYTES as u32, true)]) {
            self.rx_desc_to_slot[head as usize] = slot as u16;
        }
    }

    fn reclaim_tx(&mut self) {
        while let Some((done_id, _)) = self.tx_q.pop_used() {
            let slot = self.tx_desc_to_slot[done_id as usize] as usize;
            self.tx_in_use[slot] = false;
            self.tx_q.free_chain(done_id);
        }
    }

    fn alloc_tx_slot(&mut self) -> Option<usize> {
        self.reclaim_tx();
        for (i, busy) in self.tx_in_use.iter_mut().enumerate() {
            if !*busy {
                *busy = true;
                return Some(i);
            }
        }
        None
    }

    fn send_tx_slot(&mut self, slot: usize, frame_len: usize) {
        let total = VIRTIO_NET_HDR_LEN + frame_len;
        let paddr = self.tx_buf.paddr + (slot * SLOT_BYTES) as u64;
        if let Some(head) = self.tx_q.submit_chain(&[(paddr, total as u32, false)]) {
            self.tx_desc_to_slot[head as usize] = slot as u16;
            self.transport.notify_queue(1, self.tx_q.notify_off);
        } else {
            self.tx_in_use[slot] = false;
        }
    }
}

pub struct VirtioRxToken<'a> {
    dev: &'a mut VirtioNet,
    slot: usize,
    frame_len: usize,
    consumed: bool,
}

impl<'a> phy::RxToken for VirtioRxToken<'a> {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        self.consumed = true;
        let start = self.slot * SLOT_BYTES + VIRTIO_NET_HDR_LEN;
        let result = f(&self.dev.rx_buf.as_slice()[start..start + self.frame_len]);
        self.dev.post_rx_slot(self.slot);
        self.dev.transport.notify_queue(0, self.dev.rx_q.notify_off);
        result
    }
}

impl Drop for VirtioRxToken<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            self.dev.post_rx_slot(self.slot);
            self.dev.transport.notify_queue(0, self.dev.rx_q.notify_off);
        }
    }
}

pub struct VirtioTxToken<'a> {
    dev: &'a mut VirtioNet,
    slot: usize,
    consumed: bool,
}

impl<'a> phy::TxToken for VirtioTxToken<'a> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.consumed = true;
        let base = self.slot * SLOT_BYTES;
        let buf = self.dev.tx_buf.as_mut_slice();
        // Zero 12-byte Virtio 1.0 net header (no checksum offload / GSO).
        buf[base..base + VIRTIO_NET_HDR_LEN].fill(0);
        let result = f(&mut buf[base + VIRTIO_NET_HDR_LEN..base + VIRTIO_NET_HDR_LEN + len]);
        self.dev.send_tx_slot(self.slot, len);
        result
    }
}

impl Drop for VirtioTxToken<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            self.dev.tx_in_use[self.slot] = false;
        }
    }
}

impl Device for VirtioNet {
    type RxToken<'a>
        = VirtioRxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = VirtioTxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let tx_slot = self.alloc_tx_slot()?;
        while let Some((done_id, used_len)) = self.rx_q.pop_used() {
            let rx_slot = self.rx_desc_to_slot[done_id as usize] as usize;
            self.rx_q.free_chain(done_id);
            let total = (used_len as usize).min(SLOT_BYTES);
            if total > VIRTIO_NET_HDR_LEN {
                let frame_len = total - VIRTIO_NET_HDR_LEN;
                // Safety: RxToken reads rx_buf and TxToken writes tx_buf (disjoint fields);
                // smoltcp consumes RxToken before TxToken, so split-borrow via raw pointer is sound
                // because RxToken::consume and TxToken::consume run sequentially in smoltcp.
                let dev_ptr = self as *mut VirtioNet;
                return Some((
                    VirtioRxToken {
                        dev: unsafe { &mut *dev_ptr },
                        slot: rx_slot,
                        frame_len,
                        consumed: false,
                    },
                    VirtioTxToken {
                        dev: unsafe { &mut *dev_ptr },
                        slot: tx_slot,
                        consumed: false,
                    },
                ));
            }
            // Empty/runt frame: immediately re-post RX slot.
            self.post_rx_slot(rx_slot);
            self.transport.notify_queue(0, self.rx_q.notify_off);
        }
        self.tx_in_use[tx_slot] = false;
        None
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        let slot = self.alloc_tx_slot()?;
        Some(VirtioTxToken {
            dev: self,
            slot,
            consumed: false,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(16);
        caps
    }
}
