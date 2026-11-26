//! Ring-3 Virtio 1.0 Block Device driver (`virtio-blk-pci`) on Zircon.

#![cfg(target_os = "fuchsia")]

use crate::BlockDevice;
use crate::dma::{Bti, DmaBuf, ZirconIommu};
use crate::pci::{
    PciBus, VIRTIO_DEV_BLK_MODERN, VIRTIO_DEV_BLK_TRANSITIONAL, VIRTIO_VENDOR_ID,
    VirtioPciTransport,
};
use crate::queue::SplitVirtqueue;
use core::mem::size_of;
use core::ptr;
use zx_sys::*;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9;

const SECTOR_SIZE: usize = 512;
const IO_CHUNK_BYTES: usize = 64 * 1024; // 128 sectors per DMA transfer

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioBlkReqHdr {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

pub struct VirtioBlk {
    transport: VirtioPciTransport,
    _bti: Bti,
    queue: SplitVirtqueue,
    req_buf: DmaBuf,  // [0..16] header, [16] status byte
    data_buf: DmaBuf, // 64 KiB DMA bounce buffer
    capacity: u64,
    supports_flush: bool,
}

impl VirtioBlk {
    pub fn probe(
        bus: &PciBus,
        iommu: &ZirconIommu,
        mmio_resource: zx_handle_t,
    ) -> Result<Option<Self>, String> {
        let Some((addr, did)) = bus.find_device(
            VIRTIO_VENDOR_ID,
            &[VIRTIO_DEV_BLK_TRANSITIONAL, VIRTIO_DEV_BLK_MODERN],
        ) else {
            return Ok(None);
        };

        let transport = VirtioPciTransport::open(bus, addr, did, mmio_resource)?;
        let (feat_lo, _) = transport.negotiate_features(VIRTIO_BLK_F_FLUSH, 0)?;
        let supports_flush = (feat_lo & VIRTIO_BLK_F_FLUSH) != 0;

        let cap_lo = transport.device_cfg().read_u32(0) as u64;
        let cap_hi = transport.device_cfg().read_u32(4) as u64;
        let capacity = cap_lo | (cap_hi << 32);

        let bti = iommu.create_bti(addr.bti_id())?;
        let queue = transport.setup_queue(0, 64, &bti)?;
        let req_buf = DmaBuf::alloc(&bti, 4096)?;
        let data_buf = DmaBuf::alloc(&bti, IO_CHUNK_BYTES)?;

        transport.set_driver_ok();

        Ok(Some(Self {
            transport,
            _bti: bti,
            queue,
            req_buf,
            data_buf,
            capacity,
            supports_flush,
        }))
    }

    fn execute_req(&mut self, req_type: u32, sector: u64, data_len: usize) -> Result<(), String> {
        let hdr = VirtioBlkReqHdr {
            req_type,
            reserved: 0,
            sector,
        };
        unsafe {
            ptr::write_volatile(self.req_buf.vaddr.cast::<VirtioBlkReqHdr>(), hdr);
            ptr::write_volatile(self.req_buf.vaddr.add(16), 0xFFu8);
        }

        let hdr_paddr = self.req_buf.paddr;
        let status_paddr = self.req_buf.paddr + 16;

        let head = if data_len > 0 {
            let dev_writable = req_type == VIRTIO_BLK_T_IN;
            self.queue.submit_chain(&[
                (hdr_paddr, size_of::<VirtioBlkReqHdr>() as u32, false),
                (self.data_buf.paddr, data_len as u32, dev_writable),
                (status_paddr, 1, true),
            ])
        } else {
            self.queue.submit_chain(&[
                (hdr_paddr, size_of::<VirtioBlkReqHdr>() as u32, false),
                (status_paddr, 1, true),
            ])
        }
        .ok_or_else(|| "virtio-blk queue full".to_string())?;

        self.transport.notify_queue(0, self.queue.notify_off);

        // Poll used ring until completion.
        let mut spins: u32 = 0;
        loop {
            if let Some((done_id, _)) = self.queue.pop_used() {
                self.queue.free_chain(done_id);
                if done_id != head {
                    return Err(format!("virtio-blk unexpected desc id {done_id} != {head}"));
                }
                let status = unsafe { ptr::read_volatile(self.req_buf.vaddr.add(16)) };
                if status != 0 {
                    return Err(format!("virtio-blk I/O error status={status}"));
                }
                return Ok(());
            }
            spins = spins.wrapping_add(1);
            if spins > 50_000 {
                let next = unsafe { zx_clock_get_monotonic() }.saturating_add(50_000);
                unsafe { zx_nanosleep(next) };
                spins = 0;
            } else {
                core::hint::spin_loop();
            }
        }
    }
}

impl BlockDevice for VirtioBlk {
    fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    fn read_sectors(&mut self, mut sector: u64, mut dst: &mut [u8]) -> Result<(), String> {
        if dst.len() % SECTOR_SIZE != 0 {
            return Err("read length must be a multiple of 512".into());
        }
        while !dst.is_empty() {
            let chunk = dst.len().min(IO_CHUNK_BYTES);
            self.execute_req(VIRTIO_BLK_T_IN, sector, chunk)?;
            dst[..chunk].copy_from_slice(&self.data_buf.as_slice()[..chunk]);
            sector += (chunk / SECTOR_SIZE) as u64;
            dst = &mut dst[chunk..];
        }
        Ok(())
    }

    fn write_sectors(&mut self, mut sector: u64, mut src: &[u8]) -> Result<(), String> {
        if src.len() % SECTOR_SIZE != 0 {
            return Err("write length must be a multiple of 512".into());
        }
        while !src.is_empty() {
            let chunk = src.len().min(IO_CHUNK_BYTES);
            self.data_buf.as_mut_slice()[..chunk].copy_from_slice(&src[..chunk]);
            self.execute_req(VIRTIO_BLK_T_OUT, sector, chunk)?;
            sector += (chunk / SECTOR_SIZE) as u64;
            src = &src[chunk..];
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.supports_flush {
            self.execute_req(VIRTIO_BLK_T_FLUSH, 0, 0)?;
        }
        Ok(())
    }
}
