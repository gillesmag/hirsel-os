//! Pure-Rust Zircon Boot Image (ZBI) reader and minimal packer.
//!
//! Reference: Zircon `<lib/zbi-format/zbi.h>` (`zbi_header_t`, 32 bytes, 8-byte aligned items).

use std::fmt;

pub const ZBI_TYPE_CONTAINER: u32 = 0x544f_4f42; // "BOOT"
pub const ZBI_TYPE_KERNEL_X64: u32 = 0x4c4e_524b; // "KRNL"
pub const ZBI_TYPE_KERNEL_ARM64: u32 = 0x384e_524b; // "KRN8"
pub const ZBI_TYPE_CMDLINE: u32 = 0x4c44_4d43; // "CMDL"
pub const ZBI_TYPE_STORAGE_BOOTFS: u32 = 0x4253_4642; // "BFSB"
pub const ZBI_TYPE_STORAGE_RAMDISK: u32 = 0x4b53_4452; // "RDSK"
pub const ZBI_TYPE_DISCARD: u32 = 0x5049_4b53; // "SKIP"

pub const ZBI_CONTAINER_MAGIC: u32 = 0x868c_f7e6;
pub const ZBI_ITEM_MAGIC: u32 = 0xb578_1729;
pub const ZBI_FLAGS_VERSION: u32 = 0x0001_0000;
pub const ZBI_FLAGS_CRC32: u32 = 0x0002_0000;
pub const ZBI_ITEM_NO_CRC32: u32 = 0x4a87_e8d6;
pub const ZBI_ALIGNMENT: usize = 8;
pub const ZBI_HEADER_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZbiHeader {
    pub item_type: u32,
    pub length: u32,
    pub extra: u32,
    pub flags: u32,
    pub reserved0: u32,
    pub reserved1: u32,
    pub magic: u32,
    pub crc32: u32,
}

impl ZbiHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < ZBI_HEADER_SIZE {
            return Err(format!(
                "truncated ZBI header: need {ZBI_HEADER_SIZE} bytes, got {}",
                bytes.len()
            ));
        }
        let u32_at = |off: usize| -> u32 {
            u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
        };
        Ok(Self {
            item_type: u32_at(0),
            length: u32_at(4),
            extra: u32_at(8),
            flags: u32_at(12),
            reserved0: u32_at(16),
            reserved1: u32_at(20),
            magic: u32_at(24),
            crc32: u32_at(28),
        })
    }

    pub fn to_bytes(self) -> [u8; ZBI_HEADER_SIZE] {
        let mut out = [0u8; ZBI_HEADER_SIZE];
        out[0..4].copy_from_slice(&self.item_type.to_le_bytes());
        out[4..8].copy_from_slice(&self.length.to_le_bytes());
        out[8..12].copy_from_slice(&self.extra.to_le_bytes());
        out[12..16].copy_from_slice(&self.flags.to_le_bytes());
        out[16..20].copy_from_slice(&self.reserved0.to_le_bytes());
        out[20..24].copy_from_slice(&self.reserved1.to_le_bytes());
        out[24..28].copy_from_slice(&self.magic.to_le_bytes());
        out[28..32].copy_from_slice(&self.crc32.to_le_bytes());
        out
    }

    pub fn container(payload_len: u32) -> Self {
        Self {
            item_type: ZBI_TYPE_CONTAINER,
            length: payload_len,
            extra: ZBI_CONTAINER_MAGIC,
            flags: ZBI_FLAGS_VERSION,
            reserved0: 0,
            reserved1: 0,
            magic: ZBI_ITEM_MAGIC,
            crc32: ZBI_ITEM_NO_CRC32,
        }
    }

    pub fn item(item_type: u32, payload_len: u32, extra: u32) -> Self {
        Self {
            item_type,
            length: payload_len,
            extra,
            flags: ZBI_FLAGS_VERSION,
            reserved0: 0,
            reserved1: 0,
            magic: ZBI_ITEM_MAGIC,
            crc32: ZBI_ITEM_NO_CRC32,
        }
    }

    pub fn type_name(self) -> String {
        match self.item_type {
            ZBI_TYPE_CONTAINER => "CONTAINER".to_owned(),
            ZBI_TYPE_KERNEL_X64 => "KERNEL_X64".to_owned(),
            ZBI_TYPE_KERNEL_ARM64 => "KERNEL_ARM64".to_owned(),
            ZBI_TYPE_CMDLINE => "CMDLINE".to_owned(),
            ZBI_TYPE_STORAGE_BOOTFS => "STORAGE_BOOTFS".to_owned(),
            ZBI_TYPE_STORAGE_RAMDISK => "STORAGE_RAMDISK".to_owned(),
            ZBI_TYPE_DISCARD => "DISCARD".to_owned(),
            raw => {
                let bytes = raw.to_le_bytes();
                if bytes.iter().all(|b| b.is_ascii_graphic()) {
                    format!("0x{raw:08x} ({})", std::str::from_utf8(&bytes).unwrap())
                } else {
                    format!("0x{raw:08x}")
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZbiItem {
    pub header: ZbiHeader,
    pub payload: Vec<u8>,
}

impl fmt::Display for ZbiItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<16} len={:<10} extra=0x{:08x} flags=0x{:08x}",
            self.header.type_name(),
            self.header.length,
            self.header.extra,
            self.header.flags
        )
    }
}

