//! Linux x86-64 Boot Protocol (v2.15+) zero-page (`boot_params`), E820 memory map,
//! command line, initramfs, and `bzImage` setup-header parser for Tier 2 MicroVMs.
//!
//! Reference: Linux `arch/x86/include/uapi/asm/bootparam.h` and `Documentation/arch/x86/boot.rst`.

use core::mem::size_of;

pub const GPA_ZERO_PAGE: u64 = 0x0000_7000;
pub const GPA_CMDLINE: u64 = 0x0002_0000;
pub const GPA_ACPI_RSDP: u64 = 0x000E_0000;
pub const GPA_ACPI_XSDT: u64 = 0x000E_0040;
pub const GPA_ACPI_FADT: u64 = 0x000E_0100;
pub const GPA_ACPI_DSDT: u64 = 0x000E_0280;
pub const GPA_ACPI_MADT: u64 = 0x000E_0600;
pub const GPA_KERNEL_DEFAULT: u64 = 0x0100_0000; // 16 MiB standard 64-bit load address

pub const BOOT_FLAG_MAGIC: u16 = 0xAA55;
pub const SETUP_HDR_MAGIC: u32 = 0x5372_6448; // "HdrS"
pub const LOADER_TYPE_UNDEFINED: u8 = 0xFF;

pub const E820_RAM: u32 = 1;
pub const E820_RESERVED: u32 = 2;

/// Single E820 memory map entry (`struct boot_e820_entry`, 20 bytes packed).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct E820Entry {
    pub addr: u64,
    pub size: u64,
    pub entry_type: u32,
}

/// Subset of Linux `struct setup_header` starting at byte offset `0x1f1` in `boot_params`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BzImageInfo {
    pub setup_sects: u8,
    pub protocol_version: u16,
    pub payload_offset: usize,
    pub pref_address: u64,
    pub code64_entry: u64,
    pub initrd_addr_max: u32,
}

/// Exact byte offsets inside the 4096-byte Linux `struct boot_params`.
pub mod offsets {
    pub const ACPI_RSDP_ADDR: usize = 0x070;
    pub const E820_ENTRIES: usize = 0x1e8;
    pub const SETUP_SECTS: usize = 0x1f1;
    pub const BOOT_FLAG: usize = 0x1fe;
    pub const HEADER_MAGIC: usize = 0x202;
    pub const PROTOCOL_VERSION: usize = 0x206;
    pub const TYPE_OF_LOADER: usize = 0x210;
    pub const LOADFLAGS: usize = 0x211;
    pub const CODE32_START: usize = 0x214;
    pub const RAMDISK_IMAGE: usize = 0x218;
    pub const RAMDISK_SIZE: usize = 0x21c;
    pub const CMD_LINE_PTR: usize = 0x228;
    pub const INITRD_ADDR_MAX: usize = 0x22c;
    pub const KERNEL_ALIGNMENT: usize = 0x230;
    pub const RELOCATABLE_KERNEL: usize = 0x234;
    pub const XLOADFLAGS: usize = 0x236;
    pub const CMDLINE_SIZE: usize = 0x238;
    pub const PREF_ADDRESS: usize = 0x258;
    pub const E820_TABLE: usize = 0x2d0;
    pub const BOOT_PARAMS_SIZE: usize = 0x1000;
}

