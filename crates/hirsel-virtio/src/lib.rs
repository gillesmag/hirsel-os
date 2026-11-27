//! Ring-3 Virtio 1.0 PCI drivers (`virtio-blk`, `virtio-net`), persistent
//! content-addressed image store (`HRSLBLK1`), A/B ZBI upgrade slot metadata
//! (`HRSLAB01`), and persistent Tier 1 `Dir` file namespace (`HRSLFS01`).

pub mod queue;

#[cfg(target_os = "fuchsia")]
pub mod blk;
#[cfg(target_os = "fuchsia")]
pub mod dma;
#[cfg(target_os = "fuchsia")]
pub mod net;
#[cfg(target_os = "fuchsia")]
pub mod pci;

use hirsel_proto::{UpgradeInfo, VolumeInfo, crc32, sha256};

pub const SECTOR_SIZE: usize = 512;
const SUPERBLOCK_MAGIC: [u8; 8] = *b"HRSLBLK1";
const AB_META_MAGIC: [u8; 8] = *b"HRSLAB01";
const FS_SUPER_MAGIC: [u8; 8] = *b"HRSLFS01";
const VOL_META_MAGIC: [u8; 8] = *b"HRSLVOL1";

const CATALOG_START_SECTOR: u64 = 1;
const CATALOG_SECTORS: u64 = 16; // 8 KiB = 128 x 64-byte entries
const DATA_START_SECTOR: u64 = CATALOG_START_SECTOR + CATALOG_SECTORS;
const MAX_CATALOG_ENTRIES: usize = 128;
const ENTRY_BYTES: usize = 64;

/// Reserved tail regions on the persistent disk:
/// - Last sector (`cap - 1`): A/B ZBI upgrade slot metadata (`HRSLAB01`).
/// - Preceding 512 sectors (256 KiB): Persistent Tier 1 `Dir` + declarative VM state store (`HRSLFS01`).
/// - Preceding `ab_slot_sectors` (8 MiB on >=16 MiB disks): Staged inactive ZBI boot image slot.
/// - Preceding 2 sectors (1 KiB): Persistent named VM volume catalog (`HRSLVOL1`, up to 16 volumes).
const AB_META_SECTORS: u64 = 1;
const FS_TOTAL_SECTORS: u64 = 512; // 256 KiB total
const FS_INDEX_SECTORS: u64 = 8; // 4 KiB = 64 x 64-byte file headers
const FS_MAX_FILES: usize = 64;
const FS_ENTRY_BYTES: usize = 64;
const FS_DATA_BYTES: usize = ((FS_TOTAL_SECTORS - FS_INDEX_SECTORS) as usize) * SECTOR_SIZE;
const VOL_META_SECTORS: u64 = 2; // 1 KiB = 16-byte header + 16 x 48-byte volume entries
const MAX_VOLUMES: usize = 16;
const VOL_ENTRY_BYTES: usize = 48;
const LARGE_DISK_THRESHOLD_SECTORS: u64 = 32 * 1024; // 16 MiB
const DEFAULT_AB_SLOT_SECTORS: u64 = 16 * 1024; // 8 MiB

// ZBI container header constants (for validating staged upgrades).
const ZBI_TYPE_CONTAINER: u32 = 0x544f_4f42; // "BOOT"
const ZBI_CONTAINER_MAGIC: u32 = 0x868c_f7e6;
const ZBI_ITEM_MAGIC: u32 = 0xb578_1729;

