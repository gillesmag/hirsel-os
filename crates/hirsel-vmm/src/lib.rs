//! Tier 2 Hardware-Isolated MicroVM VMM for Hirsel-OS on Zircon.
//!
//! Uses Zircon's built-in kernel hypervisor (`zx_guest_create`, `zx_vcpu_create`,
//! `zx_vcpu_enter`, `zx_guest_set_trap`) backed by AMD SVM + NPT (and Intel VMX + EPT)
//! to execute:
//! - Freestanding 64-bit ELF payloads (`run_elf_microvm`),
//! - Unmodified Linux x86-64 static ELFs in Ring 3 (`run_linux_user_elf`), and
//! - Full upstream Linux x86-64 kernels (`vmlinux` ELF or `bzImage`) + `initramfs`
//!   (`run_linux_kernel`) with 16550A COM1 console, CMOS/RTC, i8042 `reboot=k`,
//!   KVM pvclock, and in-kernel x2APIC timer virtualization.

pub mod linux_boot;
pub mod mmio;
pub mod nat;
pub mod virtio_blk_mmio;
pub mod virtio_console_mmio;
pub mod virtio_net_mmio;

use core::mem::size_of;
use core::ptr;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "fuchsia")]
use virtio_blk_mmio::VmBlockBackend;
use virtio_console_mmio::VirtioConsoleChannel;
use virtio_net_mmio::VirtioNetChannel;

pub const PAGE_SIZE: usize = 4096;

/// Guest physical memory map constants.
pub const GPA_PML4: u64 = 0x0000_0000;
pub const GPA_PDPT: u64 = 0x0000_1000;
pub const GPA_PD_LOW: u64 = 0x0000_2000; // 0x0000_0000..0x4000_0000 (1 GiB, 2 MiB pages)
pub const GPA_PD_HIGH: u64 = 0x0000_3000; // 0xC000_0000..0x1_0000_0000 (MMIO window)
pub const GPA_BOOT_INFO: u64 = 0x0000_4000;
/// Auxiliary payload GPA (used to hand an unmodified Linux x86-64 static ELF to `hirsel-linux-shim`).
pub const GPA_AUX_PAYLOAD: u64 = 0x0020_0000;

/// Paravirt MMIO trap pages (unmapped in guest RAM -> trapped by NPT/EPT).
pub const GPA_MMIO_HEARTBEAT: u64 = 0xD000_0000;
pub const GPA_MMIO_BELL: u64 = 0xD000_1000;
pub const GPA_VIRTIO_CON: u64 = 0xD000_2000;

/// Virtual I/O ports.
pub const PORT_COM1_BASE: u16 = 0x03F8;
pub const PORT_COM1_LEN: usize = 8;
pub const PORT_PV_BASE: u16 = 0x0500;
pub const PORT_PV_LEN: usize = 16;

pub const PORT_PV_EXIT: u16 = 0x0500; // out u32: exit microVM with signed i32 status
pub const PORT_PV_MONO_LO: u16 = 0x0504; // in  u32: latch host monotonic ns, return low 32 bits
pub const PORT_PV_MONO_HI: u16 = 0x0508; // in  u32: return latched host monotonic ns high 32 bits
pub const PORT_PV_RNG: u16 = 0x050C; // in  u32: draw 32 bits from host zx_cprng_draw

pub const BOOT_INFO_MAGIC: u64 = 0x4852_534C_564D_3032; // "HRSLVM02"

/// Shared boot descriptor written at `GPA_BOOT_INFO` and passed to the guest in `RDI`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuestBootInfo {
    pub magic: u64,
    pub version: u32,
    pub flags: u32,
    pub ram_bytes: u64,
    pub mmio_heartbeat_gpa: u64,
    pub mmio_bell_gpa: u64,
    pub scratch_gpa: u64,
    pub scratch_bytes: u64,
    /// Optional auxiliary payload GPA (e.g. unmodified Linux x86-64 static ELF).
    pub aux_gpa: u64,
    /// Byte length of the auxiliary payload (0 if none).
    pub aux_bytes: u64,
    /// Paravirt MMIO console window GPA (`GPA_VIRTIO_CON`).
    pub virtio_con_gpa: u64,
}

/// Configuration for a Tier 2 microVM launch.
#[derive(Clone, Copy, Debug)]
pub struct MicroVmConfig {
    /// Guest RAM size in bytes (must be a multiple of 2 MiB, >= 2 MiB).
    pub ram_bytes: usize,
    /// Hard cap on VM exits before terminating a runaway/hostile guest.
    pub max_vm_exits: u64,
}

impl Default for MicroVmConfig {
    fn default() -> Self {
        Self {
            ram_bytes: 8 * 1024 * 1024, // 8 MiB default microVM RAM
            max_vm_exits: 100_000,
        }
    }
}

/// Outcome of a completed Tier 2 microVM execution.
#[derive(Clone, Debug)]
pub struct VmRunReport {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub io_exits: u64,
    pub mmio_exits: u64,
    pub bell_count: u64,
    pub elapsed_us: u64,
}

const MAX_CONSOLE_RING_BYTES: usize = 256 * 1024;

struct ConsoleRing {
    base_offset: u64,
    buf: VecDeque<u8>,
}

/// Live control & telemetry handle shared between a persistent managed Linux MicroVM
/// and `hirsel-init`'s VM manager / network stack.
pub struct ManagedVmControl {
    console_out: Mutex<ConsoleRing>,
    console_in: Mutex<VecDeque<u8>>,
    pub net_channel: Option<Arc<VirtioNetChannel>>,
    pub console_channel: Arc<VirtioConsoleChannel>,
    stop_requested: AtomicBool,
    pub io_exits: AtomicU64,
    pub mmio_exits: AtomicU64,
    pub blk_reads: AtomicU64,
    pub blk_writes: AtomicU64,
    #[cfg(target_os = "fuchsia")]
    vcpu_dups: Mutex<Vec<zx_sys::zx_handle_t>>,
}

impl ManagedVmControl {
    pub fn new(net_channel: Option<Arc<VirtioNetChannel>>) -> Arc<Self> {
        Arc::new(Self {
            console_out: Mutex::new(ConsoleRing {
                base_offset: 0,
                buf: VecDeque::with_capacity(16 * 1024),
            }),
            console_in: Mutex::new(VecDeque::with_capacity(256)),
            net_channel,
            console_channel: VirtioConsoleChannel::new(),
            stop_requested: AtomicBool::new(false),
            io_exits: AtomicU64::new(0),
            mmio_exits: AtomicU64::new(0),
            blk_reads: AtomicU64::new(0),
            blk_writes: AtomicU64::new(0),
            #[cfg(target_os = "fuchsia")]
            vcpu_dups: Mutex::new(Vec::new()),
        })
    }

    pub fn push_console_out(&self, byte: u8) {
        let mut ring = self.console_out.lock().unwrap();
        if ring.buf.len() >= MAX_CONSOLE_RING_BYTES {
            ring.buf.pop_front();
            ring.base_offset = ring.base_offset.saturating_add(1);
        }
        ring.buf.push_back(byte);
    }

    /// Reads up to `max_bytes` starting at logical stream `offset`, returning `(bytes, next_offset)`.
    pub fn read_console_logs(&self, offset: u64, max_bytes: usize) -> (Vec<u8>, u64) {
        let ring = self.console_out.lock().unwrap();
        let end_offset = ring.base_offset + ring.buf.len() as u64;
        let start = offset.max(ring.base_offset).min(end_offset);
        let rel_start = (start - ring.base_offset) as usize;
        let avail = ring.buf.len().saturating_sub(rel_start);
        let take = avail.min(max_bytes.max(1));
        let mut out = Vec::with_capacity(take);
        for b in ring.buf.iter().skip(rel_start).take(take) {
            out.push(*b);
        }
        let next = start + out.len() as u64;
        (out, next)
    }

    pub fn write_console_in(&self, bytes: &[u8]) {
        {
            let mut q = self.console_in.lock().unwrap();
            for &b in bytes {
                if q.len() >= 4096 {
                    q.pop_front();
                }
                q.push_back(b);
            }
        }
        self.kick();
    }