/// Parses and validates a Linux x86-64 `bzImage` setup header.
pub fn parse_bzimage(image: &[u8]) -> Result<BzImageInfo, String> {
    if image.len() < 0x268 {
        return Err("bzImage too small for setup header".into());
    }
    let boot_flag = u16::from_le_bytes(image[offsets::BOOT_FLAG..offsets::BOOT_FLAG + 2].try_into().unwrap());
    if boot_flag != BOOT_FLAG_MAGIC {
        return Err(format!("invalid bzImage boot_flag {boot_flag:#06x} (expected 0xAA55)"));
    }
    let magic = u32::from_le_bytes(image[offsets::HEADER_MAGIC..offsets::HEADER_MAGIC + 4].try_into().unwrap());
    if magic != SETUP_HDR_MAGIC {
        return Err(format!("invalid bzImage header magic {magic:#010x} (expected HdrS)"));
    }
    let setup_sects = match image[offsets::SETUP_SECTS] {
        0 => 4,
        n => n,
    };
    let protocol_version = u16::from_le_bytes(
        image[offsets::PROTOCOL_VERSION..offsets::PROTOCOL_VERSION + 2]
            .try_into()
            .unwrap(),
    );
    let payload_offset = ((setup_sects as usize) + 1) * 512;
    if payload_offset >= image.len() {
        return Err("bzImage setup_sects exceeds image size".into());
    }
    let pref_address = if protocol_version >= 0x020a {
        let addr = u64::from_le_bytes(
            image[offsets::PREF_ADDRESS..offsets::PREF_ADDRESS + 8]
                .try_into()
                .unwrap(),
        );
        if addr == 0 { GPA_KERNEL_DEFAULT } else { addr }
    } else {
        GPA_KERNEL_DEFAULT
    };
    let initrd_addr_max = u32::from_le_bytes(
        image[offsets::INITRD_ADDR_MAX..offsets::INITRD_ADDR_MAX + 4]
            .try_into()
            .unwrap(),
    );
    Ok(BzImageInfo {
        setup_sects,
        protocol_version,
        payload_offset,
        pref_address,
        // Linux 64-bit entry point (`startup_64`) is always +0x200 from protected-mode start.
        code64_entry: pref_address + 0x200,
        initrd_addr_max,
    })
}

/// Populates a Linux x86-64 `boot_params` zero-page at `GPA_ZERO_PAGE`, command line
/// at `GPA_CMDLINE`, E820 RAM map, minimal ACPI 2.0 tables (with `vcpus` CPUs in MADT
/// and Virtio-MMIO DSDT devices), and optional initramfs inside `ram`.
pub fn install_linux_boot_params(
    ram: &mut [u8],
    cmdline: &str,
    initrd: Option<&[u8]>,
    bzimage_setup_hdr: Option<&[u8]>,
) -> Result<(), String> {
    install_linux_boot_params_smp(ram, cmdline, initrd, bzimage_setup_hdr, 1)
}

