//! Tier 2 Hardware-Isolated MicroVM VMM for Hirsel-OS on Zircon.
//!
//! Uses Zircon's built-in kernel hypervisor (`zx_guest_create`, `zx_vcpu_create`,
//! `zx_vcpu_enter`, `zx_guest_set_trap`) backed by AMD SVM + NPT (and Intel VMX + EPT)
//! to execute freestanding 64-bit ELF payloads or raw machine-code workloads with
//! hardware memory isolation, virtual COM1 UART console capture, paravirt host
//! clock/CSPRNG hypercalls, and NPT MMIO/doorbell trapping.

pub mod linux_boot;

use core::mem::size_of;
use core::ptr;

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

    // Clear 4 page-table pages (0x0000..0x4000).
    ram[0..0x4000].fill(0);

    let write_u64 = |buf: &mut [u8], off: usize, val: u64| {
        buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
    };

    // PML4[0] -> PDPT at 0x1000
    write_u64(ram, GPA_PML4 as usize, GPA_PDPT | P | W);
    // PDPT[0] -> PD_LOW (0..1 GiB)
    write_u64(ram, GPA_PDPT as usize, GPA_PD_LOW | P | W);
    // PDPT[3] -> PD_HIGH (3..4 GiB, covers 0xD000_0000 MMIO window)
    write_u64(ram, GPA_PDPT as usize + 3 * 8, GPA_PD_HIGH | P | W);

    // Fill PD_LOW: 512 x 2 MiB identity pages covering 0x0000_0000..0x4000_0000.
    for i in 0..512u64 {
        let entry = (i << 21) | P | W | PS;
        write_u64(ram, GPA_PD_LOW as usize + (i as usize) * 8, entry);
    }
    // Fill PD_HIGH: 512 x 2 MiB identity pages covering 0xC000_0000..0x1_0000_0000.
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

/// Loads a static 64-bit x86-64 ELF image into `ram` and returns its entry GPA.
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
                "guest ELF PT_LOAD [{dst_addr:#x}, {:#x}) outside usable guest RAM",
                dst_addr.saturating_add(memsz)
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

#[cfg(target_os = "fuchsia")]
mod fuchsia_vmm {
    use super::*;
    use std::thread;
    use zx_sys::*;