pub fn align_up(n: usize) -> usize {
    (n + (ZBI_ALIGNMENT - 1)) & !(ZBI_ALIGNMENT - 1)
}

pub fn parse_container(bytes: &[u8]) -> Result<Vec<ZbiItem>, String> {
    let container = ZbiHeader::parse(bytes)?;
    if container.item_type != ZBI_TYPE_CONTAINER {
        return Err(format!(
            "not a ZBI container: type=0x{:08x}",
            container.item_type
        ));
    }
    if container.extra != ZBI_CONTAINER_MAGIC || container.magic != ZBI_ITEM_MAGIC {
        return Err("invalid ZBI container magic".to_owned());
    }
    let total_end = ZBI_HEADER_SIZE
        .checked_add(container.length as usize)
        .ok_or_else(|| "ZBI container length overflow".to_owned())?;
    if total_end > bytes.len() {
        return Err(format!(
            "ZBI container claims {total_end} bytes, file has {}",
            bytes.len()
        ));
    }

    let mut items = Vec::new();
    let mut offset = ZBI_HEADER_SIZE;
    while offset < total_end {
        let header = ZbiHeader::parse(&bytes[offset..total_end])?;
        if header.magic != ZBI_ITEM_MAGIC {
            return Err(format!("invalid ZBI item magic at offset 0x{offset:x}"));
        }
        let payload_start = offset + ZBI_HEADER_SIZE;
        let payload_end = payload_start
            .checked_add(header.length as usize)
            .ok_or_else(|| "ZBI item length overflow".to_owned())?;
        if payload_end > total_end {
            return Err(format!("truncated ZBI item at offset 0x{offset:x}"));
        }
        items.push(ZbiItem {
            header,
            payload: bytes[payload_start..payload_end].to_vec(),
        });
        offset = payload_start + align_up(header.length as usize);
    }
    Ok(items)
}

pub fn serialize_container(items: &[ZbiItem]) -> Vec<u8> {
    let mut body = Vec::new();
    for item in items {
        let mut header = item.header;
        header.length = item.payload.len() as u32;
        header.magic = ZBI_ITEM_MAGIC;
        header.flags = (header.flags & !ZBI_FLAGS_CRC32) | ZBI_FLAGS_VERSION;
        header.crc32 = ZBI_ITEM_NO_CRC32;
        body.extend_from_slice(&header.to_bytes());
        body.extend_from_slice(&item.payload);
        let pad = align_up(item.payload.len()) - item.payload.len();
        body.resize(body.len() + pad, 0);
    }
    let mut out = Vec::with_capacity(ZBI_HEADER_SIZE + body.len());
    out.extend_from_slice(&ZbiHeader::container(body.len() as u32).to_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn cmdline_item(cmdline: &str) -> ZbiItem {
    let mut payload = cmdline.as_bytes().to_vec();
    if !payload.ends_with(&[0]) {
        payload.push(0);
    }
    ZbiItem {
        header: ZbiHeader::item(ZBI_TYPE_CMDLINE, payload.len() as u32, 0),
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_container_and_cmdline() {
        let items = vec![
            ZbiItem {
                header: ZbiHeader::item(ZBI_TYPE_KERNEL_X64, 5, 0),
                payload: vec![1, 2, 3, 4, 5],
            },
            cmdline_item("userboot.next=bin/hirsel-init"),
        ];
        let encoded = serialize_container(&items);
        let decoded = parse_container(&encoded).expect("valid ZBI");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].payload, vec![1, 2, 3, 4, 5]);
        assert_eq!(
            decoded[1].payload,
            b"userboot.next=bin/hirsel-init\0".to_vec()
        );
    }
}