pub fn install_linux_boot_params_smp(
    ram: &mut [u8],
    cmdline: &str,
    initrd: Option<&[u8]>,
    bzimage_setup_hdr: Option<&[u8]>,
    vcpus: u8,
) -> Result<(), String> {
    let zp = GPA_ZERO_PAGE as usize;
    let cmd_gpa = GPA_CMDLINE as usize;
    if ram.len() < cmd_gpa + cmdline.len() + 1 {
        return Err("guest RAM too small for Linux boot_params and cmdline".into());
    }
    ram[zp..zp + offsets::BOOT_PARAMS_SIZE].fill(0);

    // Copy original setup_header from bzImage if provided.
    if let Some(img) = bzimage_setup_hdr {
        let copy_end = img.len().min(0x290);
        if copy_end > offsets::SETUP_SECTS {
            ram[zp + offsets::SETUP_SECTS..zp + copy_end]
                .copy_from_slice(&img[offsets::SETUP_SECTS..copy_end]);
        }
    } else {
        ram[zp + offsets::BOOT_FLAG..zp + offsets::BOOT_FLAG + 2]
            .copy_from_slice(&BOOT_FLAG_MAGIC.to_le_bytes());
        ram[zp + offsets::HEADER_MAGIC..zp + offsets::HEADER_MAGIC + 4]
            .copy_from_slice(&SETUP_HDR_MAGIC.to_le_bytes());
        ram[zp + offsets::PROTOCOL_VERSION..zp + offsets::PROTOCOL_VERSION + 2]
            .copy_from_slice(&0x020fu16.to_le_bytes());
    }

    // Bootloader identity & flags (LOADED_HIGH = bit 0, CAN_USE_HEAP = bit 7).
    ram[zp + offsets::TYPE_OF_LOADER] = LOADER_TYPE_UNDEFINED;
    ram[zp + offsets::LOADFLAGS] |= 0x81;
    // Allow initrd anywhere in low 2 GiB when booting direct vmlinux ELF.
    if bzimage_setup_hdr.is_none() {
        ram[zp + offsets::INITRD_ADDR_MAX..zp + offsets::INITRD_ADDR_MAX + 4]
            .copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
    }

    // Write NUL-terminated kernel command line.
    ram[cmd_gpa..cmd_gpa + cmdline.len()].copy_from_slice(cmdline.as_bytes());
    ram[cmd_gpa + cmdline.len()] = 0;
    ram[zp + offsets::CMD_LINE_PTR..zp + offsets::CMD_LINE_PTR + 4]
        .copy_from_slice(&(GPA_CMDLINE as u32).to_le_bytes());

    // Place optional initramfs near the top of RAM (page-aligned).
    if let Some(rd) = initrd {
        let aligned_len = (rd.len() + 0xfff) & !0xfff;
        if ram.len() < aligned_len + 0x20_0000 {
            return Err("guest RAM too small for initramfs".into());
        }
        let rd_gpa = ram.len() - aligned_len;
        ram[rd_gpa..rd_gpa + rd.len()].copy_from_slice(rd);
        ram[zp + offsets::RAMDISK_IMAGE..zp + offsets::RAMDISK_IMAGE + 4]
            .copy_from_slice(&(rd_gpa as u32).to_le_bytes());
        ram[zp + offsets::RAMDISK_SIZE..zp + offsets::RAMDISK_SIZE + 4]
            .copy_from_slice(&(rd.len() as u32).to_le_bytes());
    }

    // Standard E820 memory map:
    //   [0x0000_0000 .. 0x0009_FC00)  Usable low RAM
    //   [0x0009_FC00 .. 0x0010_0000)  Reserved EBDA/BIOS hole
    //   [0x0010_0000 .. ram.len())    Usable extended RAM
    let entries = [
        E820Entry {
            addr: 0,
            size: 0x0009_FC00,
            entry_type: E820_RAM,
        },
        E820Entry {
            addr: 0x0009_FC00,
            size: 0x0006_0400,
            entry_type: E820_RESERVED,
        },
        E820Entry {
            addr: 0x0010_0000,
            size: (ram.len() as u64).saturating_sub(0x0010_0000),
            entry_type: E820_RAM,
        },
    ];
    ram[zp + offsets::E820_ENTRIES] = entries.len() as u8;
    for (i, e) in entries.iter().enumerate() {
        let dst = zp + offsets::E820_TABLE + i * size_of::<E820Entry>();
        let addr = e.addr;
        let size = e.size;
        let ty = e.entry_type;
        ram[dst..dst + 8].copy_from_slice(&addr.to_le_bytes());
        ram[dst + 8..dst + 16].copy_from_slice(&size.to_le_bytes());
        ram[dst + 16..dst + 20].copy_from_slice(&ty.to_le_bytes());
    }
    // Install minimal ACPI 2.0 tables (RSDP, XSDT, FADT, DSDT with VNET/VBLK/VCON, N-CPU MADT)
    // in the reserved BIOS ROM region [0x000E_0000 .. 0x000E_1000) and point boot_params
    // `acpi_rsdp_addr` directly at RSDP so Linux discovers all vCPUs & Virtio-MMIO devices cleanly.
    install_minimal_acpi(ram, vcpus.clamp(1, 8))?;
    ram[zp + offsets::ACPI_RSDP_ADDR..zp + offsets::ACPI_RSDP_ADDR + 8]
        .copy_from_slice(&GPA_ACPI_RSDP.to_le_bytes());

    Ok(())
}

fn acpi_checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0u8.wrapping_sub(sum)
}

fn write_acpi_header(buf: &mut [u8], sig: &[u8; 4], rev: u8) {
    let len = buf.len() as u32;
    buf[0..4].copy_from_slice(sig);
    buf[4..8].copy_from_slice(&len.to_le_bytes());
    buf[8] = rev;
    buf[9] = 0; // checksum filled after body
    buf[10..16].copy_from_slice(b"HIRSEL");
    buf[16..24].copy_from_slice(b"HIRSELVM");
    buf[24..28].copy_from_slice(&1u32.to_le_bytes()); // OEM revision
    buf[28..32].copy_from_slice(b"HRSL"); // Creator ID
    buf[32..36].copy_from_slice(&1u32.to_le_bytes()); // Creator revision
}

fn aml_pkg_len(body_len: usize) -> Vec<u8> {
    if body_len + 1 < 64 {
        vec![(body_len + 1) as u8]
    } else {
        let total = body_len + 2;
        vec![0x40 | ((total & 0x0f) as u8), ((total >> 4) & 0xff) as u8]
    }
}