    pub fn drain_console_in(&self, dst: &mut VecDeque<u8>) -> bool {
        let mut q = self.console_in.lock().unwrap();
        if q.is_empty() {
            return false;
        }
        while let Some(b) = q.pop_front() {
            dst.push_back(b);
        }
        true
    }

    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
        self.kick_all();
    }

    #[cfg(target_os = "fuchsia")]
    pub fn kick(&self) {
        let dups = self.vcpu_dups.lock().unwrap();
        if let Some(&h) = dups.first() {
            if h != zx_sys::ZX_HANDLE_INVALID {
                unsafe { zx_sys::zx_vcpu_kick(h) };
            }
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    pub fn kick(&self) {}

    #[cfg(target_os = "fuchsia")]
    pub fn kick_all(&self) {
        let dups = self.vcpu_dups.lock().unwrap();
        for &h in dups.iter() {
            if h != zx_sys::ZX_HANDLE_INVALID {
                unsafe { zx_sys::zx_vcpu_kick(h) };
            }
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    pub fn kick_all(&self) {}

    #[cfg(target_os = "fuchsia")]
    pub fn interrupt_vcpu(&self, apic_id: usize, vector: u32) {
        let h = {
            let dups = self.vcpu_dups.lock().unwrap();
            dups.get(apic_id).copied().unwrap_or(zx_sys::ZX_HANDLE_INVALID)
        };
        if h != zx_sys::ZX_HANDLE_INVALID {
            let _ = unsafe { zx_sys::zx_vcpu_interrupt(h, vector) };
        }
    }

    #[cfg(target_os = "fuchsia")]
    fn set_vcpu_dup(&self, apic_id: usize, handle: zx_sys::zx_handle_t) {
        let mut dups = self.vcpu_dups.lock().unwrap();
        if dups.len() <= apic_id {
            dups.resize(apic_id + 1, zx_sys::ZX_HANDLE_INVALID);
        }
        if dups[apic_id] != zx_sys::ZX_HANDLE_INVALID {
            unsafe { zx_sys::zx_handle_close(dups[apic_id]) };
        }
        dups[apic_id] = handle;
    }

    #[cfg(target_os = "fuchsia")]
    fn clear_vcpu_dups(&self) {
        let mut dups = self.vcpu_dups.lock().unwrap();
        for h in dups.drain(..) {
            if h != zx_sys::ZX_HANDLE_INVALID {
                unsafe { zx_sys::zx_handle_close(h) };
            }
        }
    }
}

/// Writes x86-64 4-level identity page tables (2 MiB pages) at offsets `0x0000..0x4000`
/// of `ram`, covering `[0, 1 GiB)` and `[3 GiB, 4 GiB)` (so MMIO addresses at `0xD000_0000`
/// translate cleanly through guest paging and fault at the NPT/EPT layer).
pub fn init_identity_page_tables(ram: &mut [u8]) -> Result<(), String> {
    if ram.len() < 0x5000 {
        return Err("guest RAM too small for identity page tables".into());
    }
    const P: u64 = 1 << 0;
    const W: u64 = 1 << 1;
    const PS: u64 = 1 << 7;

    ram[0..0x4000].fill(0);

    let write_u64 = |buf: &mut [u8], off: usize, val: u64| {
        buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
    };

    write_u64(ram, GPA_PML4 as usize, GPA_PDPT | P | W);
    write_u64(ram, GPA_PDPT as usize, GPA_PD_LOW | P | W);
    write_u64(ram, GPA_PDPT as usize + 3 * 8, GPA_PD_HIGH | P | W);

    for i in 0..512u64 {
        let entry = (i << 21) | P | W | PS;
        write_u64(ram, GPA_PD_LOW as usize + (i as usize) * 8, entry);
    }
    for i in 0..512u64 {
        let entry = (0xC000_0000u64 + (i << 21)) | P | W | PS;
        write_u64(ram, GPA_PD_HIGH as usize + (i as usize) * 8, entry);
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

const PT_LOAD: u32 = 1;
const EM_X86_64: u16 = 62;

/// Loads a static 64-bit x86-64 ELF image (`vmlinux` or freestanding guest) into `ram`
/// and returns its entry GPA.
pub fn load_elf_into_ram(elf: &[u8], ram: &mut [u8]) -> Result<u64, String> {
    if elf.len() < size_of::<Elf64Ehdr>() || &elf[0..4] != b"\x7fELF" {
        return Err("guest image is not an ELF64 binary".into());
    }
    if elf[4] != 2 || elf[5] != 1 {
        return Err("guest ELF must be 64-bit little-endian".into());
    }
    let ehdr: Elf64Ehdr = unsafe { ptr::read_unaligned(elf.as_ptr().cast()) };
    if ehdr.e_machine != EM_X86_64 {
        return Err(format!("guest ELF machine {} != EM_X86_64", ehdr.e_machine));
    }
    let phoff = ehdr.e_phoff as usize;
    let phentsize = ehdr.e_phentsize as usize;
    let phnum = ehdr.e_phnum as usize;
    if phentsize < size_of::<Elf64Phdr>() || phoff + phnum * phentsize > elf.len() {
        return Err("guest ELF program header table out of bounds".into());
    }

    let mut loaded_any = false;
    for i in 0..phnum {
        let ph_ptr = unsafe { elf.as_ptr().add(phoff + i * phentsize).cast::<Elf64Phdr>() };
        let ph = unsafe { ptr::read_unaligned(ph_ptr) };
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        let dst_addr = if ph.p_paddr != 0 { ph.p_paddr } else { ph.p_vaddr } as usize;
        let filesz = ph.p_filesz as usize;
        let memsz = ph.p_memsz as usize;
        let src_off = ph.p_offset as usize;
        if filesz > memsz {
            return Err("guest ELF PT_LOAD filesz > memsz".into());
        }
        if src_off.checked_add(filesz).is_none_or(|e| e > elf.len()) {
            return Err("guest ELF PT_LOAD file range out of bounds".into());
        }
        if dst_addr < 0x5000 || dst_addr.checked_add(memsz).is_none_or(|e| e > ram.len()) {
            return Err(format!(
                "guest ELF PT_LOAD [{dst_addr:#x}, {:#x}) outside usable guest RAM ({} MiB)",
                dst_addr.saturating_add(memsz),
                ram.len() / (1024 * 1024),
            ));
        }
        ram[dst_addr..dst_addr + filesz].copy_from_slice(&elf[src_off..src_off + filesz]);
        if memsz > filesz {
            ram[dst_addr + filesz..dst_addr + memsz].fill(0);
        }
        loaded_any = true;
    }
    if !loaded_any {
        return Err("guest ELF has no PT_LOAD segments".into());
    }
    if (ehdr.e_entry as usize) >= ram.len() {
        return Err(format!("guest ELF entry {:#x} outside guest RAM", ehdr.e_entry));
    }
    Ok(ehdr.e_entry)
}

/// Loads either an uncompressed Linux `vmlinux` ELF64 or a Linux `bzImage` into `ram`,
/// populates Linux `boot_params` @ `0x7000`, cmdline @ `0x20000`, E820 map, SMP ACPI
/// MADT/DSDT tables, and optional `initramfs`, returning the 64-bit `startup_64` entry GPA.
pub fn load_linux_kernel_into_ram_smp(
    kernel: &[u8],
    initrd: Option<&[u8]>,
    cmdline: &str,
    ram: &mut [u8],
    vcpus: u8,
) -> Result<u64, String> {
    if kernel.len() >= 4 && &kernel[0..4] == b"\x7fELF" {
        let entry = load_elf_into_ram(kernel, ram)?;
        linux_boot::install_linux_boot_params_smp(ram, cmdline, initrd, None, vcpus)?;
        Ok(entry)
    } else {
        let bz = linux_boot::parse_bzimage(kernel)?;
        let payload = &kernel[bz.payload_offset..];
        let dst = bz.pref_address as usize;
        if dst.checked_add(payload.len()).is_none_or(|e| e > ram.len()) {
            return Err(format!(
                "bzImage payload ({} bytes at {dst:#x}) exceeds guest RAM ({} MiB)",
                payload.len(),
                ram.len() / (1024 * 1024),
            ));
        }
        ram[dst..dst + payload.len()].copy_from_slice(payload);
        linux_boot::install_linux_boot_params_smp(ram, cmdline, initrd, Some(kernel), vcpus)?;
        Ok(bz.code64_entry)
    }
}

pub fn load_linux_kernel_into_ram(
    kernel: &[u8],
    initrd: Option<&[u8]>,
    cmdline: &str,
    ram: &mut [u8],
) -> Result<u64, String> {
    load_linux_kernel_into_ram_smp(kernel, initrd, cmdline, ram, 1)
}

#[cfg(target_os = "fuchsia")]
mod fuchsia_vmm {
    use super::*;
    use crate::mmio::{MmioAccess, decode_mmio_inst, fetch_guest_inst_bytes, get_gpr, set_gpr};
    use crate::virtio_blk_mmio::{GPA_VIRTIO_BLK_MMIO, VIRTIO_BLK_IRQ, VirtioBlkMmio};
    use crate::virtio_console_mmio::{GPA_VIRTIO_CON_MMIO, VIRTIO_CON_IRQ, VirtioConsoleMmio};
    use crate::virtio_net_mmio::{GPA_VIRTIO_NET_MMIO, VIRTIO_NET_IRQ, VirtioNetMmio};
    use std::thread;
    use zx_sys::*;

    const TRAP_KEY_COM1: u64 = 1;
    const TRAP_KEY_PV: u64 = 2;
    const TRAP_KEY_MMIO_HEARTBEAT: u64 = 3;
    const TRAP_KEY_MMIO_BELL: u64 = 4;
    const TRAP_KEY_VIRTIO_CON: u64 = 5;
    const TRAP_KEY_ALL_IO: u64 = 6;
    const TRAP_KEY_VIRTIO_NET: u64 = 7;
    const TRAP_KEY_VIRTIO_BLK: u64 = 8;
    const TRAP_KEY_VIRTIO_CON_MMIO: u64 = 9;

    struct LaunchSpec {
        entry_gpa: u64,
        rsp: u64,
        rdi: u64,
        rsi: u64,
        trap_all_io: bool,
    }

    struct ManagedLaunch {
        ctrl: Arc<ManagedVmControl>,
        vcpus: u8,
        mac: Option<[u8; 6]>,
        blk_backend: Option<Arc<Mutex<dyn VmBlockBackend>>>,
    }

    /// Minimal 16550A UART state so Linux's `8250` serial driver probes and opens COM1 cleanly.
    struct Uart16550 {
        ier: u8,
        lcr: u8,
        mcr: u8,
        scr: u8,
        dll: u8,
        dlm: u8,
        fcr: u8,
        thr_ipending: bool,
        rx_queue: VecDeque<u8>,
    }

    impl Uart16550 {
        fn new() -> Self {
            Self {
                ier: 0,
                lcr: 0x03, // 8N1
                mcr: 0,
                scr: 0,
                dll: 0x01, // 115200 baud
                dlm: 0,
                fcr: 0,
                thr_ipending: false,
                rx_queue: VecDeque::with_capacity(64),
            }
        }

        fn dlab(&self) -> bool {
            (self.lcr & 0x80) != 0
        }

        fn read(&mut self, reg: u16) -> u8 {
            match reg {
                0 => {
                    if self.dlab() {
                        self.dll
                    } else {
                        self.rx_queue.pop_front().unwrap_or(0)
                    }
                }
                1 => {
                    if self.dlab() {
                        self.dlm
                    } else {
                        self.ier
                    }
                }
                2 => {
                    // IIR: bit 0 = 0 if interrupt pending, 1 if none; bits 6..7 = FIFO enabled.
                    let fifo = if (self.fcr & 1) != 0 { 0xC0 } else { 0x00 };
                    if !self.rx_queue.is_empty() && (self.ier & 0x01) != 0 {
                        fifo | 0x04 // RDA interrupt pending
                    } else if self.thr_ipending && (self.ier & 0x02) != 0 {
                        self.thr_ipending = false;
                        fifo | 0x02 // THRE interrupt pending
                    } else {
                        fifo | 0x01 // No interrupt pending
                    }
                }
                3 => self.lcr,
                4 => self.mcr,
                5 => {
                    // LSR: THRE (0x20) | TEMT (0x40), plus DR (0x01) if RX byte queued.
                    if !self.rx_queue.is_empty() { 0x61 } else { 0x60 }
                }
                6 => {
                    // MSR: in loopback mode (MCR bit 4), reflect MCR[3:0] into MSR[7:4].
                    if (self.mcr & 0x10) != 0 {
                        let mut msr = 0u8;
                        if (self.mcr & 0x02) != 0 {
                            msr |= 0x10; // RTS -> CTS
                        }
                        if (self.mcr & 0x01) != 0 {
                            msr |= 0x20; // DTR -> DSR
                        }
                        if (self.mcr & 0x04) != 0 {
                            msr |= 0x40; // OUT1 -> RI
                        }
                        if (self.mcr & 0x08) != 0 {
                            msr |= 0x80; // OUT2 -> DCD
                        }
                        msr
                    } else {
                        0xB0 // DCD | DSR | CTS
                    }
                }
                7 => self.scr,
                _ => 0,
            }
        }

        fn write(&mut self, reg: u16, val: u8, stdout: &mut Vec<u8>, managed: Option<&ManagedVmControl>) {
            match reg {
                0 => {
                    if self.dlab() {
                        self.dll = val;
                    } else if (self.mcr & 0x10) != 0 {
                        // Hardware loopback self-test (MCR bit 4): route TX back to RBR.
                        self.rx_queue.push_back(val);
                    } else {
                        if let Some(ctrl) = managed {
                            ctrl.push_console_out(val);
                        } else {
                            stdout.push(val);
                        }
                        if (self.ier & 0x02) != 0 {
                            self.thr_ipending = true;
                        }
                    }
                }
                1 => {
                    if self.dlab() {
                        self.dlm = val;
                    } else {
                        let prev = self.ier;
                        self.ier = val;
                        if (prev & 0x02) == 0 && (val & 0x02) != 0 {
                            self.thr_ipending = true;
                        }
                    }
                }
                2 => self.fcr = val,
                3 => self.lcr = val,
                4 => self.mcr = val,
                7 => self.scr = val,
                _ => {}
            }
        }
        fn irq_pending(&self) -> bool {
            // Gate COM1 IRQ 4 through OUT2 (MCR bit 3) and suppress during loopback (MCR bit 4).
            (self.mcr & 0x18) == 0x08
                && ((self.thr_ipending && (self.ier & 0x02) != 0)
                    || (!self.rx_queue.is_empty() && (self.ier & 0x01) != 0))
        }
    }

    /// Minimal Intel 8259A PIC chip emulation so Linux can receive COM1 IRQ 4 cleanly.
    struct Pic8259 {
        icw_step: u8,
        icw4_needed: bool,
        vector_base: u8,
        imr: u8,
        isr: u8,
        read_isr: bool,
    }

    impl Pic8259 {
        fn new() -> Self {
            Self {
                icw_step: 0,
                icw4_needed: false,
                vector_base: 0,
                imr: 0xFF,
                isr: 0,
                read_isr: false,
            }
        }

        fn read_cmd(&self) -> u8 {
            if self.read_isr { self.isr } else { 0 }
        }

        fn read_data(&self) -> u8 {
            self.imr
        }

        fn write_cmd(&mut self, val: u8) {
            if (val & 0x10) != 0 {
                // ICW1: start initialization sequence.
                self.icw_step = 1;
                self.icw4_needed = (val & 0x01) != 0;
                self.isr = 0;
            } else if (val & 0x20) != 0 {
                // OCW2: specific or non-specific EOI.
                if (val & 0x40) != 0 {
                    let irq = val & 0x07;
                    self.isr &= !(1u8 << irq);
                } else {
                    self.isr = 0;
                }
            } else if (val & 0x08) != 0 {
                // OCW3: select IRR (0x0A) or ISR (0x0B) for next cmd read.
                if (val & 0x02) != 0 {
                    self.read_isr = (val & 0x01) != 0;
                }
            }
        }

        fn write_data(&mut self, val: u8) {
            match self.icw_step {
                1 => {
                    self.vector_base = val & 0xF8;
                    self.icw_step = 2;
                }
                2 => {
                    self.icw_step = if self.icw4_needed { 3 } else { 0 };
                }
                3 => {
                    self.icw_step = 0;
                }
                _ => {
                    self.imr = val;
                }
            }
        }

        fn try_deliver(&mut self, irq: u8) -> Option<u32> {
            let mask = 1u8 << irq;
            if self.vector_base >= 0x20 && (self.imr & mask) == 0 && (self.isr & mask) == 0 {
                self.isr |= mask;
                Some(u32::from(self.vector_base + irq))
            } else {
                None
            }
        }
    }

    /// Minimal PC legacy I/O state (8259A PIC, CMOS/RTC, i8042, Port 0x61, PIT) for Linux boot.
    struct PcLegacyIo {
        pic_master: Pic8259,
        pic_slave: Pic8259,
        cmos_idx: u8,
        i8042_cmd: u8,
        port61: u8,
        pit_tick: u16,
    }

    impl PcLegacyIo {
        fn new() -> Self {
            Self {
                pic_master: Pic8259::new(),
                pic_slave: Pic8259::new(),
                cmos_idx: 0,
                i8042_cmd: 0,
                port61: 0,
                pit_tick: 0xFFFF,
            }
        }

        fn read_cmos(&self) -> u8 {
            match self.cmos_idx {
                0x00 => 0x00, // seconds (BCD)
                0x02 => 0x00, // minutes
                0x04 => 0x12, // hours
                0x06 => 0x05, // day of week
                0x07 => 0x27, // day of month
                0x08 => 0x11, // month
                0x09 => 0x26, // year
                0x32 => 0x20, // century
                0x0A => 0x26, // Status A (UIP=0)
                0x0B => 0x02, // Status B (24-hour mode)
                0x0C => 0x00, // Status C
                0x0D => 0x80, // Status D (CMOS RAM valid)
                _ => 0,
            }
        }
    }

    fn check(status: zx_status_t, ctx: &str) -> Result<(), String> {
        if status == ZX_OK {
            Ok(())
        } else {
            Err(format!("{ctx}: zx_status={status}"))
        }
    }

    fn read_packet<T: Copy>(pkt: &zx_port_packet_t) -> T {
        const { assert!(size_of::<T>() <= 32) };
        unsafe { ptr::read_unaligned(pkt.payload.as_ptr().cast()) }
    }

    /// Runs a freestanding 64-bit ELF binary inside a hardware-isolated Zircon hypervisor
    /// microVM (AMD SVM + NPT or Intel VMX + EPT).
    pub fn run_elf_microvm(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        elf_bytes: &[u8],
        config: MicroVmConfig,
    ) -> Result<VmRunReport, String> {
        let ram_bytes = config.ram_bytes;
        run_microvm_inner(hypervisor_resource, vmex_resource, config, None, |ram, _| {
            let entry_gpa = load_elf_into_ram(elf_bytes, ram)?;
            Ok(LaunchSpec {
                entry_gpa,
                rsp: (ram_bytes as u64 - 0x1000) & !0xf,
                rdi: GPA_BOOT_INFO,
                rsi: 0,
                trap_all_io: false,
            })
        })
    }

    /// Runs an unmodified Linux x86-64 static ELF in Ring 3 inside a Tier 2 microVM
    /// hosted by our Ring-0 Linux syscall shim (`bin/hirsel-linux-shim.elf`).
    pub fn run_linux_user_elf(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        shim_elf: &[u8],
        linux_user_elf: &[u8],
        config: MicroVmConfig,
    ) -> Result<VmRunReport, String> {
        let mut cfg = config;
        if cfg.ram_bytes < 16 * 1024 * 1024 {
            cfg.ram_bytes = 16 * 1024 * 1024;
        }
        let ram_bytes = cfg.ram_bytes;
        run_microvm_inner(
            hypervisor_resource,
            vmex_resource,
            cfg,
            None,
            |ram, boot_info| {
                let aux_start = GPA_AUX_PAYLOAD as usize;
                let aux_end = aux_start
                    .checked_add(linux_user_elf.len())
                    .ok_or_else(|| "Linux ELF size overflow".to_string())?;
                if aux_end > 0x0040_0000 {
                    return Err(format!(
                        "Linux static ELF ({} bytes) exceeds 2 MiB staging window",
                        linux_user_elf.len()
                    ));
                }
                ram[aux_start..aux_end].copy_from_slice(linux_user_elf);
                boot_info.aux_gpa = GPA_AUX_PAYLOAD;
                boot_info.aux_bytes = linux_user_elf.len() as u64;
                let _ = linux_boot::install_linux_boot_params(
                    ram,
                    "console=ttyS0 hirsel.tier=2",
                    None,
                    None,
                );
                let entry_gpa = load_elf_into_ram(shim_elf, ram)?;
                Ok(LaunchSpec {
                    entry_gpa,
                    rsp: (ram_bytes as u64 - 0x1000) & !0xf,
                    rdi: GPA_BOOT_INFO,
                    rsi: 0,
                    trap_all_io: false,
                })
            },
        )
    }

    /// Boots a REAL upstream Linux x86-64 kernel (`vmlinux` ELF or `bzImage`) + optional
    /// `initramfs` inside a hardware-isolated Zircon hypervisor microVM (one-shot).
    pub fn run_linux_kernel(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        kernel_bytes: &[u8],
        initrd_bytes: Option<&[u8]>,
        cmdline: &str,
        config: MicroVmConfig,
    ) -> Result<VmRunReport, String> {
        let mut cfg = config;
        if cfg.ram_bytes < 64 * 1024 * 1024 {
            cfg.ram_bytes = 128 * 1024 * 1024;
        }
        if cfg.max_vm_exits < 500_000 {
            cfg.max_vm_exits = 2_000_000;
        }
        let cmdline_eff = if cmdline.is_empty() {
            "console=ttyS0 reboot=k panic=1 pci=off nomodules"
        } else {
            cmdline
        };
        run_microvm_inner(hypervisor_resource, vmex_resource, cfg, None, |ram, _| {
            let entry_gpa =
                load_linux_kernel_into_ram(kernel_bytes, initrd_bytes, cmdline_eff, ram)?;
            Ok(LaunchSpec {
                entry_gpa,
                rsp: 0x0000_6000,
                rdi: 0,
                rsi: linux_boot::GPA_ZERO_PAGE,
                trap_all_io: true,
            })
        })
    }

    /// Runs a persistent, operator-managed upstream Linux x86-64 MicroVM with SMP multi-vCPU
    /// support, live console streaming/input, optional Virtio-MMIO `virtio-net` (IRQ 5),
    /// optional persistent Virtio-MMIO `virtio-blk` (IRQ 6 -> `/dev/vda`), Virtio-MMIO
    /// `virtio-console` (IRQ 7 -> `/dev/hvc0`), and clean remote stop via `ManagedVmControl`.
    #[allow(clippy::too_many_arguments)]
    pub fn run_managed_linux_vm(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        kernel_bytes: &[u8],
        initrd_bytes: Option<&[u8]>,
        cmdline: &str,
        ram_bytes: usize,
        vcpus: u8,
        mac: Option<[u8; 6]>,
        blk_backend: Option<Arc<Mutex<dyn VmBlockBackend>>>,
        ctrl: Arc<ManagedVmControl>,
    ) -> Result<VmRunReport, String> {
        let vcpus_eff = vcpus.clamp(1, 8);
        let cfg = MicroVmConfig {
            ram_bytes: ram_bytes.max(64 * 1024 * 1024),
            max_vm_exits: u64::MAX,
        };
        let cmdline_eff = if cmdline.is_empty() {
            "console=ttyS0 reboot=k panic=1 pci=off nomodules"
        } else {
            cmdline
        };
        run_microvm_inner(
            hypervisor_resource,
            vmex_resource,
            cfg,
            Some(ManagedLaunch {
                ctrl,
                vcpus: vcpus_eff,
                mac,
                blk_backend,
            }),
            |ram, _| {
                let entry_gpa = load_linux_kernel_into_ram_smp(
                    kernel_bytes,
                    initrd_bytes,
                    cmdline_eff,
                    ram,
                    vcpus_eff,
                )?;
                Ok(LaunchSpec {
                    entry_gpa,
                    rsp: 0x0000_6000,
                    rdi: 0,
                    rsi: linux_boot::GPA_ZERO_PAGE,
                    trap_all_io: true,
                })
            },
        )
    }

    /// Runs raw x86-64 machine code placed at `entry_gpa` inside a hardware-isolated
    /// Zircon hypervisor microVM (useful for self-tests and isolation checks).
    pub fn run_raw_microvm(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        entry_gpa: u64,
        code: &[u8],
        config: MicroVmConfig,
    ) -> Result<VmRunReport, String> {
        let ram_bytes = config.ram_bytes;
        run_microvm_inner(hypervisor_resource, vmex_resource, config, None, |ram, _| {
            let start = entry_gpa as usize;
            if start < 0x5000 || start + code.len() > ram.len() {
                return Err("raw guest code out of guest RAM bounds".into());
            }
            ram[start..start + code.len()].copy_from_slice(code);
            Ok(LaunchSpec {
                entry_gpa,
                rsp: (ram_bytes as u64 - 0x1000) & !0xf,
                rdi: GPA_BOOT_INFO,
                rsi: 0,
                trap_all_io: false,
            })
        })
    }

    fn run_microvm_inner<F>(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        config: MicroVmConfig,
        managed: Option<ManagedLaunch>,
        load_guest: F,
    ) -> Result<VmRunReport, String>
    where
        F: FnOnce(&mut [u8], &mut GuestBootInfo) -> Result<LaunchSpec, String>,
    {
        if hypervisor_resource == ZX_HANDLE_INVALID {
            return Err("hypervisor resource handle is invalid".into());
        }
        if vmex_resource == ZX_HANDLE_INVALID {
            return Err("vmex resource handle is invalid".into());
        }
        if config.ram_bytes < 2 * 1024 * 1024 || (config.ram_bytes & (PAGE_SIZE - 1)) != 0 {
            return Err("ram_bytes must be page-aligned and >= 2 MiB".into());
        }

        let t0 = unsafe { zx_clock_get_monotonic() };

        // 1. Create Zircon Guest + guest physical VMAR (NPT/EPT address space).
        let mut guest = ZX_HANDLE_INVALID;
        let mut guest_vmar = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_guest_create(hypervisor_resource, 0, &mut guest, &mut guest_vmar) },
            "zx_guest_create",
        )?;

        // 2. Allocate guest RAM VMO, mark executable via `vmex_resource`, and map into
        //    both host VMM (`RW`) and guest NPT/EPT VMAR (`RWX`).
        let mut raw_ram_vmo = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_create(config.ram_bytes as u64, 0, &mut raw_ram_vmo) },
            "zx_vmo_create(guest_ram)",
        )?;
        let mut ram_vmo = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_replace_as_executable(raw_ram_vmo, vmex_resource, &mut ram_vmo) },
            "zx_vmo_replace_as_executable(guest_ram)",
        )?;
        let _ = unsafe {
            zx_object_set_property(ram_vmo, ZX_PROP_NAME, b"hirsel-vm-ram".as_ptr(), 13)
        };

        let host_root_vmar = unsafe { zx_vmar_root_self() };
        let mut host_ram_base: usize = 0;
        check(
            unsafe {
                zx_vmar_map(
                    host_root_vmar,
                    ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                    0,
                    ram_vmo,
                    0,
                    config.ram_bytes,
                    &mut host_ram_base,
                )
            },
            "zx_vmar_map(host_ram)",
        )?;

        let mut guest_ram_base: usize = 0;
        let map_guest_res = check(
            unsafe {
                zx_vmar_map(
                    guest_vmar,
                    ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_PERM_EXECUTE | ZX_VM_SPECIFIC,
                    0,
                    ram_vmo,
                    0,
                    config.ram_bytes,
                    &mut guest_ram_base,
                )
            },
            "zx_vmar_map(guest_ram)",
        );
        if let Err(e) = map_guest_res {
            unsafe {
                zx_vmar_unmap(host_root_vmar, host_ram_base, config.ram_bytes);
                zx_handle_close(ram_vmo);
                zx_handle_close(guest_vmar);
                zx_handle_close(guest);
            }
            return Err(e);
        }

        // 3. Populate 4-level identity page tables, GuestBootInfo, and guest image.
        let load_res = {
            let ram_slice =
                unsafe { core::slice::from_raw_parts_mut(host_ram_base as *mut u8, config.ram_bytes) };
            init_identity_page_tables(ram_slice).and_then(|()| {
                let mut boot_info = GuestBootInfo {
                    magic: BOOT_INFO_MAGIC,
                    version: 2,
                    flags: 0,
                    ram_bytes: config.ram_bytes as u64,
                    mmio_heartbeat_gpa: GPA_MMIO_HEARTBEAT,
                    mmio_bell_gpa: GPA_MMIO_BELL,
                    scratch_gpa: 0x0040_0000,
                    scratch_bytes: 0x0010_0000,
                    aux_gpa: 0,
                    aux_bytes: 0,
                    virtio_con_gpa: GPA_VIRTIO_CON,
                };
                let spec = load_guest(ram_slice, &mut boot_info)?;
                unsafe {
                    ptr::write_unaligned(
                        ram_slice.as_mut_ptr().add(GPA_BOOT_INFO as usize).cast::<GuestBootInfo>(),
                        boot_info,
                    );
                }
                Ok(spec)
            })
        };

        let spec = match load_res {
            Ok(s) => s,
            Err(err) => {
                unsafe {
                    zx_vmar_unmap(host_root_vmar, host_ram_base, config.ram_bytes);
                    zx_handle_close(ram_vmo);
                    zx_handle_close(guest_vmar);
                    zx_handle_close(guest);
                }
                return Err(err);
            }
        };

        // 4. Register hardware traps: I/O ports + MMIO devices.
        let mut bell_port = ZX_HANDLE_INVALID;
        let setup_traps = check(unsafe { zx_port_create(0, &mut bell_port) }, "zx_port_create")
            .and_then(|()| {
                if spec.trap_all_io {
                    check(
                        unsafe {
                            zx_guest_set_trap(
                                guest,
                                ZX_GUEST_TRAP_IO,
                                0,
                                0xFFFF,
                                ZX_HANDLE_INVALID,
                                TRAP_KEY_ALL_IO,
                            )
                        },
                        "zx_guest_set_trap(ALL_IO)",
                    )
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_MEM,
                                    GPA_VIRTIO_NET_MMIO as usize,
                                    PAGE_SIZE,
                                    ZX_HANDLE_INVALID,
                                    TRAP_KEY_VIRTIO_NET,
                                )
                            },
                            "zx_guest_set_trap(VIRTIO_NET)",
                        )
                    })
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_MEM,
                                    GPA_VIRTIO_BLK_MMIO as usize,
                                    PAGE_SIZE,
                                    ZX_HANDLE_INVALID,
                                    TRAP_KEY_VIRTIO_BLK,
                                )
                            },
                            "zx_guest_set_trap(VIRTIO_BLK)",
                        )
                    })
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_MEM,
                                    GPA_VIRTIO_CON_MMIO as usize,
                                    PAGE_SIZE,
                                    ZX_HANDLE_INVALID,
                                    TRAP_KEY_VIRTIO_CON_MMIO,
                                )
                            },
                            "zx_guest_set_trap(VIRTIO_CON_MMIO)",
                        )
                    })
                } else {
                    check(
                        unsafe {
                            zx_guest_set_trap(
                                guest,
                                ZX_GUEST_TRAP_IO,
                                PORT_COM1_BASE as usize,
                                PORT_COM1_LEN,
                                ZX_HANDLE_INVALID,
                                TRAP_KEY_COM1,
                            )
                        },
                        "zx_guest_set_trap(COM1)",
                    )
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_IO,
                                    PORT_PV_BASE as usize,
                                    PORT_PV_LEN,
                                    ZX_HANDLE_INVALID,
                                    TRAP_KEY_PV,
                                )
                            },
                            "zx_guest_set_trap(PV)",
                        )
                    })
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_MEM,
                                    GPA_MMIO_HEARTBEAT as usize,
                                    PAGE_SIZE,
                                    ZX_HANDLE_INVALID,
                                    TRAP_KEY_MMIO_HEARTBEAT,
                                )
                            },
                            "zx_guest_set_trap(MMIO_HEARTBEAT)",
                        )
                    })
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_BELL,
                                    GPA_MMIO_BELL as usize,
                                    PAGE_SIZE,
                                    bell_port,
                                    TRAP_KEY_MMIO_BELL,
                                )
                            },
                            "zx_guest_set_trap(MMIO_BELL)",
                        )
                    })
                    .and_then(|()| {
                        check(
                            unsafe {
                                zx_guest_set_trap(
                                    guest,
                                    ZX_GUEST_TRAP_MEM,
                                    GPA_VIRTIO_CON as usize,
                                    PAGE_SIZE,
                                    ZX_HANDLE_INVALID,
                                    TRAP_KEY_VIRTIO_CON,
                                )
                            },
                            "zx_guest_set_trap(VIRTIO_CON)",
                        )
                    })
                }
            });

        if let Err(err) = setup_traps {
            unsafe {
                if bell_port != ZX_HANDLE_INVALID {
                    zx_handle_close(bell_port);
                }
                zx_vmar_unmap(host_root_vmar, host_ram_base, config.ram_bytes);
                zx_handle_close(ram_vmo);
                zx_handle_close(guest_vmar);
                zx_handle_close(guest);
            }
            return Err(err);
        }

        // 5. Run BSP (and dynamically spawned SMP AP vCPUs) on dedicated Zircon threads.
        let max_exits = config.max_vm_exits;
        let ram_bytes_len = config.ram_bytes;
        let max_vcpus = managed.as_ref().map_or(1, |m| m.vcpus.clamp(1, 8));

        struct SharedDevices {
            stdout: Vec<u8>,
            uart: Uart16550,
            legacy: PcLegacyIo,
            virtio_net: Option<VirtioNetMmio>,
            virtio_blk: Option<VirtioBlkMmio>,
            virtio_con: Option<VirtioConsoleMmio>,
            latched_mono: u64,
            exit_code: Option<i32>,
            loop_err: Option<String>,
        }

        struct SharedVmState {
            guest: zx_handle_t,
            host_ram_base: usize,
            ram_bytes_len: usize,
            trap_all_io: bool,
            max_exits: u64,
            max_vcpus: u8,
            managed_ctrl: Option<Arc<ManagedVmControl>>,
            stop_all: AtomicBool,
            io_exits: AtomicU64,
            mmio_exits: AtomicU64,
            total_exits: AtomicU64,
            devs: Mutex<SharedDevices>,
            local_vcpu_dups: Mutex<Vec<zx_handle_t>>,
            ap_threads: Mutex<Vec<Option<thread::JoinHandle<()>>>>,
        }

        impl SharedVmState {
            fn register_vcpu(&self, apic_id: usize, vcpu: zx_handle_t) {
                let mut dup = ZX_HANDLE_INVALID;
                if unsafe { zx_handle_duplicate(vcpu, ZX_RIGHT_SAME_RIGHTS, &mut dup) } == ZX_OK {
                    if let Some(ctrl) = self.managed_ctrl.as_ref() {
                        ctrl.set_vcpu_dup(apic_id, dup);
                    } else {
                        let mut dups = self.local_vcpu_dups.lock().unwrap();
                        if dups.len() <= apic_id {
                            dups.resize(apic_id + 1, ZX_HANDLE_INVALID);
                        }
                        if dups[apic_id] != ZX_HANDLE_INVALID {
                            unsafe { zx_handle_close(dups[apic_id]) };
                        }
                        dups[apic_id] = dup;
                    }
                }
            }

            fn interrupt_vcpu(&self, apic_id: usize, vector: u32) {
                if let Some(ctrl) = self.managed_ctrl.as_ref() {
                    ctrl.interrupt_vcpu(apic_id, vector);
                } else {
                    let h = self
                        .local_vcpu_dups
                        .lock()
                        .unwrap()
                        .get(apic_id)
                        .copied()
                        .unwrap_or(ZX_HANDLE_INVALID);
                    if h != ZX_HANDLE_INVALID {
                        let _ = unsafe { zx_vcpu_interrupt(h, vector) };
                    }
                }
            }

            fn signal_stop(&self) {
                self.stop_all.store(true, Ordering::Release);
                if let Some(ctrl) = self.managed_ctrl.as_ref() {
                    ctrl.kick_all();
                } else {
                    let dups = self.local_vcpu_dups.lock().unwrap();
                    for &h in dups.iter() {
                        if h != ZX_HANDLE_INVALID {
                            unsafe { zx_vcpu_kick(h) };
                        }
                    }
                }
            }

            fn deliver_pic_irqs(&self, devs: &mut SharedDevices, cur_vcpu: zx_handle_t, apic_id: usize) {
                if !self.trap_all_io {
                    return;
                }
                let mut inject = |irq: u8| {
                    if let Some(vec) = devs.legacy.pic_master.try_deliver(irq) {
                        if apic_id == 0 {
                            let _ = unsafe { zx_vcpu_interrupt(cur_vcpu, vec) };
                        } else {
                            self.interrupt_vcpu(0, vec);
                        }
                    }
                };
                if devs.uart.irq_pending() {
                    inject(4);
                }
                if devs.virtio_net.as_ref().is_some_and(|v| v.irq_pending()) {
                    inject(VIRTIO_NET_IRQ);
                }
                if devs.virtio_blk.as_ref().is_some_and(|v| v.irq_pending()) {
                    inject(VIRTIO_BLK_IRQ);
                }
                if devs.virtio_con.as_ref().is_some_and(|v| v.irq_pending()) {
                    inject(VIRTIO_CON_IRQ);
                }
            }
        }

        fn run_vcpu_loop(vcpu: zx_handle_t, apic_id: usize, state: Arc<SharedVmState>) {
            let ram_slice = unsafe {
                core::slice::from_raw_parts_mut(state.host_ram_base as *mut u8, state.ram_bytes_len)
            };

            while !state.stop_all.load(Ordering::Acquire) {
                if state.total_exits.load(Ordering::Relaxed) >= state.max_exits {
                    break;
                }

                // On BSP, drain incoming console stdin, Virtio-Net RX, and Virtio-Console RX before entering.
                if apic_id == 0 {
                    if let Some(ctrl) = state.managed_ctrl.as_deref() {
                        if ctrl.is_stop_requested() {
                            let mut devs = state.devs.lock().unwrap();
                            devs.exit_code.get_or_insert(0);
                            drop(devs);
                            state.signal_stop();
                            break;
                        }
                    }
                    let mut devs = state.devs.lock().unwrap();
                    if let Some(ctrl) = state.managed_ctrl.as_deref() {
                        let _ = ctrl.drain_console_in(&mut devs.uart.rx_queue);
                    }
                    if let Some(vnet) = devs.virtio_net.as_mut() {
                        let _ = vnet.process_rx(ram_slice);
                    }
                    if let Some(vcon) = devs.virtio_con.as_mut() {
                        let _ = vcon.process_rx(ram_slice);
                    }
                    state.deliver_pic_irqs(&mut devs, vcpu, 0);
                }

                let mut pkt = zx_port_packet_t::default();
                let st = unsafe { zx_vcpu_enter(vcpu, &mut pkt) };
                if st == ZX_ERR_CANCELED {
                    if state.stop_all.load(Ordering::Acquire)
                        || state
                            .managed_ctrl
                            .as_deref()
                            .is_some_and(|c| c.is_stop_requested())
                    {
                        if apic_id == 0 {
                            let mut devs = state.devs.lock().unwrap();
                            devs.exit_code.get_or_insert(0);
                        }
                        state.signal_stop();
                        break;
                    }
                    continue;
                }
                if st != ZX_OK {
                    let mut devs = state.devs.lock().unwrap();
                    if state.trap_all_io
                        && st == ZX_ERR_BAD_STATE
                        && (!devs.stdout.is_empty() || state.managed_ctrl.is_some())
                    {
                        devs.exit_code.get_or_insert(0);
                        drop(devs);
                        state.signal_stop();
                        break;
                    }
                    let mut st_regs = zx_vcpu_state_t::default();
                    let _ = unsafe {
                        zx_vcpu_read_state(
                            vcpu,
                            ZX_VCPU_STATE,
                            ptr::from_mut(&mut st_regs).cast(),
                            size_of::<zx_vcpu_state_t>(),
                        )
                    };
                    devs.loop_err.get_or_insert_with(|| {
                        format!(
                            "vcpu[{apic_id}] zx_vcpu_enter exited with zx_status={st} (io_exits={}, mmio_exits={}, rsp={:#x}, rax={:#x}, rflags={:#x})",
                            state.io_exits.load(Ordering::Relaxed),
                            state.mmio_exits.load(Ordering::Relaxed),
                            st_regs.rsp,
                            st_regs.rax,
                            st_regs.rflags
                        )
                    });
                    drop(devs);
                    state.signal_stop();
                    break;
                }

                state.total_exits.fetch_add(1, Ordering::Relaxed);

                match pkt.packet_type {
                    ZX_PKT_TYPE_GUEST_VCPU => {
                        let vp: zx_packet_guest_vcpu_t = read_packet(&pkt);
                        match vp.r#type as u8 {
                            ZX_PKT_GUEST_VCPU_INTERRUPT => {
                                let mask = vp.word0;
                                let vec = (vp.word1 & 0xFF) as u32;
                                for cpu in 0..usize::from(state.max_vcpus) {
                                    if (mask & (1u64 << cpu)) != 0 {
                                        state.interrupt_vcpu(cpu, vec);
                                    }
                                }
                            }
                            ZX_PKT_GUEST_VCPU_STARTUP => {
                                let target = vp.word0 as usize;
                                let entry = vp.word1 as usize;
                                if target >= 1 && target < usize::from(state.max_vcpus) {
                                    let mut aps = state.ap_threads.lock().unwrap();
                                    if aps.len() <= target {
                                        aps.resize_with(target + 1, || None);
                                    }
                                    if aps[target].is_none() {
                                        let ready = Arc::new(AtomicBool::new(false));
                                        let ready_c = Arc::clone(&ready);
                                        let state_c = Arc::clone(&state);
                                        let jh = thread::spawn(move || {
                                            let mut ap_vcpu = ZX_HANDLE_INVALID;
                                            let res = unsafe {
                                                zx_vcpu_create(state_c.guest, 0, entry, &mut ap_vcpu)
                                            };
                                            if res != ZX_OK {
                                                println!(
                                                    "[hirsel-vmm] AP[{target}] zx_vcpu_create(entry={entry:#x}) failed: {res}"
                                                );
                                                ready_c.store(true, Ordering::Release);
                                                return;
                                            }
                                            state_c.register_vcpu(target, ap_vcpu);
                                            ready_c.store(true, Ordering::Release);
                                            run_vcpu_loop(ap_vcpu, target, state_c);
                                            unsafe { zx_handle_close(ap_vcpu) };
                                        });
                                        aps[target] = Some(jh);
                                        drop(aps);
                                        // Wait briefly for deterministic VPID ordering & handle registration.
                                        for _ in 0..200 {
                                            if ready.load(Ordering::Acquire) {
                                                break;
                                            }
                                            let next = unsafe { zx_clock_get_monotonic() }
                                                .saturating_add(100_000);
                                            unsafe { zx_nanosleep(next) };
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    ZX_PKT_TYPE_GUEST_IO => {
                        let n_io = state.io_exits.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Some(ctrl) = state.managed_ctrl.as_deref() {
                            ctrl.io_exits.store(n_io, Ordering::Relaxed);
                        }
                        let io: zx_packet_guest_io_t = read_packet(&pkt);
                        let is_in = io.input != 0;
                        let mut reply: Option<zx_vcpu_io_t> = None;
                        let mut should_stop = false;

                        {
                            let mut devs = state.devs.lock().unwrap();
                            match io.port {
                                PORT_COM1_BASE..=0x03FF => {
                                    let reg = io.port - PORT_COM1_BASE;
                                    if is_in {
                                        let b = devs.uart.read(reg);
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: io.access_size,
                                            _pad: [0; 3],
                                            data: [b, 0, 0, 0],
                                        });
                                    } else {
                                        let d = &mut *devs;
                                        d.uart.write(
                                            reg,
                                            io.data[0],
                                            &mut d.stdout,
                                            state.managed_ctrl.as_deref(),
                                        );
                                    }
                                }
                                0x0020 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.pic_master.read_cmd(), 0, 0, 0],
                                        });
                                    } else {
                                        devs.legacy.pic_master.write_cmd(io.data[0]);
                                    }
                                }
                                0x0021 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.pic_master.read_data(), 0, 0, 0],
                                        });
                                    } else {
                                        devs.legacy.pic_master.write_data(io.data[0]);
                                    }
                                }
                                0x00A0 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.pic_slave.read_cmd(), 0, 0, 0],
                                        });
                                    } else {
                                        devs.legacy.pic_slave.write_cmd(io.data[0]);
                                    }
                                }
                                0x00A1 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.pic_slave.read_data(), 0, 0, 0],
                                        });
                                    } else {
                                        devs.legacy.pic_slave.write_data(io.data[0]);
                                    }
                                }
                                0x0060 => {
                                    if is_in {
                                        let v = if devs.legacy.i8042_cmd == 0xAA { 0x55 } else { 0x00 };
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [v, 0, 0, 0],
                                        });
                                    }
                                }
                                0x0064 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [0x01, 0, 0, 0],
                                        });
                                    } else {
                                        devs.legacy.i8042_cmd = io.data[0];
                                        if io.data[0] == 0xFE {
                                            devs.exit_code.get_or_insert(0);
                                            should_stop = true;
                                        }
                                    }
                                }
                                0x0061 => {
                                    if is_in {
                                        devs.legacy.port61 ^= 0x20;
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.port61, 0, 0, 0],
                                        });
                                    } else {
                                        devs.legacy.port61 =
                                            (devs.legacy.port61 & 0xF0) | (io.data[0] & 0x0F);
                                    }
                                }
                                0x0070 => {
                                    if !is_in {
                                        devs.legacy.cmos_idx = io.data[0] & 0x7F;
                                    } else {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.cmos_idx, 0, 0, 0],
                                        });
                                    }
                                }
                                0x0071 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [devs.legacy.read_cmos(), 0, 0, 0],
                                        });
                                    }
                                }
                                0x0040..=0x0043 => {
                                    if is_in {
                                        devs.legacy.pit_tick =
                                            devs.legacy.pit_tick.wrapping_sub(0x101);
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 1,
                                            _pad: [0; 3],
                                            data: [(devs.legacy.pit_tick & 0xFF) as u8, 0, 0, 0],
                                        });
                                    }
                                }
                                0x0600..=0x0607 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: io.access_size,
                                            _pad: [0; 3],
                                            data: [0; 4],
                                        });
                                    }
                                }
                                0x0CF9 => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: io.access_size,
                                            _pad: [0; 3],
                                            data: [0; 4],
                                        });
                                    } else if (io.data[0] & 0x04) != 0 {
                                        devs.exit_code.get_or_insert(0);
                                        should_stop = true;
                                    }
                                }
                                PORT_PV_EXIT => {
                                    if !is_in {
                                        let code = i32::from_le_bytes(io.data);
                                        devs.exit_code.get_or_insert(code);
                                        should_stop = true;
                                    }
                                }
                                PORT_PV_MONO_LO => {
                                    if is_in {
                                        devs.latched_mono =
                                            unsafe { zx_clock_get_monotonic() } as u64;
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 4,
                                            _pad: [0; 3],
                                            data: (devs.latched_mono as u32).to_le_bytes(),
                                        });
                                    }
                                }
                                PORT_PV_MONO_HI => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 4,
                                            _pad: [0; 3],
                                            data: ((devs.latched_mono >> 32) as u32).to_le_bytes(),
                                        });
                                    }
                                }
                                PORT_PV_RNG => {
                                    if is_in {
                                        let mut rng = [0u8; 4];
                                        unsafe { zx_cprng_draw(rng.as_mut_ptr(), rng.len()) };
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: 4,
                                            _pad: [0; 3],
                                            data: rng,
                                        });
                                    }
                                }
                                _ => {
                                    if is_in {
                                        reply = Some(zx_vcpu_io_t {
                                            access_size: io.access_size,
                                            _pad: [0; 3],
                                            data: [0xff; 4],
                                        });
                                    }
                                }
                            }
                            if !should_stop {
                                state.deliver_pic_irqs(&mut devs, vcpu, apic_id);
                            }
                        }

                        if should_stop {
                            state.signal_stop();
                            break;
                        }
                        if let Some(io_reply) = reply {
                            let st = unsafe {
                                zx_vcpu_write_state(
                                    vcpu,
                                    ZX_VCPU_IO,
                                    ptr::from_ref(&io_reply).cast(),
                                    size_of::<zx_vcpu_io_t>(),
                                )
                            };
                            if st != ZX_OK {
                                state.devs.lock().unwrap().loop_err.get_or_insert_with(|| {
                                    format!("zx_vcpu_write_state(ZX_VCPU_IO): zx_status={st}")
                                });
                                state.signal_stop();
                                break;
                            }
                        }
                    }
                    ZX_PKT_TYPE_GUEST_MEM => {
                        let n_mmio = state.mmio_exits.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Some(ctrl) = state.managed_ctrl.as_deref() {
                            ctrl.mmio_exits.store(n_mmio, Ordering::Relaxed);
                        }
                        let mem: zx_packet_guest_mem_t = read_packet(&pkt);
                        let is_virtio = (pkt.key == TRAP_KEY_VIRTIO_NET
                            && mem.addr >= GPA_VIRTIO_NET_MMIO
                            && mem.addr < GPA_VIRTIO_NET_MMIO + PAGE_SIZE as u64)
                            || (pkt.key == TRAP_KEY_VIRTIO_BLK
                                && mem.addr >= GPA_VIRTIO_BLK_MMIO
                                && mem.addr < GPA_VIRTIO_BLK_MMIO + PAGE_SIZE as u64)
                            || (pkt.key == TRAP_KEY_VIRTIO_CON_MMIO
                                && mem.addr >= GPA_VIRTIO_CON_MMIO
                                && mem.addr < GPA_VIRTIO_CON_MMIO + PAGE_SIZE as u64);

                        if is_virtio {
                            let mut inst_buf = [0u8; 15];
                            let n =
                                fetch_guest_inst_bytes(ram_slice, mem.cr3, mem.rip, &mut inst_buf);
                            match decode_mmio_inst(&inst_buf[..n]) {
                                Ok(access) => {
                                    let mut regs = zx_vcpu_state_t::default();
                                    let _ = unsafe {
                                        zx_vcpu_read_state(
                                            vcpu,
                                            ZX_VCPU_STATE,
                                            ptr::from_mut(&mut regs).cast(),
                                            size_of::<zx_vcpu_state_t>(),
                                        )
                                    };
                                    let mut need_write_regs = false;
                                    {
                                        let mut devs = state.devs.lock().unwrap();
                                        let val_opt: Option<u32> = match pkt.key {
                                            TRAP_KEY_VIRTIO_NET => {
                                                let off = mem.addr - GPA_VIRTIO_NET_MMIO;
                                                match (devs.virtio_net.as_mut(), access) {
                                                    (Some(v), MmioAccess::Read { size, .. }) => {
                                                        Some(v.read(off, size))
                                                    }
                                                    (Some(v), MmioAccess::WriteReg { reg, .. }) => {
                                                        v.write(
                                                            off,
                                                            get_gpr(&regs, reg) as u32,
                                                            ram_slice,
                                                        );
                                                        None
                                                    }
                                                    (
                                                        Some(v),
                                                        MmioAccess::WriteImm { value, .. },
                                                    ) => {
                                                        v.write(off, value as u32, ram_slice);
                                                        None
                                                    }
                                                    (None, MmioAccess::Read { .. }) => Some(0),
                                                    (None, _) => None,
                                                }
                                            }
                                            TRAP_KEY_VIRTIO_BLK => {
                                                let off = mem.addr - GPA_VIRTIO_BLK_MMIO;
                                                let res = match (devs.virtio_blk.as_mut(), access) {
                                                    (Some(v), MmioAccess::Read { size, .. }) => {
                                                        Some(v.read(off, size))
                                                    }
                                                    (Some(v), MmioAccess::WriteReg { reg, .. }) => {
                                                        v.write(
                                                            off,
                                                            get_gpr(&regs, reg) as u32,
                                                            ram_slice,
                                                        );
                                                        None
                                                    }
                                                    (
                                                        Some(v),
                                                        MmioAccess::WriteImm { value, .. },
                                                    ) => {
                                                        v.write(off, value as u32, ram_slice);
                                                        None
                                                    }
                                                    (None, MmioAccess::Read { .. }) => Some(0),
                                                    (None, _) => None,
                                                };
                                                if let (Some(ctrl), Some(v)) = (
                                                    state.managed_ctrl.as_deref(),
                                                    devs.virtio_blk.as_ref(),
                                                ) {
                                                    ctrl.blk_reads.store(
                                                        v.reads.load(Ordering::Relaxed),
                                                        Ordering::Relaxed,
                                                    );
                                                    ctrl.blk_writes.store(
                                                        v.writes.load(Ordering::Relaxed),
                                                        Ordering::Relaxed,
                                                    );
                                                }
                                                res
                                            }
                                            TRAP_KEY_VIRTIO_CON_MMIO => {
                                                let off = mem.addr - GPA_VIRTIO_CON_MMIO;
                                                match (devs.virtio_con.as_mut(), access) {
                                                    (Some(v), MmioAccess::Read { size, .. }) => {
                                                        Some(v.read(off, size))
                                                    }
                                                    (Some(v), MmioAccess::WriteReg { reg, .. }) => {
                                                        v.write(
                                                            off,
                                                            get_gpr(&regs, reg) as u32,
                                                            ram_slice,
                                                        );
                                                        None
                                                    }
                                                    (
                                                        Some(v),
                                                        MmioAccess::WriteImm { value, .. },
                                                    ) => {
                                                        v.write(off, value as u32, ram_slice);
                                                        None
                                                    }
                                                    (None, MmioAccess::Read { .. }) => Some(0),
                                                    (None, _) => None,
                                                }
                                            }
                                            _ => None,
                                        };
                                        if let (
                                            Some(val),
                                            MmioAccess::Read {
                                                reg,
                                                size,
                                                zero_extend,
                                            },
                                        ) = (val_opt, access)
                                        {
                                            set_gpr(&mut regs, reg, size, zero_extend, u64::from(val));
                                            need_write_regs = true;
                                        }
                                        state.deliver_pic_irqs(&mut devs, vcpu, apic_id);
                                    }
                                    if need_write_regs {
                                        let _ = unsafe {
                                            zx_vcpu_write_state(
                                                vcpu,
                                                ZX_VCPU_STATE,
                                                ptr::from_ref(&regs).cast(),
                                                size_of::<zx_vcpu_state_t>(),
                                            )
                                        };
                                    }
                                }
                                Err(e) => {
                                    state.devs.lock().unwrap().loop_err.get_or_insert_with(|| {
                                        format!(
                                            "virtio MMIO decode error at addr={:#x} rip={:#x} bytes={:02x?}: {e}",
                                            mem.addr,
                                            mem.rip,
                                            &inst_buf[..n.min(8)]
                                        )
                                    });
                                    state.signal_stop();
                                    break;
                                }
                            }
                            continue;
                        }

                        let in_hb = pkt.key == TRAP_KEY_MMIO_HEARTBEAT
                            && mem.addr >= GPA_MMIO_HEARTBEAT
                            && mem.addr < GPA_MMIO_HEARTBEAT + PAGE_SIZE as u64;
                        let in_con = pkt.key == TRAP_KEY_VIRTIO_CON
                            && mem.addr >= GPA_VIRTIO_CON
                            && mem.addr < GPA_VIRTIO_CON + PAGE_SIZE as u64;
                        if !in_hb && !in_con {
                            state.devs.lock().unwrap().loop_err.get_or_insert_with(|| {
                                format!(
                                    "unexpected GUEST_MEM trap addr={:#x} rip={:#x}",
                                    mem.addr, mem.rip
                                )
                            });
                            state.signal_stop();
                            break;
                        }
                    }
                    other => {
                        state.devs.lock().unwrap().loop_err.get_or_insert_with(|| {
                            format!("unhandled guest packet type={other}")
                        });
                        state.signal_stop();
                        break;
                    }
                }
            }
        }

        let vcpu_res = thread::spawn(move || -> Result<VmRunReport, String> {
            let mut vcpu = ZX_HANDLE_INVALID;
            check(
                unsafe { zx_vcpu_create(guest, 0, spec.entry_gpa as usize, &mut vcpu) },
                "zx_vcpu_create",
            )?;

            let initial_state = zx_vcpu_state_t {
                rsp: spec.rsp,
                rdi: spec.rdi,
                rsi: spec.rsi,
                rflags: 0x2,
                ..Default::default()
            };
            let init_res = check(
                unsafe {
                    zx_vcpu_write_state(
                        vcpu,
                        ZX_VCPU_STATE,
                        ptr::from_ref(&initial_state).cast(),
                        size_of::<zx_vcpu_state_t>(),
                    )
                },
                "zx_vcpu_write_state(init)",
            );
            if let Err(e) = init_res {
                unsafe { zx_handle_close(vcpu) };
                return Err(e);
            }

            let managed_ctrl = managed.as_ref().map(|m| Arc::clone(&m.ctrl));
            let virtio_net = managed.as_ref().and_then(|m| {
                m.ctrl.net_channel.as_ref().map(|ch| {
                    let mac = m.mac.unwrap_or([0x02, 0x48, 0x52, 0x53, 0x4c, 0x01]);
                    VirtioNetMmio::new(mac, Arc::clone(ch))
                })
            });
            let virtio_blk = managed
                .as_ref()
                .and_then(|m| m.blk_backend.as_ref().map(|b| VirtioBlkMmio::new(Arc::clone(b))));
            let virtio_con = managed
                .as_ref()
                .map(|m| VirtioConsoleMmio::new(Arc::clone(&m.ctrl.console_channel)));

            let state = Arc::new(SharedVmState {
                guest,
                host_ram_base,
                ram_bytes_len,
                trap_all_io: spec.trap_all_io,
                max_exits,
                max_vcpus,
                managed_ctrl: managed_ctrl.clone(),
                stop_all: AtomicBool::new(false),
                io_exits: AtomicU64::new(0),
                mmio_exits: AtomicU64::new(0),
                total_exits: AtomicU64::new(0),
                devs: Mutex::new(SharedDevices {
                    stdout: Vec::new(),
                    uart: Uart16550::new(),
                    legacy: PcLegacyIo::new(),
                    virtio_net,
                    virtio_blk,
                    virtio_con,
                    latched_mono: 0,
                    exit_code: None,
                    loop_err: None,
                }),
                local_vcpu_dups: Mutex::new(Vec::new()),
                ap_threads: Mutex::new(Vec::new()),
            });

            state.register_vcpu(0, vcpu);
            if let Some(ctrl) = managed_ctrl.as_ref() {
                if let Some(ch) = ctrl.net_channel.as_ref() {
                    let ctrl_w = Arc::clone(ctrl);
                    ch.set_wake_guest(Some(Box::new(move || ctrl_w.kick())));
                }
                {
                    let ctrl_w = Arc::clone(ctrl);
                    ctrl.console_channel
                        .set_wake_guest(Some(Box::new(move || ctrl_w.kick())));
                }
            }

            // Watchdog for one-shot Linux kernel boots only (managed VMs run persistently).
            let done_flag = Arc::new(AtomicBool::new(false));
            let mut watchdog = if spec.trap_all_io && managed_ctrl.is_none() {
                let done_c = Arc::clone(&done_flag);
                let mut vcpu_dup = ZX_HANDLE_INVALID;
                let _ = unsafe { zx_handle_duplicate(vcpu, ZX_RIGHT_SAME_RIGHTS, &mut vcpu_dup) };
                Some(thread::spawn(move || {
                    for _ in 0..120 {
                        if done_c.load(Ordering::Relaxed) {
                            unsafe { zx_handle_close(vcpu_dup) };
                            return;
                        }
                        let next = unsafe { zx_clock_get_monotonic() }.saturating_add(100_000_000);
                        unsafe { zx_nanosleep(next) };
                    }
                    if !done_c.load(Ordering::Relaxed) {
                        println!("[hirsel-vmm] watchdog kicking VCPU after 12s timeout");
                        unsafe { zx_vcpu_kick(vcpu_dup) };
                    }
                    unsafe { zx_handle_close(vcpu_dup) };
                }))
            } else {
                None
            };

            run_vcpu_loop(vcpu, 0, Arc::clone(&state));

            // Stop and join all spawned SMP AP threads before closing guest handles!
            state.signal_stop();
            let aps: Vec<_> = state
                .ap_threads
                .lock()
                .unwrap()
                .drain(..)
                .flatten()
                .collect();
            for jh in aps {
                let _ = jh.join();
            }

            done_flag.store(true, Ordering::Relaxed);
            if let Some(w) = watchdog.take() {
                let _ = w.join();
            }
            if let Some(ctrl) = managed_ctrl.as_ref() {
                if let Some(ch) = ctrl.net_channel.as_ref() {
                    ch.set_wake_guest(None);
                }
                ctrl.console_channel.set_wake_guest(None);
                ctrl.clear_vcpu_dups();
            }
            for h in state.local_vcpu_dups.lock().unwrap().drain(..) {
                if h != ZX_HANDLE_INVALID {
                    unsafe { zx_handle_close(h) };
                }
            }
            unsafe { zx_handle_close(vcpu) };

            let io_exits = state.io_exits.load(Ordering::Relaxed);
            let mmio_exits = state.mmio_exits.load(Ordering::Relaxed);
            let mut devs = state.devs.lock().unwrap();
            if let Some(err) = devs.loop_err.take() {
                return Err(err);
            }
            let code = devs
                .exit_code
                .ok_or_else(|| format!("guest exceeded max_vm_exits ({max_exits})"))?;
            let stdout = core::mem::take(&mut devs.stdout);
            drop(devs);

            let mut bell_count: u64 = 0;
            loop {
                let mut bpkt = zx_port_packet_t::default();
                let st = unsafe { zx_port_wait(bell_port, 0, &mut bpkt) };
                if st == ZX_OK && bpkt.packet_type == ZX_PKT_TYPE_GUEST_BELL {
                    bell_count += 1;
                } else {
                    break;
                }
            }

            let elapsed_us = ((unsafe { zx_clock_get_monotonic() } - t0) / 1000) as u64;
            Ok(VmRunReport {
                exit_code: code,
                stdout,
                io_exits,
                mmio_exits,
                bell_count,
                elapsed_us,
            })
        })
        .join()
        .map_err(|_| "VCPU thread panicked".to_string())?;

        // 6. Teardown guest handles & unmap host RAM mirror cleanly.
        unsafe {
            zx_handle_close(bell_port);
            zx_vmar_unmap(host_root_vmar, host_ram_base, config.ram_bytes);
            zx_handle_close(ram_vmo);
            zx_handle_close(guest_vmar);
            zx_handle_close(guest);
        }

        vcpu_res
    }
}

