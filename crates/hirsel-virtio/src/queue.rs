//! Virtio 1.0 Split Virtqueue layout & ring operations.

#[cfg(any(target_os = "fuchsia", test))]
use core::mem::size_of;

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

pub const RING_DESC_OFF: usize = 0;
pub const RING_AVAIL_OFF: usize = 2048;
pub const RING_USED_OFF: usize = 4096;
pub const RING_TOTAL_BYTES: usize = 8192;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

#[cfg(target_os = "fuchsia")]
mod fuchsia_vq {
    use super::*;
    use crate::dma::{Bti, DmaBuf};
    use core::ptr;
    use core::sync::atomic::{Ordering, fence};

    /// A Virtio 1.0 Split Virtqueue backed by a single contiguous pinned `DmaBuf`.
    pub struct SplitVirtqueue {
        pub size: u16,
        pub notify_off: u16,
        buf: DmaBuf,
        free_head: u16,
        num_free: u16,
        avail_idx: u16,
        last_used_idx: u16,
    }

    impl SplitVirtqueue {
        pub fn new(bti: &Bti, size: u16, notify_off: u16) -> Result<Self, String> {
            if size == 0 || size > 128 || !size.is_power_of_two() {
                return Err(format!("invalid virtqueue size {size}"));
            }
            let buf = DmaBuf::alloc(bti, RING_TOTAL_BYTES)?;
            let mut vq = Self {
                size,
                notify_off,
                buf,
                free_head: 0,
                num_free: size,
                avail_idx: 0,
                last_used_idx: 0,
            };
            // Initialize descriptor free list: 0 -> 1 -> ... -> size-1.
            for i in 0..size {
                let d = VirtqDesc {
                    addr: 0,
                    len: 0,
                    flags: 0,
                    next: if i + 1 < size { i + 1 } else { u16::MAX },
                };
                vq.write_desc(i, d);
            }
            Ok(vq)
        }

        pub fn desc_paddr(&self) -> u64 {
            self.buf.paddr + RING_DESC_OFF as u64
        }

        pub fn avail_paddr(&self) -> u64 {
            self.buf.paddr + RING_AVAIL_OFF as u64
        }

        pub fn used_paddr(&self) -> u64 {
            self.buf.paddr + RING_USED_OFF as u64
        }

        fn desc_ptr(&self, idx: u16) -> *mut VirtqDesc {
            unsafe {
                self.buf
                    .vaddr
                    .add(RING_DESC_OFF + (idx as usize) * size_of::<VirtqDesc>())
                    .cast::<VirtqDesc>()
            }
        }

        fn write_desc(&mut self, idx: u16, desc: VirtqDesc) {
            unsafe { ptr::write_volatile(self.desc_ptr(idx), desc) };
        }

        fn read_desc(&self, idx: u16) -> VirtqDesc {
            unsafe { ptr::read_volatile(self.desc_ptr(idx)) }
        }

        fn alloc_desc(&mut self) -> Option<u16> {
            if self.num_free == 0 {
                return None;
            }
            let idx = self.free_head;
            self.free_head = self.read_desc(idx).next;
            self.num_free -= 1;
            Some(idx)
        }

        pub fn free_chain(&mut self, mut head: u16) {
            loop {
                let d = self.read_desc(head);
                let has_next = (d.flags & VIRTQ_DESC_F_NEXT) != 0;
                let next = d.next;
                self.write_desc(
                    head,
                    VirtqDesc {
                        addr: 0,
                        len: 0,
                        flags: 0,
                        next: self.free_head,
                    },
                );
                self.free_head = head;
                self.num_free += 1;
                if has_next {
                    head = next;
                } else {
                    break;
                }
            }
        }

        /// Enqueues a descriptor chain `(paddr, len, device_writable)` into the avail ring
        /// and returns the head descriptor index.
        pub fn submit_chain(&mut self, segs: &[(u64, u32, bool)]) -> Option<u16> {
            if segs.is_empty() || (self.num_free as usize) < segs.len() {
                return None;
            }
            let mut indices = [0u16; 8];
            if segs.len() > indices.len() {
                return None;
            }
            for (i, slot) in indices.iter_mut().take(segs.len()).enumerate() {
                *slot = self.alloc_desc().unwrap();
                let _ = i;
            }
            for (i, &(addr, len, writable)) in segs.iter().enumerate() {
                let mut flags = if writable { VIRTQ_DESC_F_WRITE } else { 0 };
                let next = if i + 1 < segs.len() {
                    flags |= VIRTQ_DESC_F_NEXT;
                    indices[i + 1]
                } else {
                    0
                };
                self.write_desc(
                    indices[i],
                    VirtqDesc {
                        addr,
                        len,
                        flags,
                        next,
                    },
                );
            }
            let head = indices[0];
            let slot = (self.avail_idx % self.size) as usize;
            unsafe {
                // avail.ring[slot] = head (ring starts at avail_off + 4)
                let ring_ptr = self.buf.vaddr.add(RING_AVAIL_OFF + 4 + slot * 2).cast::<u16>();
                ptr::write_volatile(ring_ptr, head);
                fence(Ordering::SeqCst);
                self.avail_idx = self.avail_idx.wrapping_add(1);
                let idx_ptr = self.buf.vaddr.add(RING_AVAIL_OFF + 2).cast::<u16>();
                ptr::write_volatile(idx_ptr, self.avail_idx);
                fence(Ordering::SeqCst);
            }
            Some(head)
        }

        /// Pops the next completed descriptor `(head_id, bytes_written)` from the used ring.
        pub fn pop_used(&mut self) -> Option<(u16, u32)> {
            fence(Ordering::SeqCst);
            let used_idx =
                unsafe { ptr::read_volatile(self.buf.vaddr.add(RING_USED_OFF + 2).cast::<u16>()) };
            if used_idx == self.last_used_idx {
                return None;
            }
            let slot = (self.last_used_idx % self.size) as usize;
            let elem = unsafe {
                ptr::read_volatile(
                    self.buf
                        .vaddr
                        .add(RING_USED_OFF + 4 + slot * size_of::<VirtqUsedElem>())
                        .cast::<VirtqUsedElem>(),
                )
            };
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            Some((elem.id as u16, elem.len))
        }
    }
}

#[cfg(target_os = "fuchsia")]
pub use fuchsia_vq::SplitVirtqueue;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtq_struct_sizes() {
        assert_eq!(size_of::<VirtqDesc>(), 16);
        assert_eq!(size_of::<VirtqUsedElem>(), 8);
    }
}