fn build_virtio_mmio_aml_device(name: &[u8; 4], uid: u8, base_gpa: u32, irq: u32) -> Vec<u8> {
    let mut crs_data = Vec::new();
    // Memory32Fixed (0x86, len=9, RW=1, base_gpa, size=0x1000)
    crs_data.extend_from_slice(&[0x86, 0x09, 0x00, 0x01]);
    crs_data.extend_from_slice(&base_gpa.to_le_bytes());
    crs_data.extend_from_slice(&0x0000_1000u32.to_le_bytes());
    // Extended Interrupt Descriptor (0x89, len=6, Consumer|Edge|ActiveHigh|Exclusive, count=1, gsi=irq)
    crs_data.extend_from_slice(&[0x89, 0x06, 0x00, 0x03, 0x01]);
    crs_data.extend_from_slice(&irq.to_le_bytes());
    // EndTag
    crs_data.extend_from_slice(&[0x79, 0x00]);

    let mut crs_buf = Vec::new();
    crs_buf.push(0x11); // BufferOp
    let mut buf_inner = Vec::new();
    buf_inner.push(0x0A); // BytePrefix
    buf_inner.push(crs_data.len() as u8);
    buf_inner.extend_from_slice(&crs_data);
    crs_buf.extend_from_slice(&aml_pkg_len(buf_inner.len()));
    crs_buf.extend_from_slice(&buf_inner);

    let mut dev_body = Vec::new();
    dev_body.extend_from_slice(name);
    // Name (_HID, "LNRO0005")
    dev_body.push(0x08);
    dev_body.extend_from_slice(b"_HID");
    dev_body.push(0x0D);
    dev_body.extend_from_slice(b"LNRO0005\0");
    // Name (_UID, uid)
    dev_body.push(0x08);
    dev_body.extend_from_slice(b"_UID");
    if uid == 0 {
        dev_body.push(0x00);
    } else if uid == 1 {
        dev_body.push(0x01);
    } else {
        dev_body.push(0x0A);
        dev_body.push(uid);
    }
    // Name (_CRS, Buffer(...))
    dev_body.push(0x08);
    dev_body.extend_from_slice(b"_CRS");
    dev_body.extend_from_slice(&crs_buf);

    let mut dev = Vec::new();
    dev.extend_from_slice(&[0x5B, 0x82]); // DeviceOp
    dev.extend_from_slice(&aml_pkg_len(dev_body.len()));
    dev.extend_from_slice(&dev_body);
    dev
}

fn build_virtio_devices_dsdt_aml() -> Vec<u8> {
    let mut scope_body = Vec::new();
    scope_body.extend_from_slice(b"\\_SB_");
    // VNET: virtio-net at 0xD000_0000, IRQ 5
    scope_body.extend_from_slice(&build_virtio_mmio_aml_device(b"VNET", 0, 0xD000_0000, 5));
    // VBLK: virtio-blk at 0xD000_1000, IRQ 6
    scope_body.extend_from_slice(&build_virtio_mmio_aml_device(b"VBLK", 1, 0xD000_1000, 6));
    // VCON: virtio-console at 0xD000_2000, IRQ 3
    scope_body.extend_from_slice(&build_virtio_mmio_aml_device(b"VCON", 2, 0xD000_2000, 3));

    let mut scope = Vec::new();
    scope.push(0x10); // ScopeOp
    scope.extend_from_slice(&aml_pkg_len(scope_body.len()));
    scope.extend_from_slice(&scope_body);
    scope
}