#[cfg(target_os = "fuchsia")]
pub use fuchsia_vmm::{
    run_elf_microvm, run_linux_kernel, run_linux_user_elf, run_managed_linux_vm, run_raw_microvm,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_page_tables_map_low_and_mmio() {
        let mut ram = vec![0u8; 0x8000];
        init_identity_page_tables(&mut ram).unwrap();
        let pml4_0 = u64::from_le_bytes(ram[0..8].try_into().unwrap());
        assert_eq!(pml4_0, GPA_PDPT | 0x3);
        let pdpt_0 = u64::from_le_bytes(ram[0x1000..0x1008].try_into().unwrap());
        assert_eq!(pdpt_0, GPA_PD_LOW | 0x3);
        let pdpt_3 = u64::from_le_bytes(ram[0x1018..0x1020].try_into().unwrap());
        assert_eq!(pdpt_3, GPA_PD_HIGH | 0x3);
        let mmio_pde = u64::from_le_bytes(
            ram[GPA_PD_HIGH as usize + 128 * 8..GPA_PD_HIGH as usize + 129 * 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(mmio_pde, GPA_MMIO_HEARTBEAT | 0x83);
    }

    #[test]
    fn boot_info_layout() {
        assert_eq!(size_of::<GuestBootInfo>(), 80);
    }
}
