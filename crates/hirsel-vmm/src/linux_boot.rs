//! Linux x86-64 Boot Protocol (v2.15+) zero-page (`boot_params`), E820 memory map,
//! command line, initramfs, and `bzImage` setup-header parser for Tier 2 MicroVMs.
//!
//! Reference: Linux `arch/x86/include/uapi/asm/bootparam.h` and `Documentation/arch/x86/boot.rst`.

use core::mem::size_of;

pub const GPA_ZERO_PAGE: u64 = 0x0000_7000;
pub const GPA_CMDLINE: u64 = 0x0002_0000;
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
/// at `GPA_CMDLINE`, E820 RAM map, and optional initramfs inside `ram`.
pub fn install_linux_boot_params(
    ram: &mut [u8],
    cmdline: &str,
    initrd: Option<&[u8]>,
    bzimage_setup_hdr: Option<&[u8]>,
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