fn install_minimal_acpi(ram: &mut [u8], vcpus: u8) -> Result<(), String> {
    if ram.len() < 0x000F_0000 {
        return Err("guest RAM too small for ACPI tables".into());
    }
    ram[GPA_ACPI_RSDP as usize..GPA_ACPI_RSDP as usize + 0x1000].fill(0);

    // 1. DSDT: 36-byte ACPI header + AML declaring `\_SB_.{VNET, VBLK, VCON}` (`LNRO0005`).
    {
        let aml = build_virtio_devices_dsdt_aml();
        let total = 36 + aml.len();
        let dsdt = &mut ram[GPA_ACPI_DSDT as usize..GPA_ACPI_DSDT as usize + total];
        write_acpi_header(dsdt, b"DSDT", 2);
        dsdt[36..total].copy_from_slice(&aml);
        dsdt[9] = acpi_checksum(dsdt);
    }

    // 2. FADT ("FACP", ACPI 6.x, 276 bytes, force physical x2APIC + i8042 reset + PM1a stub)
    {
        let fadt = &mut ram[GPA_ACPI_FADT as usize..GPA_ACPI_FADT as usize + 276];
        write_acpi_header(fadt, b"FACP", 6);
        fadt[40..44].copy_from_slice(&(GPA_ACPI_DSDT as u32).to_le_bytes()); // DSDT
        fadt[46..48].copy_from_slice(&9u16.to_le_bytes()); // SCI_INT = 9
        fadt[56..60].copy_from_slice(&0x0600u32.to_le_bytes()); // PM1a_EVT_BLK
        fadt[64..68].copy_from_slice(&0x0604u32.to_le_bytes()); // PM1a_CNT_BLK
        fadt[88] = 4; // PM1_EVT_LEN
        fadt[89] = 2; // PM1_CNT_LEN
        // Flags: RESET_REG_SUP (1<<10) | FORCE_APIC_PHYSICAL (1<<19)
        // Leave HW_REDUCED_ACPI unset so Linux keeps the legacy 8259A PIC active for COM1 IRQ 4.
        let flags: u32 = (1 << 10) | (1 << 19);
        fadt[112..116].copy_from_slice(&flags.to_le_bytes());
        // RESET_REG Generic Address Structure (offset 116, 12 bytes): System I/O 0x64, 8-bit
        fadt[116] = 1; // Address Space ID: System I/O
        fadt[117] = 8; // Register Bit Width
        fadt[118] = 0; // Register Bit Offset
        fadt[119] = 1; // Access Size: Byte
        fadt[120..128].copy_from_slice(&0x0064u64.to_le_bytes());
        fadt[128] = 0xFE; // RESET_VALUE
        // X_DSDT (offset 140)
        fadt[140..148].copy_from_slice(&GPA_ACPI_DSDT.to_le_bytes());
        // X_PM1a_EVT_BLK (offset 148, 12-byte GAS): System I/O 0x600, 32-bit
        fadt[148] = 1;
        fadt[149] = 32;
        fadt[151] = 2;
        fadt[152..160].copy_from_slice(&0x0600u64.to_le_bytes());
        // X_PM1a_CNT_BLK (offset 172, 12-byte GAS): System I/O 0x604, 16-bit
        fadt[172] = 1;
        fadt[173] = 16;
        fadt[175] = 2;
        fadt[176..184].copy_from_slice(&0x0604u64.to_le_bytes());
        fadt[9] = acpi_checksum(fadt);
    }

    // 3. MADT ("APIC", 44-byte header + N x 8-byte Processor Local APIC entries for CPUs 0..vcpus)
    {
        let madt_len = 44 + usize::from(vcpus) * 8;
        let madt = &mut ram[GPA_ACPI_MADT as usize..GPA_ACPI_MADT as usize + madt_len];
        write_acpi_header(madt, b"APIC", 4);
        madt[36..40].copy_from_slice(&0xFEE0_0000u32.to_le_bytes()); // Local Interrupt Controller Address
        madt[40..44].copy_from_slice(&1u32.to_le_bytes()); // Flags: PCAT_COMPAT (dual-8259 present)
        for cpu in 0..vcpus {
            let off = 44 + usize::from(cpu) * 8;
            madt[off] = 0; // Type 0: Processor Local APIC
            madt[off + 1] = 8; // Length
            madt[off + 2] = cpu; // ACPI Processor UID
            madt[off + 3] = cpu; // APIC ID
            madt[off + 4..off + 8].copy_from_slice(&1u32.to_le_bytes()); // Flags: Enabled
        }
        madt[9] = acpi_checksum(madt);
    }

    // 4. XSDT (36-byte header + 2 * 8-byte pointers to FADT and MADT)
    {
        let xsdt = &mut ram[GPA_ACPI_XSDT as usize..GPA_ACPI_XSDT as usize + 52];
        write_acpi_header(xsdt, b"XSDT", 1);
        xsdt[36..44].copy_from_slice(&GPA_ACPI_FADT.to_le_bytes());
        xsdt[44..52].copy_from_slice(&GPA_ACPI_MADT.to_le_bytes());
        xsdt[9] = acpi_checksum(xsdt);
    }

    // 5. RSDP v2 (36 bytes at 0x000E_0000)
    {
        let rsdp = &mut ram[GPA_ACPI_RSDP as usize..GPA_ACPI_RSDP as usize + 36];
        rsdp[0..8].copy_from_slice(b"RSD PTR ");
        rsdp[9..15].copy_from_slice(b"HIRSEL");
        rsdp[15] = 2; // ACPI 2.0+
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&GPA_ACPI_XSDT.to_le_bytes());
        rsdp[8] = acpi_checksum(&rsdp[0..20]);
        rsdp[32] = acpi_checksum(&rsdp[0..36]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e820_entry_is_20_bytes_and_zero_page_populates() {
        assert_eq!(size_of::<E820Entry>(), 20);
        let mut ram = vec![0u8; 4 * 1024 * 1024];
        install_linux_boot_params(
            &mut ram,
            "console=ttyS0 reboot=k panic=1",
            Some(b"mock-initramfs-cpio"),
            None,
        )
        .unwrap();
        let zp = GPA_ZERO_PAGE as usize;
        assert_eq!(ram[zp + offsets::E820_ENTRIES], 3);
        assert_eq!(ram[zp + offsets::TYPE_OF_LOADER], 0xFF);
        let cmd_ptr = u32::from_le_bytes(
            ram[zp + offsets::CMD_LINE_PTR..zp + offsets::CMD_LINE_PTR + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(cmd_ptr, GPA_CMDLINE as u32);
        assert!(ram[cmd_ptr as usize..].starts_with(b"console=ttyS0"));
        let rd_size = u32::from_le_bytes(
            ram[zp + offsets::RAMDISK_SIZE..zp + offsets::RAMDISK_SIZE + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(rd_size as usize, b"mock-initramfs-cpio".len());
        let rsdp_ptr = u64::from_le_bytes(
            ram[zp + offsets::ACPI_RSDP_ADDR..zp + offsets::ACPI_RSDP_ADDR + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(rsdp_ptr, GPA_ACPI_RSDP);
        // Verify every ACPI table checksum sums to 0 mod 256.
        let sum = |addr: u64, len: usize| -> u8 {
            ram[addr as usize..addr as usize + len]
                .iter()
                .fold(0u8, |a, &b| a.wrapping_add(b))
        };
        assert_eq!(sum(GPA_ACPI_RSDP, 20), 0);
        assert_eq!(sum(GPA_ACPI_RSDP, 36), 0);
        assert_eq!(sum(GPA_ACPI_XSDT, 52), 0);
        assert_eq!(sum(GPA_ACPI_FADT, 276), 0);
        let dsdt_len = u32::from_le_bytes(
            ram[GPA_ACPI_DSDT as usize + 4..GPA_ACPI_DSDT as usize + 8]
                .try_into()
                .unwrap(),
        ) as usize;
        assert!(dsdt_len > 36);
        assert_eq!(sum(GPA_ACPI_DSDT, dsdt_len), 0);
        assert_eq!(sum(GPA_ACPI_MADT, 52), 0);
    }

    #[test]
    fn parse_synthetic_bzimage_header() {
        let mut img = vec![0u8; 4096];
        img[offsets::SETUP_SECTS] = 2; // payload starts at (2 + 1) * 512 = 1536
        img[offsets::BOOT_FLAG..offsets::BOOT_FLAG + 2]
            .copy_from_slice(&BOOT_FLAG_MAGIC.to_le_bytes());
        img[offsets::HEADER_MAGIC..offsets::HEADER_MAGIC + 4]
            .copy_from_slice(&SETUP_HDR_MAGIC.to_le_bytes());
        img[offsets::PROTOCOL_VERSION..offsets::PROTOCOL_VERSION + 2]
            .copy_from_slice(&0x020fu16.to_le_bytes());
        img[offsets::PREF_ADDRESS..offsets::PREF_ADDRESS + 8]
            .copy_from_slice(&0x0100_0000u64.to_le_bytes());
        let info = parse_bzimage(&img).unwrap();
        assert_eq!(info.setup_sects, 2);
        assert_eq!(info.payload_offset, 1536);
        assert_eq!(info.code64_entry, 0x0100_0200);
    }
}
