//! Zircon Ring-3 DMA & Physical MMIO abstractions (`IOMMU`, `BTI`, `DmaBuf`, `MmioRegion`).

#![cfg(target_os = "fuchsia")]

use core::ptr;
use zx_sys::*;

const PAGE_SIZE: usize = 4096;

fn page_up(n: usize) -> usize {
    (n + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn check(status: zx_status_t, ctx: &str) -> Result<(), String> {
    if status == ZX_OK {
        Ok(())
    } else {
        Err(format!("{ctx}: zx_status={status}"))
    }
}

/// Zircon stub IOMMU object for creating per-device BTIs.
pub struct ZirconIommu {
    handle: zx_handle_t,
}

impl ZirconIommu {
    pub fn create(iommu_resource: zx_handle_t) -> Result<Self, String> {
        if iommu_resource == ZX_HANDLE_INVALID {
            return Err("iommu_resource is invalid".into());
        }
        let desc = zx_iommu_desc_stub_t { reserved: 0 };
        let mut handle = ZX_HANDLE_INVALID;
        check(
            unsafe {
                zx_iommu_create(
                    iommu_resource,
                    ZX_IOMMU_TYPE_STUB,
                    ptr::from_ref(&desc).cast(),
                    core::mem::size_of::<zx_iommu_desc_stub_t>(),
                    &mut handle,
                )
            },
            "zx_iommu_create(STUB)",
        )?;
        Ok(Self { handle })
    }

    pub fn create_bti(&self, bti_id: u64) -> Result<Bti, String> {
        let mut handle = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_bti_create(self.handle, 0, bti_id, &mut handle) },
            "zx_bti_create",
        )?;
        Ok(Bti { handle })
    }
}

impl Drop for ZirconIommu {
    fn drop(&mut self) {
        unsafe { zx_handle_close(self.handle) };
    }
}

/// Per-device Bus Transaction Initiator for DMA pinning.
pub struct Bti {
    pub handle: zx_handle_t,
}

impl Drop for Bti {
    fn drop(&mut self) {
        unsafe { zx_handle_close(self.handle) };
    }
}

/// Physically contiguous, BTI-pinned, host-mapped DMA buffer.
pub struct DmaBuf {
    pub vaddr: *mut u8,
    pub paddr: u64,
    pub len: usize,
    vmo: zx_handle_t,
    pmt: zx_handle_t,
}

unsafe impl Send for DmaBuf {}

impl DmaBuf {
    pub fn alloc(bti: &Bti, len: usize) -> Result<Self, String> {
        let size = page_up(len.max(PAGE_SIZE));
        let mut vmo = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_create_contiguous(bti.handle, size, 0, &mut vmo) },
            "zx_vmo_create_contiguous",
        )?;

        let mut paddr: zx_paddr_t = 0;
        let mut pmt = ZX_HANDLE_INVALID;
        if let Err(e) = check(
            unsafe {
                zx_bti_pin(
                    bti.handle,
                    ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE | ZX_BTI_CONTIGUOUS,
                    vmo,
                    0,
                    size as u64,
                    &mut paddr,
                    1,
                    &mut pmt,
                )
            },
            "zx_bti_pin",
        ) {
            unsafe { zx_handle_close(vmo) };
            return Err(e);
        }

        let mut vaddr: usize = 0;
        if let Err(e) = check(
            unsafe {
                zx_vmar_map(
                    zx_vmar_root_self(),
                    ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                    0,
                    vmo,
                    0,
                    size,
                    &mut vaddr,
                )
            },
            "zx_vmar_map(dma)",
        ) {
            unsafe {
                zx_pmt_unpin(pmt);
                zx_handle_close(vmo);
            }
            return Err(e);
        }

        unsafe { ptr::write_bytes(vaddr as *mut u8, 0, size) };
        Ok(Self {
            vaddr: vaddr as *mut u8,
            paddr: paddr as u64,
            len: size,
            vmo,
            pmt,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.vaddr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.vaddr, self.len) }
    }
}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        unsafe {
            zx_vmar_unmap(zx_vmar_root_self(), self.vaddr as usize, self.len);
            zx_pmt_unpin(self.pmt);
            zx_handle_close(self.vmo);
        }
    }
}

