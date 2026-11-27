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
pub mod virtio_net_mmio;

use core::mem::size_of;
use core::ptr;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    stop_requested: AtomicBool,
    pub io_exits: AtomicU64,
    pub mmio_exits: AtomicU64,
    #[cfg(target_os = "fuchsia")]
    vcpu_dup: Mutex<zx_sys::zx_handle_t>,
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
            stop_requested: AtomicBool::new(false),
            io_exits: AtomicU64::new(0),
            mmio_exits: AtomicU64::new(0),
            #[cfg(target_os = "fuchsia")]
            vcpu_dup: Mutex::new(zx_sys::ZX_HANDLE_INVALID),
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
        self.kick();
    }

    #[cfg(target_os = "fuchsia")]
    pub fn kick(&self) {
        let h = *self.vcpu_dup.lock().unwrap();
        if h != zx_sys::ZX_HANDLE_INVALID {
            unsafe { zx_sys::zx_vcpu_kick(h) };
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    pub fn kick(&self) {}

    #[cfg(target_os = "fuchsia")]
    fn set_vcpu_dup(&self, handle: zx_sys::zx_handle_t) {
        let mut guard = self.vcpu_dup.lock().unwrap();
        if *guard != zx_sys::ZX_HANDLE_INVALID {
            unsafe { zx_sys::zx_handle_close(*guard) };
        }
        *guard = handle;
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
/// populates Linux `boot_params` @ `0x7000`, cmdline @ `0x20000`, E820 map, and optional
/// `initramfs`, returning the 64-bit `startup_64` entry GPA.
pub fn load_linux_kernel_into_ram(
    kernel: &[u8],
    initrd: Option<&[u8]>,
    cmdline: &str,
    ram: &mut [u8],
) -> Result<u64, String> {
    if kernel.len() >= 4 && &kernel[0..4] == b"\x7fELF" {
        let entry = load_elf_into_ram(kernel, ram)?;
        linux_boot::install_linux_boot_params(ram, cmdline, initrd, None)?;
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
        linux_boot::install_linux_boot_params(ram, cmdline, initrd, Some(kernel))?;
        Ok(bz.code64_entry)
    }
}

#[cfg(target_os = "fuchsia")]
mod fuchsia_vmm {
    use super::*;
    use crate::mmio::{MmioAccess, decode_mmio_inst, fetch_guest_inst_bytes, get_gpr, set_gpr};
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

    struct LaunchSpec {
        entry_gpa: u64,
        rsp: u64,
        rdi: u64,
        rsi: u64,
        trap_all_io: bool,
    }

    struct ManagedLaunch {
        ctrl: Arc<ManagedVmControl>,
        mac: Option<[u8; 6]>,
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

    /// Runs a persistent, operator-managed upstream Linux x86-64 MicroVM with live
    /// console streaming/input, optional Virtio-MMIO `virtio-net` networking (IRQ 5),
    /// and clean remote stop via `ManagedVmControl`.
    #[allow(clippy::too_many_arguments)]
    pub fn run_managed_linux_vm(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        kernel_bytes: &[u8],
        initrd_bytes: Option<&[u8]>,
        cmdline: &str,
        ram_bytes: usize,
        mac: Option<[u8; 6]>,
        ctrl: Arc<ManagedVmControl>,
    ) -> Result<VmRunReport, String> {
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
            Some(ManagedLaunch { ctrl, mac }),
            |ram, _| {
                let entry_gpa =
                    load_linux_kernel_into_ram(kernel_bytes, initrd_bytes, cmdline_eff, ram)?;
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

        // 5. Run the VCPU on a dedicated thread (Zircon binds each VCPU to its creator thread).
        let max_exits = config.max_vm_exits;
        let ram_bytes_len = config.ram_bytes;
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
            let mut virtio_net = managed.as_ref().and_then(|m| {
                m.ctrl.net_channel.as_ref().map(|ch| {
                    let mac = m.mac.unwrap_or([0x02, 0x48, 0x52, 0x53, 0x4c, 0x01]);
                    VirtioNetMmio::new(mac, Arc::clone(ch))
                })
            });

            if let Some(ctrl) = managed_ctrl.as_ref() {
                let mut vcpu_dup = ZX_HANDLE_INVALID;
                if unsafe { zx_handle_duplicate(vcpu, ZX_RIGHT_SAME_RIGHTS, &mut vcpu_dup) }
                    == ZX_OK
                {
                    ctrl.set_vcpu_dup(vcpu_dup);
                }
                if let Some(ch) = ctrl.net_channel.as_ref() {
                    let ctrl_w = Arc::clone(ctrl);
                    ch.set_wake_guest(Some(Box::new(move || ctrl_w.kick())));
                }
            }

            let ram_slice = unsafe {
                core::slice::from_raw_parts_mut(host_ram_base as *mut u8, ram_bytes_len)
            };

            let mut stdout = Vec::new();
            let mut uart = Uart16550::new();
            let mut legacy = PcLegacyIo::new();
            let mut io_exits: u64 = 0;
            let mut mmio_exits: u64 = 0;
            let mut latched_mono: u64 = 0;
            let mut exit_code: Option<i32> = None;

            // Watchdog for one-shot Linux kernel boots only (managed VMs run persistently).
            let done_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut watchdog = if spec.trap_all_io && managed_ctrl.is_none() {
                let done_c = std::sync::Arc::clone(&done_flag);
                let mut vcpu_dup = ZX_HANDLE_INVALID;
                let _ = unsafe { zx_handle_duplicate(vcpu, ZX_RIGHT_SAME_RIGHTS, &mut vcpu_dup) };
                Some(thread::spawn(move || {
                    for _ in 0..120 {
                        if done_c.load(std::sync::atomic::Ordering::Relaxed) {
                            unsafe { zx_handle_close(vcpu_dup) };
                            return;
                        }
                        let next = unsafe { zx_clock_get_monotonic() }.saturating_add(100_000_000);
                        unsafe { zx_nanosleep(next) };
                    }
                    if !done_c.load(std::sync::atomic::Ordering::Relaxed) {
                        println!("[hirsel-vmm] watchdog kicking VCPU after 12s timeout");
                        unsafe { zx_vcpu_kick(vcpu_dup) };
                    }
                    unsafe { zx_handle_close(vcpu_dup) };
                }))
            } else {
                None
            };

            let mut loop_err: Option<String> = None;
            let mut total_exits: u64 = 0;
            while total_exits < max_exits {
                // Before entering the VCPU, deliver any host-queued console input or Virtio-Net RX frames.
                if let Some(ctrl) = managed_ctrl.as_deref() {
                    if ctrl.is_stop_requested() {
                        exit_code = Some(0);
                        break;
                    }
                    if ctrl.drain_console_in(&mut uart.rx_queue) && uart.irq_pending() {
                        if let Some(vec) = legacy.pic_master.try_deliver(4) {
                            let _ = unsafe { zx_vcpu_interrupt(vcpu, vec) };
                        }
                    }
                }
                if let Some(vnet) = virtio_net.as_mut() {
                    if vnet.process_rx(ram_slice) && vnet.irq_pending() {
                        if let Some(vec) = legacy.pic_master.try_deliver(VIRTIO_NET_IRQ) {
                            let _ = unsafe { zx_vcpu_interrupt(vcpu, vec) };
                        }
                    }
                }

                let mut pkt = zx_port_packet_t::default();
                let st = unsafe { zx_vcpu_enter(vcpu, &mut pkt) };
                if st == ZX_ERR_CANCELED {
                    if let Some(ctrl) = managed_ctrl.as_deref() {
                        if ctrl.is_stop_requested() {
                            exit_code = Some(0);
                            break;
                        }
                        // Wakeup kick for incoming Virtio-Net RX packet or console stdin!
                        continue;
                    }
                }
                if st != ZX_OK {
                    if spec.trap_all_io && st == ZX_ERR_BAD_STATE && (!stdout.is_empty() || managed_ctrl.is_some()) {
                        // Triple-fault / VMEXIT_SHUTDOWN during Linux reboot fallback.
                        exit_code = Some(0);
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
                    loop_err = Some(format!(
                        "zx_vcpu_enter exited with zx_status={st} (io_exits={io_exits}, mmio_exits={mmio_exits}, rsp={:#x}, rax={:#x}, rflags={:#x})",
                        st_regs.rsp,
                        st_regs.rax,
                        st_regs.rflags
                    ));
                    break;
                }
                total_exits = total_exits.saturating_add(1);

                match pkt.packet_type {
                    ZX_PKT_TYPE_GUEST_IO => {
                        io_exits += 1;
                        if let Some(ctrl) = managed_ctrl.as_deref() {
                            ctrl.io_exits.store(io_exits, Ordering::Relaxed);
                        }
                        let io: zx_packet_guest_io_t = read_packet(&pkt);
                        let is_in = io.input != 0;
                        let mut reply: Option<zx_vcpu_io_t> = None;

                        match io.port {
                            // COM1 16550A UART (0x3F8..0x400)
                            PORT_COM1_BASE..=0x03FF => {
                                let reg = io.port - PORT_COM1_BASE;
                                if is_in {
                                    let b = uart.read(reg);
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: io.access_size,
                                        _pad: [0; 3],
                                        data: [b, 0, 0, 0],
                                    });
                                } else {
                                    uart.write(reg, io.data[0], &mut stdout, managed_ctrl.as_deref());
                                }
                            }
                            // Dual 8259A PIC (Master 0x20/0x21, Slave 0xA0/0xA1)
                            0x0020 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.pic_master.read_cmd(), 0, 0, 0],
                                    });
                                } else {
                                    legacy.pic_master.write_cmd(io.data[0]);
                                }
                            }
                            0x0021 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.pic_master.read_data(), 0, 0, 0],
                                    });
                                } else {
                                    legacy.pic_master.write_data(io.data[0]);
                                }
                            }
                            0x00A0 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.pic_slave.read_cmd(), 0, 0, 0],
                                    });
                                } else {
                                    legacy.pic_slave.write_cmd(io.data[0]);
                                }
                            }
                            0x00A1 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.pic_slave.read_data(), 0, 0, 0],
                                    });
                                } else {
                                    legacy.pic_slave.write_data(io.data[0]);
                                }
                            }
                            // i8042 Keyboard Controller (0x60/0x64) -> `reboot=k` writes 0xFE to 0x64!
                            0x0060 => {
                                if is_in {
                                    let v = if legacy.i8042_cmd == 0xAA { 0x55 } else { 0x00 };
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
                                        data: [0x01, 0, 0, 0], // output buffer full
                                    });
                                } else {
                                    legacy.i8042_cmd = io.data[0];
                                    if io.data[0] == 0xFE {
                                        // Clean Linux `reboot=k` microVM shutdown!
                                        exit_code = Some(0);
                                        break;
                                    }
                                }
                            }
                            // Port 0x61 (NMI Status / PIT Ch2 gate): toggle bit 5 so calibration finishes!
                            0x0061 => {
                                if is_in {
                                    legacy.port61 ^= 0x20;
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.port61, 0, 0, 0],
                                    });
                                } else {
                                    legacy.port61 = (legacy.port61 & 0xF0) | (io.data[0] & 0x0F);
                                }
                            }
                            // MC146818 CMOS/RTC (0x70/0x71)
                            0x0070 => {
                                if !is_in {
                                    legacy.cmos_idx = io.data[0] & 0x7F;
                                } else {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.cmos_idx, 0, 0, 0],
                                    });
                                }
                            }
                            0x0071 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [legacy.read_cmos(), 0, 0, 0],
                                    });
                                }
                            }
                            // Legacy 8254 PIT (0x40..0x43)
                            0x0040..=0x0043 => {
                                if is_in {
                                    legacy.pit_tick = legacy.pit_tick.wrapping_sub(0x101);
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [(legacy.pit_tick & 0xFF) as u8, 0, 0, 0],
                                    });
                                }
                            }
                            // Minimal ACPI PM1a status/control stub (0x600..0x608)
                            0x0600..=0x0607 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: io.access_size,
                                        _pad: [0; 3],
                                        data: [0; 4],
                                    });
                                }
                            }
                            // ICH/PIIX Reset Control Register (0xCF9): bit 2 = system reset.
                            0x0CF9 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: io.access_size,
                                        _pad: [0; 3],
                                        data: [0; 4],
                                    });
                                } else if (io.data[0] & 0x04) != 0 {
                                    exit_code = Some(0);
                                    break;
                                }
                            }
                            // Paravirt clean VM exit
                            PORT_PV_EXIT => {
                                if !is_in {
                                    let code = i32::from_le_bytes(io.data);
                                    exit_code = Some(code);
                                    break;
                                }
                            }
                            // Paravirt host monotonic clock (low 32 bits, latches 64-bit ns)
                            PORT_PV_MONO_LO => {
                                if is_in {
                                    latched_mono = unsafe { zx_clock_get_monotonic() } as u64;
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 4,
                                        _pad: [0; 3],
                                        data: (latched_mono as u32).to_le_bytes(),
                                    });
                                }
                            }
                            // Paravirt host monotonic clock (high 32 bits of latched ns)
                            PORT_PV_MONO_HI => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 4,
                                        _pad: [0; 3],
                                        data: ((latched_mono >> 32) as u32).to_le_bytes(),
                                    });
                                }
                            }
                            // Paravirt host CSPRNG entropy (32 bits per read)
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
                            // All other legacy ports (PIC 0x20/0xA0, PCI 0xCF8/0xCFC, COM2..4, etc.)
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

                        if let Some(io_reply) = reply {
                            check(
                                unsafe {
                                    zx_vcpu_write_state(
                                        vcpu,
                                        ZX_VCPU_IO,
                                        ptr::from_ref(&io_reply).cast(),
                                        size_of::<zx_vcpu_io_t>(),
                                    )
                                },
                                "zx_vcpu_write_state(ZX_VCPU_IO)",
                            )?;
                        }
                        // Deliver pending COM1 IRQ 4 and Virtio-Net IRQ 5 via 8259A master PIC.
                        if spec.trap_all_io {
                            if uart.irq_pending() {
                                if let Some(vec) = legacy.pic_master.try_deliver(4) {
                                    let _ = unsafe { zx_vcpu_interrupt(vcpu, vec) };
                                }
                            }
                            if let Some(vnet) = virtio_net.as_ref() {
                                if vnet.irq_pending() {
                                    if let Some(vec) = legacy.pic_master.try_deliver(VIRTIO_NET_IRQ) {
                                        let _ = unsafe { zx_vcpu_interrupt(vcpu, vec) };
                                    }
                                }
                            }
                        }
                    }
                    ZX_PKT_TYPE_GUEST_MEM => {
                        mmio_exits += 1;
                        if let Some(ctrl) = managed_ctrl.as_deref() {
                            ctrl.mmio_exits.store(mmio_exits, Ordering::Relaxed);
                        }
                        let mem: zx_packet_guest_mem_t = read_packet(&pkt);
                        if pkt.key == TRAP_KEY_VIRTIO_NET
                            && mem.addr >= GPA_VIRTIO_NET_MMIO
                            && mem.addr < GPA_VIRTIO_NET_MMIO + PAGE_SIZE as u64
                        {
                            let mut inst_buf = [0u8; 15];
                            let n = fetch_guest_inst_bytes(ram_slice, mem.cr3, mem.rip, &mut inst_buf);
                            match decode_mmio_inst(&inst_buf[..n]) {
                                Ok(access) => {
                                    let off = mem.addr - GPA_VIRTIO_NET_MMIO;
                                    let mut regs = zx_vcpu_state_t::default();
                                    let _ = unsafe {
                                        zx_vcpu_read_state(
                                            vcpu,
                                            ZX_VCPU_STATE,
                                            ptr::from_mut(&mut regs).cast(),
                                            size_of::<zx_vcpu_state_t>(),
                                        )
                                    };
                                    if let Some(vnet) = virtio_net.as_mut() {
                                        match access {
                                            MmioAccess::Read {
                                                reg,
                                                size,
                                                zero_extend,
                                            } => {
                                                let val = u64::from(vnet.read(off, size));
                                                set_gpr(&mut regs, reg, size, zero_extend, val);
                                                let _ = unsafe {
                                                    zx_vcpu_write_state(
                                                        vcpu,
                                                        ZX_VCPU_STATE,
                                                        ptr::from_ref(&regs).cast(),
                                                        size_of::<zx_vcpu_state_t>(),
                                                    )
                                                };
                                            }
                                            MmioAccess::WriteReg { reg, .. } => {
                                                let val = get_gpr(&regs, reg) as u32;
                                                vnet.write(off, val, ram_slice);
                                            }
                                            MmioAccess::WriteImm { value, .. } => {
                                                vnet.write(off, value as u32, ram_slice);
                                            }
                                        }
                                        if vnet.irq_pending() {
                                            if let Some(vec) =
                                                legacy.pic_master.try_deliver(VIRTIO_NET_IRQ)
                                            {
                                                let _ = unsafe { zx_vcpu_interrupt(vcpu, vec) };
                                            }
                                        }
                                    } else if let MmioAccess::Read {
                                        reg,
                                        size,
                                        zero_extend,
                                    } = access
                                    {
                                        // No virtio-net attached: return 0 (invalid magic) so Linux skips probe.
                                        set_gpr(&mut regs, reg, size, zero_extend, 0);
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
                                    loop_err = Some(format!(
                                        "virtio-net MMIO decode error at addr={:#x} rip={:#x} bytes={:02x?}: {e}",
                                        mem.addr,
                                        mem.rip,
                                        &inst_buf[..n.min(8)]
                                    ));
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
                            loop_err = Some(format!(
                                "unexpected GUEST_MEM trap addr={:#x} rip={:#x}",
                                mem.addr, mem.rip
                            ));
                            break;
                        }
                    }
                    other => {
                        loop_err = Some(format!("unhandled guest packet type={other}"));
                        break;
                    }
                }
            }

            done_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(w) = watchdog.take() {
                let _ = w.join();
            }
            if let Some(ctrl) = managed_ctrl.as_ref() {
                if let Some(ch) = ctrl.net_channel.as_ref() {
                    ch.set_wake_guest(None);
                }
                ctrl.set_vcpu_dup(ZX_HANDLE_INVALID);
            }
            unsafe { zx_handle_close(vcpu) };

            if let Some(err) = loop_err {
                return Err(err);
            }

            let code = exit_code
                .ok_or_else(|| format!("guest exceeded max_vm_exits ({max_exits})"))?;

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