    const TRAP_KEY_COM1: u64 = 1;
    const TRAP_KEY_PV: u64 = 2;
    const TRAP_KEY_MMIO_HEARTBEAT: u64 = 3;
    const TRAP_KEY_MMIO_BELL: u64 = 4;
    const TRAP_KEY_VIRTIO_CON: u64 = 5;

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
        run_microvm_inner(hypervisor_resource, vmex_resource, config, |ram, _| {
            load_elf_into_ram(elf_bytes, ram)
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
        run_microvm_inner(hypervisor_resource, vmex_resource, cfg, |ram, boot_info| {
            let aux_start = GPA_AUX_PAYLOAD as usize;
            let aux_end = aux_start
                .checked_add(linux_user_elf.len())
                .ok_or_else(|| "Linux ELF size overflow".to_string())?;
            // Aux staging lives in [2 MiB .. 4 MiB); user ELF loads at >= 4 MiB.
            if aux_end > 0x0040_0000 {
                return Err(format!(
                    "Linux static ELF ({} bytes) exceeds 2 MiB staging window",
                    linux_user_elf.len()
                ));
            }
            ram[aux_start..aux_end].copy_from_slice(linux_user_elf);
            boot_info.aux_gpa = GPA_AUX_PAYLOAD;
            boot_info.aux_bytes = linux_user_elf.len() as u64;
            // Also populate standard Linux boot_params zero-page at 0x7000.
            let _ = linux_boot::install_linux_boot_params(
                ram,
                "console=ttyS0 hirsel.tier=2",
                None,
                None,
            );
            load_elf_into_ram(shim_elf, ram)
        })
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
        run_microvm_inner(hypervisor_resource, vmex_resource, config, |ram, _| {
            let start = entry_gpa as usize;
            if start < 0x5000 || start + code.len() > ram.len() {
                return Err("raw guest code out of guest RAM bounds".into());
            }
            ram[start..start + code.len()].copy_from_slice(code);
            Ok(entry_gpa)
        })
    }

    fn run_microvm_inner<F>(
        hypervisor_resource: zx_handle_t,
        vmex_resource: zx_handle_t,
        config: MicroVmConfig,
        load_guest: F,
    ) -> Result<VmRunReport, String>
    where
        F: FnOnce(&mut [u8], &mut GuestBootInfo) -> Result<u64, String>,
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

        // 3. Populate 4-level identity page tables, GuestBootInfo, and guest ELF image.
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
                let entry = load_guest(ram_slice, &mut boot_info)?;
                unsafe {
                    ptr::write_unaligned(
                        ram_slice.as_mut_ptr().add(GPA_BOOT_INFO as usize).cast::<GuestBootInfo>(),
                        boot_info,
                    );
                }
                Ok(entry)
            })
        };

        let entry_gpa = match load_res {
            Ok(e) => e,
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

        // 4. Register hardware traps: COM1 I/O, Paravirt I/O, MMIO heartbeat, async MMIO bell.
        let mut bell_port = ZX_HANDLE_INVALID;
        let setup_traps = check(unsafe { zx_port_create(0, &mut bell_port) }, "zx_port_create")
            .and_then(|()| {
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
            })
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
        let ram_bytes = config.ram_bytes;
        let max_exits = config.max_vm_exits;
        let vcpu_res = thread::spawn(move || -> Result<VmRunReport, String> {
            let mut vcpu = ZX_HANDLE_INVALID;
            check(
                unsafe { zx_vcpu_create(guest, 0, entry_gpa as usize, &mut vcpu) },
                "zx_vcpu_create",
            )?;

            // Set initial 64-bit registers: RSP at top of guest RAM (16-byte aligned), RDI = GuestBootInfo.
            let initial_state = zx_vcpu_state_t {
                rsp: (ram_bytes as u64 - 0x1000) & !0xf,
                rdi: GPA_BOOT_INFO,
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

            let mut stdout = Vec::new();
            let mut io_exits: u64 = 0;
            let mut mmio_exits: u64 = 0;
            let mut latched_mono: u64 = 0;
            let mut exit_code: Option<i32> = None;

            for _ in 0..max_exits {
                let mut pkt = zx_port_packet_t::default();
                let st = unsafe { zx_vcpu_enter(vcpu, &mut pkt) };
                if st != ZX_OK {
                    unsafe { zx_handle_close(vcpu) };
                    return Err(format!("zx_vcpu_enter exited with zx_status={st}"));
                }

                match pkt.packet_type {
                    ZX_PKT_TYPE_GUEST_IO => {
                        io_exits += 1;
                        let io: zx_packet_guest_io_t = read_packet(&pkt);
                        let is_in = io.input != 0;
                        let mut reply: Option<zx_vcpu_io_t> = None;

                        match io.port {
                            // COM1 16550 UART data register (TX)
                            PORT_COM1_BASE => {
                                if !is_in {
                                    stdout.push(io.data[0]);
                                } else {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: io.access_size,
                                        _pad: [0; 3],
                                        data: [0; 4],
                                    });
                                }
                            }
                            // COM1 Line Status Register (0x3FD): THRE | TEMT always ready
                            p if p == PORT_COM1_BASE + 5 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: 1,
                                        _pad: [0; 3],
                                        data: [0x60, 0, 0, 0],
                                    });
                                }
                            }
                            // Other COM1 control registers (IER, FCR, LCR, MCR, scratch)
                            p if p > PORT_COM1_BASE && p < PORT_COM1_BASE + PORT_COM1_LEN as u16 => {
                                if is_in {
                                    reply = Some(zx_vcpu_io_t {
                                        access_size: io.access_size,
                                        _pad: [0; 3],
                                        data: [0; 4],
                                    });
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
                    }
                    ZX_PKT_TYPE_GUEST_MEM => {
                        mmio_exits += 1;
                        let mem: zx_packet_guest_mem_t = read_packet(&pkt);
                        let in_hb = pkt.key == TRAP_KEY_MMIO_HEARTBEAT
                            && mem.addr >= GPA_MMIO_HEARTBEAT
                            && mem.addr < GPA_MMIO_HEARTBEAT + PAGE_SIZE as u64;
                        let in_con = pkt.key == TRAP_KEY_VIRTIO_CON
                            && mem.addr >= GPA_VIRTIO_CON
                            && mem.addr < GPA_VIRTIO_CON + PAGE_SIZE as u64;
                        if !in_hb && !in_con {
                            unsafe { zx_handle_close(vcpu) };
                            return Err(format!(
                                "unexpected GUEST_MEM trap addr={:#x} rip={:#x}",
                                mem.addr, mem.rip
                            ));
                        }
                    }
                    other => {
                        unsafe { zx_handle_close(vcpu) };
                        return Err(format!("unhandled guest packet type={other}"));
                    }
                }
            }

            unsafe { zx_handle_close(vcpu) };

            let code = exit_code
                .ok_or_else(|| format!("guest exceeded max_vm_exits ({max_exits})"))?;

            // Drain any async MMIO doorbell packets queued on `bell_port`.
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
pub use fuchsia_vmm::{run_elf_microvm, run_linux_user_elf, run_raw_microvm};

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
        // Check 0xD000_0000 = 3 GiB + 256 MiB => PD_HIGH[128]
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