/// Uncached device MMIO mapping backed by `zx_vmo_create_physical`.
pub struct MmioRegion {
    pub base: *mut u8,
    pub len: usize,
    vmo: zx_handle_t,
}

unsafe impl Send for MmioRegion {}

impl MmioRegion {
    pub fn map(mmio_resource: zx_handle_t, paddr: u64, len: usize) -> Result<Self, String> {
        if mmio_resource == ZX_HANDLE_INVALID {
            return Err("mmio_resource is invalid".into());
        }
        let aligned_paddr = (paddr as usize) & !(PAGE_SIZE - 1);
        let offset_in_page = (paddr as usize) - aligned_paddr;
        let map_len = page_up(offset_in_page + len);

        let mut vmo = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_create_physical(mmio_resource, aligned_paddr, map_len, &mut vmo) },
            "zx_vmo_create_physical",
        )?;
        if let Err(e) = check(
            unsafe { zx_vmo_set_cache_policy(vmo, ZX_CACHE_POLICY_UNCACHED_DEVICE) },
            "zx_vmo_set_cache_policy",
        ) {
            unsafe { zx_handle_close(vmo) };
            return Err(e);
        }
        let mut vaddr: usize = 0;
        if let Err(e) = check(
            unsafe {
                zx_vmar_map(
                    zx_vmar_root_self(),
                    ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                    0,
                    vmo,
                    0,
                    map_len,
                    &mut vaddr,
                )
            },
            "zx_vmar_map(mmio)",
        ) {
            unsafe { zx_handle_close(vmo) };
            return Err(e);
        }
        Ok(Self {
            base: (vaddr + offset_in_page) as *mut u8,
            len: map_len,
            vmo,
        })
    }

    // Use explicit GPR `mov` inline asm for all MMIO accesses so LLVM can never
    // emit SSE/AVX instructions (e.g. `pextrd`) that hypervisor MMIO emulators reject.
    #[inline]
    pub fn read_u8(&self, off: usize) -> u8 {
        let val: u8;
        unsafe {
            core::arch::asm!(
                "mov {val}, byte ptr [{addr}]",
                addr = in(reg) self.base.add(off),
                val = out(reg_byte) val,
                options(nostack, preserves_flags),
            );
        }
        val
    }
    #[inline]
    pub fn write_u8(&self, off: usize, v: u8) {
        unsafe {
            core::arch::asm!(
                "mov byte ptr [{addr}], {val}",
                addr = in(reg) self.base.add(off),
                val = in(reg_byte) v,
                options(nostack, preserves_flags),
            );
        }
    }
    #[inline]
    pub fn read_u16(&self, off: usize) -> u16 {
        let val: u16;
        unsafe {
            core::arch::asm!(
                "mov {val:x}, word ptr [{addr}]",
                addr = in(reg) self.base.add(off),
                val = out(reg) val,
                options(nostack, preserves_flags),
            );
        }
        val
    }
    #[inline]
    pub fn write_u16(&self, off: usize, v: u16) {
        unsafe {
            core::arch::asm!(
                "mov word ptr [{addr}], {val:x}",
                addr = in(reg) self.base.add(off),
                val = in(reg) v,
                options(nostack, preserves_flags),
            );
        }
    }
    #[inline]
    pub fn read_u32(&self, off: usize) -> u32 {
        let val: u32;
        unsafe {
            core::arch::asm!(
                "mov {val:e}, dword ptr [{addr}]",
                addr = in(reg) self.base.add(off),
                val = out(reg) val,
                options(nostack, preserves_flags),
            );
        }
        val
    }
    #[inline]
    pub fn write_u32(&self, off: usize, v: u32) {
        unsafe {
            core::arch::asm!(
                "mov dword ptr [{addr}], {val:e}",
                addr = in(reg) self.base.add(off),
                val = in(reg) v,
                options(nostack, preserves_flags),
            );
        }
    }
}

impl Drop for MmioRegion {
    fn drop(&mut self) {
        let aligned_vaddr = (self.base as usize) & !(PAGE_SIZE - 1);
        unsafe {
            zx_vmar_unmap(zx_vmar_root_self(), aligned_vaddr, self.len);
            zx_handle_close(self.vmo);
        }
    }
}