/// Minimal block storage trait implemented by `VirtioBlk` (and in-memory test disks).
pub trait BlockDevice {
    fn capacity_sectors(&self) -> u64;
    fn read_sectors(&mut self, sector: u64, dst: &mut [u8]) -> Result<(), String>;
    fn write_sectors(&mut self, sector: u64, src: &[u8]) -> Result<(), String>;
    fn flush(&mut self) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct StoredImageMeta {
    pub digest: [u8; 32],
    pub start_sector: u64,
    pub byte_len: u64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFile {
    pub name: String,
    pub data: Vec<u8>,
    pub mtime_ns: u64,
}

/// Content-addressed persistent image store + A/B ZBI upgrade slots + Tier 1 `Dir`
/// file store + persistent named VM volumes (`HRSLVOL1`) backed by `virtio-blk`.
pub struct DiskImageStore<B: BlockDevice> {
    blk: B,
    entries: Vec<StoredImageMeta>,
    volumes: Vec<VolumeInfo>,
    next_free_sector: u64,
    data_limit_sector: u64,
    vol_meta_sector: u64,
    ab_slot_sector: u64,
    ab_slot_sectors: u64,
    ab_meta_sector: u64,
    fs_start_sector: u64,
    upgrade: UpgradeInfo,
    files: Vec<StoredFile>,
}

impl<B: BlockDevice> DiskImageStore<B> {
    /// Opens an existing `HRSLBLK1` store or formats a blank disk.
    pub fn open_or_format(mut blk: B) -> Result<(Self, bool), String> {
        let cap = blk.capacity_sectors();
        let min_needed = DATA_START_SECTOR + FS_TOTAL_SECTORS + AB_META_SECTORS + VOL_META_SECTORS + 32;
        if cap < min_needed {
            return Err(format!("block device too small ({cap} sectors, need >= {min_needed})"));
        }

        let ab_meta_sector = cap - AB_META_SECTORS;
        let fs_start_sector = ab_meta_sector - FS_TOTAL_SECTORS;
        let ab_slot_sectors = if cap >= LARGE_DISK_THRESHOLD_SECTORS {
            DEFAULT_AB_SLOT_SECTORS
        } else {
            (fs_start_sector - DATA_START_SECTOR) / 4
        };
        let ab_slot_sector = fs_start_sector - ab_slot_sectors;
        let vol_meta_sector = ab_slot_sector - VOL_META_SECTORS;
        let data_limit_sector = vol_meta_sector;

        let mut sb = [0u8; SECTOR_SIZE];
        blk.read_sectors(0, &mut sb)?;

        let valid_magic = sb[0..8] == SUPERBLOCK_MAGIC;
        let version = u32::from_le_bytes(sb[8..12].try_into().unwrap());
        let entry_count = u32::from_le_bytes(sb[12..16].try_into().unwrap()) as usize;
        let next_free = u64::from_le_bytes(sb[16..24].try_into().unwrap());
        let saved_crc = u32::from_le_bytes(sb[24..28].try_into().unwrap());
        let calc_crc = crc32(&[&sb[0..24]]);

        let (entries, next_free_sector, formatted) = if valid_magic
            && version == 1
            && entry_count <= MAX_CATALOG_ENTRIES
            && next_free >= DATA_START_SECTOR
            && next_free <= data_limit_sector
            && saved_crc == calc_crc
        {
            let mut cat = vec![0u8; (CATALOG_SECTORS as usize) * SECTOR_SIZE];
            blk.read_sectors(CATALOG_START_SECTOR, &mut cat)?;
            let mut entries = Vec::with_capacity(entry_count);
            for i in 0..entry_count {
                let off = i * ENTRY_BYTES;
                let digest: [u8; 32] = cat[off..off + 32].try_into().unwrap();
                let start_sector = u64::from_le_bytes(cat[off + 32..off + 40].try_into().unwrap());
                let byte_len = u64::from_le_bytes(cat[off + 40..off + 48].try_into().unwrap());
                let raw_name = &cat[off + 48..off + 64];
                let name_len = raw_name.iter().position(|&b| b == 0).unwrap_or(16);
                let name = String::from_utf8_lossy(&raw_name[..name_len]).into_owned();
                entries.push(StoredImageMeta {
                    digest,
                    start_sector,
                    byte_len,
                    name,
                });
            }
            (entries, next_free, false)
        } else {
            (Vec::new(), DATA_START_SECTOR, true)
        };

        let (upgrade, need_write_ab) = Self::load_ab_meta(&mut blk, ab_meta_sector)?;
        let (files, need_write_fs) = Self::load_fs(&mut blk, fs_start_sector)?;
        let (volumes, need_write_vols) = Self::load_volumes(&mut blk, vol_meta_sector)?;

        let mut store = Self {
            blk,
            entries,
            volumes,
            next_free_sector,
            data_limit_sector,
            vol_meta_sector,
            ab_slot_sector,
            ab_slot_sectors,
            ab_meta_sector,
            fs_start_sector,
            upgrade,
            files,
        };
        if formatted {
            store.write_metadata()?;
        }
        if formatted || need_write_ab {
            store.write_ab_meta()?;
        }
        if formatted || need_write_fs {
            store.write_fs()?;
        }
        if formatted || need_write_vols {
            store.write_volumes()?;
        }
        Ok((store, formatted))
    }

    fn load_volumes(blk: &mut B, sector: u64) -> Result<(Vec<VolumeInfo>, bool), String> {
        let mut buf = vec![0u8; (VOL_META_SECTORS as usize) * SECTOR_SIZE];
        blk.read_sectors(sector, &mut buf)?;
        let valid_magic = buf[0..8] == VOL_META_MAGIC;
        let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let count = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
        let saved_crc = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        if !valid_magic || version != 1 || count > MAX_VOLUMES {
            return Ok((Vec::new(), true));
        }
        let body_start = 32;
        let body_end = body_start + MAX_VOLUMES * VOL_ENTRY_BYTES;
        let calc_crc = crc32(&[&buf[0..16], &buf[body_start..body_end]]);
        if calc_crc != saved_crc {
            return Ok((Vec::new(), true));
        }
        let mut vols = Vec::with_capacity(count);
        for i in 0..count {
            let off = body_start + i * VOL_ENTRY_BYTES;
            let raw_name = &buf[off..off + 24];
            let nlen = raw_name.iter().position(|&b| b == 0).unwrap_or(24);
            let name = String::from_utf8_lossy(&raw_name[..nlen]).into_owned();
            let start_sector = u64::from_le_bytes(buf[off + 24..off + 32].try_into().unwrap());
            let sectors = u64::from_le_bytes(buf[off + 32..off + 40].try_into().unwrap());
            let size_mib = u32::from_le_bytes(buf[off + 40..off + 44].try_into().unwrap());
            vols.push(VolumeInfo {
                name,
                size_mib,
                sectors,
                start_sector,
            });
        }
        Ok((vols, false))
    }

    fn write_volumes(&mut self) -> Result<(), String> {
        let mut buf = vec![0u8; (VOL_META_SECTORS as usize) * SECTOR_SIZE];
        buf[0..8].copy_from_slice(&VOL_META_MAGIC);
        buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        buf[12..16].copy_from_slice(&(self.volumes.len() as u32).to_le_bytes());
        let body_start = 32;
        let body_end = body_start + MAX_VOLUMES * VOL_ENTRY_BYTES;
        for (i, v) in self.volumes.iter().enumerate() {
            let off = body_start + i * VOL_ENTRY_BYTES;
            let nb = v.name.as_bytes();
            let nlen = nb.len().min(24);
            buf[off..off + nlen].copy_from_slice(&nb[..nlen]);
            buf[off + 24..off + 32].copy_from_slice(&v.start_sector.to_le_bytes());
            buf[off + 32..off + 40].copy_from_slice(&v.sectors.to_le_bytes());
            buf[off + 40..off + 44].copy_from_slice(&v.size_mib.to_le_bytes());
        }
        let crc = crc32(&[&buf[0..16], &buf[body_start..body_end]]);
        buf[16..20].copy_from_slice(&crc.to_le_bytes());
        self.blk.write_sectors(self.vol_meta_sector, &buf)?;
        self.blk.flush()
    }

    fn load_ab_meta(blk: &mut B, sector: u64) -> Result<(UpgradeInfo, bool), String> {
        let mut buf = [0u8; SECTOR_SIZE];
        blk.read_sectors(sector, &mut buf)?;
        let valid_magic = buf[0..8] == AB_META_MAGIC;
        let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let saved_crc = u32::from_le_bytes(buf[60..64].try_into().unwrap());
        let calc_crc = crc32(&[&buf[0..60]]);
        if valid_magic && version == 1 && saved_crc == calc_crc {
            let active_slot = buf[12];
            let pending_slot = buf[13];
            let staged_digest: [u8; 32] = buf[16..48].try_into().unwrap();
            let staged_size = u64::from_le_bytes(buf[48..56].try_into().unwrap());
            let boot_attempts = u32::from_le_bytes(buf[56..60].try_into().unwrap());
            Ok((
                UpgradeInfo {
                    active_slot,
                    pending_slot,
                    staged_digest,
                    staged_size,
                    boot_attempts,
                },
                false,
            ))
        } else {
            Ok((
                UpgradeInfo {
                    active_slot: 0,
                    pending_slot: 0xFF,
                    staged_digest: [0u8; 32],
                    staged_size: 0,
                    boot_attempts: 0,
                },
                true,
            ))
        }
    }

    fn write_ab_meta(&mut self) -> Result<(), String> {
        let mut buf = [0u8; SECTOR_SIZE];
        buf[0..8].copy_from_slice(&AB_META_MAGIC);
        buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        buf[12] = self.upgrade.active_slot;
        buf[13] = self.upgrade.pending_slot;
        buf[16..48].copy_from_slice(&self.upgrade.staged_digest);
        buf[48..56].copy_from_slice(&self.upgrade.staged_size.to_le_bytes());
        buf[56..60].copy_from_slice(&self.upgrade.boot_attempts.to_le_bytes());
        let crc = crc32(&[&buf[0..60]]);
        buf[60..64].copy_from_slice(&crc.to_le_bytes());
        self.blk.write_sectors(self.ab_meta_sector, &buf)?;
        self.blk.flush()
    }

    fn load_fs(blk: &mut B, start_sector: u64) -> Result<(Vec<StoredFile>, bool), String> {
        let mut idx = vec![0u8; (FS_INDEX_SECTORS as usize) * SECTOR_SIZE];
        blk.read_sectors(start_sector, &mut idx)?;
        let valid_magic = idx[0..8] == FS_SUPER_MAGIC;
        let version = u32::from_le_bytes(idx[8..12].try_into().unwrap());
        let count = u32::from_le_bytes(idx[12..16].try_into().unwrap()) as usize;
        let data_len = u32::from_le_bytes(idx[16..20].try_into().unwrap()) as usize;
        let saved_crc = u32::from_le_bytes(idx[20..24].try_into().unwrap());
        if !valid_magic || version != 1 || count > FS_MAX_FILES - 1 || data_len > FS_DATA_BYTES {
            return Ok((Vec::new(), true));
        }
        let data_sectors = data_len.div_ceil(SECTOR_SIZE);
        let mut data_buf = vec![0u8; data_sectors * SECTOR_SIZE];
        if data_sectors > 0 {
            blk.read_sectors(start_sector + FS_INDEX_SECTORS, &mut data_buf)?;
        }
        let calc_crc = crc32(&[&idx[0..20], &idx[FS_ENTRY_BYTES..], &data_buf[..data_len]]);
        if calc_crc != saved_crc {
            return Ok((Vec::new(), true));
        }

        let mut files = Vec::with_capacity(count);
        for i in 0..count {
            let off = (i + 1) * FS_ENTRY_BYTES;
            let raw_name = &idx[off..off + 44];
            let nlen = raw_name.iter().position(|&b| b == 0).unwrap_or(44);
            let name = String::from_utf8_lossy(&raw_name[..nlen]).into_owned();
            let start = u32::from_le_bytes(idx[off + 44..off + 48].try_into().unwrap()) as usize;
            let len = u32::from_le_bytes(idx[off + 48..off + 52].try_into().unwrap()) as usize;
            let mtime_ns = u64::from_le_bytes(idx[off + 52..off + 60].try_into().unwrap());
            if start.checked_add(len).is_some_and(|e| e <= data_len) {
                files.push(StoredFile {
                    name,
                    data: data_buf[start..start + len].to_vec(),
                    mtime_ns,
                });
            }
        }
        Ok((files, false))
    }

    fn write_fs(&mut self) -> Result<(), String> {
        let mut idx = vec![0u8; (FS_INDEX_SECTORS as usize) * SECTOR_SIZE];
        let mut data_buf = Vec::new();
        for (i, f) in self.files.iter().enumerate() {
            let off = (i + 1) * FS_ENTRY_BYTES;
            let nb = f.name.as_bytes();
            let nlen = nb.len().min(44);
            idx[off..off + nlen].copy_from_slice(&nb[..nlen]);
            let start = data_buf.len() as u32;
            let len = f.data.len() as u32;
            idx[off + 44..off + 48].copy_from_slice(&start.to_le_bytes());
            idx[off + 48..off + 52].copy_from_slice(&len.to_le_bytes());
            idx[off + 52..off + 60].copy_from_slice(&f.mtime_ns.to_le_bytes());
            data_buf.extend_from_slice(&f.data);
        }
        if data_buf.len() > FS_DATA_BYTES {
            return Err("persistent Dir store exceeded 252 KiB quota".into());
        }
        let data_len = data_buf.len();
        idx[0..8].copy_from_slice(&FS_SUPER_MAGIC);
        idx[8..12].copy_from_slice(&1u32.to_le_bytes());
        idx[12..16].copy_from_slice(&(self.files.len() as u32).to_le_bytes());
        idx[16..20].copy_from_slice(&(data_len as u32).to_le_bytes());
        let crc = crc32(&[&idx[0..20], &idx[FS_ENTRY_BYTES..], &data_buf]);
        idx[20..24].copy_from_slice(&crc.to_le_bytes());

        let padded_len = data_len.div_ceil(SECTOR_SIZE) * SECTOR_SIZE;
        data_buf.resize(padded_len, 0);
        if !data_buf.is_empty() {
            self.blk
                .write_sectors(self.fs_start_sector + FS_INDEX_SECTORS, &data_buf)?;
        }
        self.blk.write_sectors(self.fs_start_sector, &idx)?;
        self.blk.flush()
    }

    pub fn capacity_mib(&self) -> u32 {
        ((self.blk.capacity_sectors() * SECTOR_SIZE as u64) / (1024 * 1024)) as u32
    }

    pub fn entry_count(&self) -> u32 {
        self.entries.len() as u32
    }

    pub fn entries(&self) -> &[StoredImageMeta] {
        &self.entries
    }

    pub fn upgrade_info(&self) -> UpgradeInfo {
        self.upgrade.clone()
    }

    /// Validates a Zircon Boot Image (ZBI) container + SHA-256 digest and stages it
    /// into the inactive A/B boot slot on `virtio-blk`.
    pub fn stage_upgrade_zbi(
        &mut self,
        expected_digest: [u8; 32],
        zbi_bytes: &[u8],
    ) -> Result<UpgradeInfo, String> {
        if zbi_bytes.len() < 32 {
            return Err("ZBI image too small (< 32 bytes)".into());
        }
        let item_type = u32::from_le_bytes(zbi_bytes[0..4].try_into().unwrap());
        let payload_len = u32::from_le_bytes(zbi_bytes[4..8].try_into().unwrap()) as usize;
        let extra = u32::from_le_bytes(zbi_bytes[8..12].try_into().unwrap());
        let magic = u32::from_le_bytes(zbi_bytes[24..28].try_into().unwrap());
        if item_type != ZBI_TYPE_CONTAINER
            || extra != ZBI_CONTAINER_MAGIC
            || magic != ZBI_ITEM_MAGIC
        {
            return Err(format!(
                "invalid ZBI container header (type={item_type:#x}, extra={extra:#x}, magic={magic:#x})"
            ));
        }
        if 32 + payload_len > zbi_bytes.len() {
            return Err(format!(
                "truncated ZBI container: header declares {payload_len} bytes, got {}",
                zbi_bytes.len() - 32
            ));
        }
        let actual_digest = sha256(zbi_bytes);
        if actual_digest != expected_digest {
            return Err("ZBI upgrade SHA-256 digest mismatch".into());
        }
        let sectors = zbi_bytes.len().div_ceil(SECTOR_SIZE) as u64;
        if sectors > self.ab_slot_sectors {
            return Err(format!(
                "ZBI image ({} bytes) exceeds A/B slot capacity ({} bytes)",
                zbi_bytes.len(),
                self.ab_slot_sectors * SECTOR_SIZE as u64
            ));
        }

        let mut padded = vec![0u8; (sectors as usize) * SECTOR_SIZE];
        padded[..zbi_bytes.len()].copy_from_slice(zbi_bytes);
        self.blk.write_sectors(self.ab_slot_sector, &padded)?;

        let next_slot = if self.upgrade.active_slot == 0 { 1 } else { 0 };
        self.upgrade.pending_slot = next_slot;
        self.upgrade.staged_digest = expected_digest;
        self.upgrade.staged_size = zbi_bytes.len() as u64;
        self.upgrade.boot_attempts = 0;
        self.write_ab_meta()?;
        Ok(self.upgrade.clone())
    }

    /// Reads back and verifies the staged ZBI image from the inactive A/B slot.
    pub fn read_staged_zbi(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.upgrade.pending_slot == 0xFF || self.upgrade.staged_size == 0 {
            return Ok(None);
        }
        let size = self.upgrade.staged_size as usize;
        let sectors = size.div_ceil(SECTOR_SIZE);
        let mut buf = vec![0u8; sectors * SECTOR_SIZE];
        self.blk.read_sectors(self.ab_slot_sector, &mut buf)?;
        buf.truncate(size);
        if sha256(&buf) != self.upgrade.staged_digest {
            return Err("staged ZBI on-disk SHA-256 mismatch".into());
        }
        Ok(Some(buf))
    }

    /// Commits a pending A/B ZBI upgrade: promotes `pending_slot` to `active_slot`
    /// and clears `pending_slot`.
    pub fn commit_upgrade(&mut self) -> Result<UpgradeInfo, String> {
        if self.upgrade.pending_slot == 0xFF {
            return Err("no pending A/B upgrade to commit".into());
        }
        self.upgrade.active_slot = self.upgrade.pending_slot;
        self.upgrade.pending_slot = 0xFF;
        self.upgrade.boot_attempts = 0;
        self.write_ab_meta()?;
        Ok(self.upgrade.clone())
    }

    /// Rolls back a pending A/B ZBI upgrade (or swaps active slot back to previous slot).
    pub fn rollback_upgrade(&mut self) -> Result<UpgradeInfo, String> {
        if self.upgrade.pending_slot != 0xFF {
            self.upgrade.pending_slot = 0xFF;
            self.upgrade.staged_digest = [0u8; 32];
            self.upgrade.staged_size = 0;
            self.upgrade.boot_attempts = 0;
        } else {
            self.upgrade.active_slot = if self.upgrade.active_slot == 0 { 1 } else { 0 };
            self.upgrade.boot_attempts = 0;
        }
        self.write_ab_meta()?;
        Ok(self.upgrade.clone())
    }

    // ---- Persistent Named VM Block Volumes (`HRSLVOL1`) ----

    pub fn list_volumes(&self) -> &[VolumeInfo] {
        &self.volumes
    }

    pub fn find_volume(&self, name: &str) -> Option<&VolumeInfo> {
        self.volumes.iter().find(|v| v.name == name)
    }

    /// Creates a new persistent named VM block volume carved out of `virtio-blk`.
    /// Optionally seeds the beginning of the volume with `initial_data` (e.g. an ext4 image).
    pub fn create_volume(
        &mut self,
        name: &str,
        size_mib: u32,
        initial_data: Option<&[u8]>,
    ) -> Result<VolumeInfo, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 24 {
            return Err("volume name must be 1..=24 characters".into());
        }
        if self.volumes.iter().any(|v| v.name == name) {
            return Err(format!("volume `{name}` already exists"));
        }
        if self.volumes.len() >= MAX_VOLUMES {
            return Err(format!("volume catalog full ({MAX_VOLUMES} volumes)"));
        }
        let min_mib = initial_data
            .map(|d| (d.len().div_ceil(1024 * 1024) as u32).max(1))
            .unwrap_or(1);
        let eff_mib = size_mib.max(min_mib);
        let sectors = u64::from(eff_mib) * ((1024 * 1024) / SECTOR_SIZE as u64);
        let end_sector = self
            .next_free_sector
            .checked_add(sectors)
            .ok_or_else(|| "volume sector overflow".to_string())?;
        if end_sector > self.data_limit_sector {
            let avail_mib =
                ((self.data_limit_sector.saturating_sub(self.next_free_sector)) * SECTOR_SIZE as u64)
                    / (1024 * 1024);
            return Err(format!(
                "not enough space on virtio-blk for {eff_mib} MiB volume `{name}` ({avail_mib} MiB free)"
            ));
        }

        let start_sector = self.next_free_sector;

        // Zero the first 64 KiB (superblock area) unless seeding from an initial image,
        // so fresh volumes don't inherit stale filesystem signatures.
        let zero_chunk = vec![0u8; 64 * 1024];
        if let Some(init) = initial_data {
            let mut off = 0usize;
            while off < init.len() {
                let end = (off + 64 * 1024).min(init.len());
                let slice = &init[off..end];
                let padded_len = slice.len().div_ceil(SECTOR_SIZE) * SECTOR_SIZE;
                let mut buf = vec![0u8; padded_len];
                buf[..slice.len()].copy_from_slice(slice);
                self.blk
                    .write_sectors(start_sector + (off / SECTOR_SIZE) as u64, &buf)?;
                off = end;
            }
        } else {
            let clear_sectors = sectors.min((zero_chunk.len() / SECTOR_SIZE) as u64);
            self.blk
                .write_sectors(start_sector, &zero_chunk[..(clear_sectors as usize) * SECTOR_SIZE])?;
        }

        let info = VolumeInfo {
            name: name.to_owned(),
            size_mib: eff_mib,
            sectors,
            start_sector,
        };
        self.volumes.push(info.clone());
        self.next_free_sector = end_sector;
        self.write_metadata()?;
        self.write_volumes()?;
        Ok(info)
    }

    /// Removes a named volume from the catalog.
    pub fn delete_volume(&mut self, name: &str) -> Result<bool, String> {
        let before = self.volumes.len();
        self.volumes.retain(|v| v.name != name);
        if self.volumes.len() != before {
            self.write_volumes()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn read_volume_sectors(
        &mut self,
        start_sector: u64,
        vol_sectors: u64,
        rel_sector: u64,
        dst: &mut [u8],
    ) -> Result<(), String> {
        let count = (dst.len() / SECTOR_SIZE) as u64;
        if rel_sector.checked_add(count).is_none_or(|e| e > vol_sectors) {
            return Err("volume read out of bounds".into());
        }
        self.blk.read_sectors(start_sector + rel_sector, dst)
    }

    pub fn write_volume_sectors(
        &mut self,
        start_sector: u64,
        vol_sectors: u64,
        rel_sector: u64,
        src: &[u8],
    ) -> Result<(), String> {
        let count = (src.len() / SECTOR_SIZE) as u64;
        if rel_sector.checked_add(count).is_none_or(|e| e > vol_sectors) {
            return Err("volume write out of bounds".into());
        }
        self.blk.write_sectors(start_sector + rel_sector, src)
    }

    pub fn flush_blk(&mut self) -> Result<(), String> {
        self.blk.flush()
    }

    // ---- Persistent Tier 1 Dir file store (`HRSLFS01`) ----

    pub fn fs_get(&self, name: &str) -> Option<&StoredFile> {
        self.files.iter().find(|f| f.name == name)
    }

    pub fn fs_list(&self) -> &[StoredFile] {
        &self.files
    }

    pub fn fs_put(&mut self, name: &str, data: Vec<u8>, mtime_ns: u64) -> Result<(), String> {
        if name.is_empty() || name.len() > 44 {
            return Err("file name must be 1..=44 bytes".into());
        }
        if let Some(existing) = self.files.iter_mut().find(|f| f.name == name) {
            existing.data = data;
            existing.mtime_ns = mtime_ns;
        } else {
            if self.files.len() >= FS_MAX_FILES - 1 {
                return Err("persistent Dir file table full (63 files)".into());
            }
            self.files.push(StoredFile {
                name: name.to_owned(),
                data,
                mtime_ns,
            });
        }
        self.write_fs()
    }

    pub fn fs_remove(&mut self, name: &str) -> Result<bool, String> {
        let before = self.files.len();
        self.files.retain(|f| f.name != name);
        if self.files.len() != before {
            self.write_fs()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn fs_rename(&mut self, from: &str, to: &str) -> Result<bool, String> {
        if to.is_empty() || to.len() > 44 {
            return Err("target file name must be 1..=44 bytes".into());
        }
        let Some(pos) = self.files.iter().position(|f| f.name == from) else {
            return Ok(false);
        };
        let mut moved = self.files.remove(pos);
        self.files.retain(|f| f.name != to);
        moved.name = to.to_owned();
        self.files.push(moved);
        self.write_fs()?;
        Ok(true)
    }

    /// Reads and SHA-256-verifies a stored image's raw bytes from disk.
    pub fn read_image(&mut self, meta: &StoredImageMeta) -> Result<Vec<u8>, String> {
        let sectors = (meta.byte_len as usize).div_ceil(SECTOR_SIZE);
        let mut buf = vec![0u8; sectors * SECTOR_SIZE];
        self.blk.read_sectors(meta.start_sector, &mut buf)?;
        buf.truncate(meta.byte_len as usize);
        if sha256(&buf) != meta.digest {
            return Err(format!("on-disk SHA-256 mismatch for `{}`", meta.name));
        }
        Ok(buf)
    }

    /// Persists a verified image to disk (no-op if digest already stored).
    pub fn put_image(&mut self, digest: [u8; 32], name: &str, bytes: &[u8]) -> Result<bool, String> {
        if self.entries.iter().any(|e| e.digest == digest) {
            return Ok(false);
        }
        if self.entries.len() >= MAX_CATALOG_ENTRIES {
            return Err("disk image catalog full (128 entries)".into());
        }
        let sectors = bytes.len().div_ceil(SECTOR_SIZE) as u64;
        let end_sector = self
            .next_free_sector
            .checked_add(sectors)
            .ok_or_else(|| "sector overflow".to_string())?;
        if end_sector > self.data_limit_sector {
            return Err("disk image store out of space".into());
        }

        let start_sector = self.next_free_sector;
        let mut padded = vec![0u8; (sectors as usize) * SECTOR_SIZE];
        padded[..bytes.len()].copy_from_slice(bytes);
        self.blk.write_sectors(start_sector, &padded)?;

        self.entries.push(StoredImageMeta {
            digest,
            start_sector,
            byte_len: bytes.len() as u64,
            name: name.chars().take(16).collect(),
        });
        self.next_free_sector = end_sector;
        self.write_metadata()?;
        Ok(true)
    }

    fn write_metadata(&mut self) -> Result<(), String> {
        let mut cat = vec![0u8; (CATALOG_SECTORS as usize) * SECTOR_SIZE];
        for (i, e) in self.entries.iter().enumerate() {
            let off = i * ENTRY_BYTES;
            cat[off..off + 32].copy_from_slice(&e.digest);
            cat[off + 32..off + 40].copy_from_slice(&e.start_sector.to_le_bytes());
            cat[off + 40..off + 48].copy_from_slice(&e.byte_len.to_le_bytes());
            let nb = e.name.as_bytes();
            let nlen = nb.len().min(16);
            cat[off + 48..off + 48 + nlen].copy_from_slice(&nb[..nlen]);
        }
        self.blk.write_sectors(CATALOG_START_SECTOR, &cat)?;

        let mut sb = [0u8; SECTOR_SIZE];
        sb[0..8].copy_from_slice(&SUPERBLOCK_MAGIC);
        sb[8..12].copy_from_slice(&1u32.to_le_bytes());
        sb[12..16].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        sb[16..24].copy_from_slice(&self.next_free_sector.to_le_bytes());
        let sum = crc32(&[&sb[0..24]]);
        sb[24..28].copy_from_slice(&sum.to_le_bytes());
        self.blk.write_sectors(0, &sb)?;
        self.blk.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemDisk(Vec<u8>);

    impl BlockDevice for MemDisk {
        fn capacity_sectors(&self) -> u64 {
            (self.0.len() / SECTOR_SIZE) as u64
        }
        fn read_sectors(&mut self, sector: u64, dst: &mut [u8]) -> Result<(), String> {
            let off = (sector as usize) * SECTOR_SIZE;
            dst.copy_from_slice(&self.0[off..off + dst.len()]);
            Ok(())
        }
        fn write_sectors(&mut self, sector: u64, src: &[u8]) -> Result<(), String> {
            let off = (sector as usize) * SECTOR_SIZE;
            self.0[off..off + src.len()].copy_from_slice(src);
            Ok(())
        }
        fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    fn make_mock_zbi(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 32 + payload.len()];
        out[0..4].copy_from_slice(&ZBI_TYPE_CONTAINER.to_le_bytes());
        out[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        out[8..12].copy_from_slice(&ZBI_CONTAINER_MAGIC.to_le_bytes());
        out[24..28].copy_from_slice(&ZBI_ITEM_MAGIC.to_le_bytes());
        out[32..].copy_from_slice(payload);
        out
    }

    #[test]
    fn disk_image_store_persists_across_reopen() {
        let disk = MemDisk(vec![0u8; 2 * 1024 * 1024]);
        let (mut store, formatted) = DiskImageStore::open_or_format(disk).unwrap();
        assert!(formatted);
        assert_eq!(store.entry_count(), 0);

        let payload = b"ELF-mock-hirsel-program-bytes-1234567890";
        let digest = sha256(payload);
        assert!(store.put_image(digest, "hello-remote", payload).unwrap());
        assert!(!store.put_image(digest, "hello-remote", payload).unwrap());

        // Persist a Tier 1 Dir file and stage an A/B ZBI upgrade.
        store.fs_put("counter.txt", b"run=1".to_vec(), 42_000).unwrap();
        let zbi = make_mock_zbi(b"mock-zircon-kernel-zbi-item");
        let zbi_digest = sha256(&zbi);
        let info = store.stage_upgrade_zbi(zbi_digest, &zbi).unwrap();
        assert_eq!(info.active_slot, 0);
        assert_eq!(info.pending_slot, 1);
        assert_eq!(info.staged_digest, zbi_digest);

        // Reopen the same backing bytes (simulating cold VM reboot).
        let raw_disk = store.blk;
        let (mut reopened, formatted2) = DiskImageStore::open_or_format(raw_disk).unwrap();
        assert!(!formatted2);
        assert_eq!(reopened.entry_count(), 1);
        let meta = reopened.entries()[0].clone();
        assert_eq!(meta.digest, digest);
        assert_eq!(meta.name, "hello-remote");
        assert_eq!(reopened.read_image(&meta).unwrap(), payload);

        // Persistent Dir file survived reboot.
        let f = reopened.fs_get("counter.txt").unwrap();
        assert_eq!(f.data, b"run=1");
        assert_eq!(f.mtime_ns, 42_000);

        // Staged A/B ZBI upgrade survived reboot and passes SHA-256 verification.
        let up = reopened.upgrade_info();
        assert_eq!(up.active_slot, 0);
        assert_eq!(up.pending_slot, 1);
        assert_eq!(up.staged_digest, zbi_digest);
        assert_eq!(reopened.read_staged_zbi().unwrap().unwrap(), zbi);
    }

    #[test]
    fn stage_upgrade_rejects_corrupt_zbi() {
        let disk = MemDisk(vec![0u8; 2 * 1024 * 1024]);
        let (mut store, _) = DiskImageStore::open_or_format(disk).unwrap();
        let bad = b"not-a-valid-zbi-container-at-all-0123456789";
        let err = store.stage_upgrade_zbi(sha256(bad), bad).unwrap_err();
        assert!(err.contains("invalid ZBI container header"));
    }

    #[test]
    fn volumes_and_upgrade_commit_rollback_persist_across_reopen() {
        let disk = MemDisk(vec![0u8; 4 * 1024 * 1024]);
        let (mut store, _) = DiskImageStore::open_or_format(disk).unwrap();

        let vol = store.create_volume("data-1", 1, None).unwrap();
        assert_eq!(vol.name, "data-1");
        assert_eq!(vol.size_mib, 1);
        assert_eq!(vol.sectors, 2048);

        let mut sec = [0u8; SECTOR_SIZE];
        sec[..15].copy_from_slice(b"HIRSEL_PERSIST!");
        store
            .write_volume_sectors(vol.start_sector, vol.sectors, 3, &sec)
            .unwrap();
        store.flush_blk().unwrap();

        let zbi = make_mock_zbi(b"zbi-upgrade-v2");
        store.stage_upgrade_zbi(sha256(&zbi), &zbi).unwrap();
        let committed = store.commit_upgrade().unwrap();
        assert_eq!(committed.active_slot, 1);
        assert_eq!(committed.pending_slot, 0xFF);

        // Reopen raw disk bytes (simulate reboot).
        let raw = store.blk;
        let (mut reopened, formatted) = DiskImageStore::open_or_format(raw).unwrap();
        assert!(!formatted);
        let v2 = reopened.find_volume("data-1").unwrap().clone();
        assert_eq!(v2.start_sector, vol.start_sector);
        let mut readback = [0u8; SECTOR_SIZE];
        reopened
            .read_volume_sectors(v2.start_sector, v2.sectors, 3, &mut readback)
            .unwrap();
        assert_eq!(&readback[..15], b"HIRSEL_PERSIST!");
        assert_eq!(reopened.upgrade_info().active_slot, 1);

        let rolled = reopened.rollback_upgrade().unwrap();
        assert_eq!(rolled.active_slot, 0);
        assert!(reopened.delete_volume("data-1").unwrap());
        assert!(reopened.find_volume("data-1").is_none());
    }
}
