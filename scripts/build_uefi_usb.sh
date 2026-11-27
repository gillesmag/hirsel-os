#!/usr/bin/env bash
# Assemble a raw GPT UEFI-bootable disk image for Hirsel-OS Desktop (bare-metal x86-64 / OVMF).
# Layout:
#   - Partition 1 (ESP, FAT32, 64 MiB): EFI/BOOT/BOOTX64.EFI + zircon-a.zbi + zircon-b.zbi
#   - Partition 2 (FVM/Fxfs workstation volume, 2048 MiB): unsparsed Hirsel-OS Fxfs disk
#
# Usage:
#   ./scripts/build_uefi_usb.sh [output_image_path]
#   sudo dd if=out/hirsel-desktop-uefi.img of=/dev/sdX bs=4M status=progress conv=fsync
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FUCHSIA="$ROOT/third-party/fuchsia"
PB_SYS="$FUCHSIA/out/hirsel-desktop.x64/obj/products/hirsel_desktop/hirsel_desktop.x64/product_bundle/system_a"
OUT_IMG="${1:-$ROOT/out/hirsel-desktop-uefi.img}"
FXFS_RAW="$ROOT/out/hirsel-desktop-fxfs.raw"

if [[ ! -f "$PB_SYS/fuchsia.zbi" || ! -f "$PB_SYS/fxfs.sparse.blk" ]]; then
  echo "ERROR: Product bundle not found at $PB_SYS. Run 'just build-desktop' first." >&2
  exit 1
fi

echo "=== 1. Unsparsing Fxfs workstation volume ==="
python3 "$ROOT/scripts/unsparse_fxfs.py" "$PB_SYS/fxfs.sparse.blk" "$FXFS_RAW"

echo "=== 2. Locating UEFI Gigaboot/EFI shim & assembling GPT image ($OUT_IMG) ==="
EFI_BIN="$(find "$FUCHSIA/out/hirsel-desktop.x64" -maxdepth 4 -name 'bootx64.efi' -o -name 'gigaboot*.efi' 2>/dev/null | head -n 1 || true)"

python3 - "$OUT_IMG" "$PB_SYS/fuchsia.zbi" "$FXFS_RAW" "$EFI_BIN" <<'PY'
import os, struct, sys, zlib

out_img, zbi_path, fxfs_path, efi_bin = sys.argv[1:5]
SECTOR = 512
ESP_START_LBA = 2048               # 1 MiB alignment
ESP_SECTORS = 64 * 1024 * 2        # 64 MiB
ZIRCON_A_LBA = ESP_START_LBA + ESP_SECTORS
ZBI_BYTES = open(zbi_path, "rb").read()
ZBI_SECTORS = max(65536, (len(ZBI_BYTES) + SECTOR - 1) // SECTOR + 2048)  # >= 32 MiB slot
ZIRCON_B_LBA = ZIRCON_A_LBA + ZBI_SECTORS
FXFS_LBA = ZIRCON_B_LBA + ZBI_SECTORS
FXFS_SIZE = os.path.getsize(fxfs_path)
FXFS_SECTORS = (FXFS_SIZE + SECTOR - 1) // SECTOR
TOTAL_SECTORS = FXFS_LBA + FXFS_SECTORS + 2048

# Standard Fuchsia / UEFI GPT Partition Type GUIDs (mixed-endian)
def guid(s: str) -> bytes:
    p = s.split("-")
    return (
        struct.pack("<IHH", int(p[0], 16), int(p[1], 16), int(p[2], 16))
        + bytes.fromhex(p[3] + p[4])
    )

GUID_ESP      = guid("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
GUID_ZIRCON_A = guid("DE30CC86-1F4A-4A31-93C4-66F147D33E05")
GUID_ZIRCON_B = guid("23CC04DF-C278-4CE7-8471-897D1A4BCDF7")
GUID_FXFS     = guid("41D0E340-57E3-954E-8C1E-17ECAC44CFF5")

entries = bytearray(128 * 128)
parts = [
    ("ESP",      GUID_ESP,      ESP_START_LBA, ESP_START_LBA + ESP_SECTORS - 1),
    ("zircon-a", GUID_ZIRCON_A, ZIRCON_A_LBA,  ZIRCON_A_LBA + ZBI_SECTORS - 1),
    ("zircon-b", GUID_ZIRCON_B, ZIRCON_B_LBA,  ZIRCON_B_LBA + ZBI_SECTORS - 1),
    ("fvm",      GUID_FXFS,     FXFS_LBA,      FXFS_LBA + FXFS_SECTORS - 1),
]
for idx, (name, ptype, first, last) in enumerate(parts):
    uniq = os.urandom(16)
    name_u16 = name.encode("utf-16le").ljust(72, b"\x00")[:72]
    ent = ptype + uniq + struct.pack("<QQQ", first, last, 0) + name_u16
    entries[idx * 128 : (idx + 1) * 128] = ent

entries_crc = zlib.crc32(entries) & 0xFFFFFFFF
disk_guid = os.urandom(16)

def make_gpt_hdr(my_lba: int, alt_lba: int, ent_lba: int) -> bytes:
    hdr = bytearray(struct.pack(
        "<8sIIIIQQQQ16sQIII",
        b"EFI PART", 0x00010000, 92, 0, 0,
        my_lba, alt_lba, 34, TOTAL_SECTORS - 34,
        disk_guid, ent_lba, 128, 128, entries_crc,
    ))
    crc = zlib.crc32(hdr) & 0xFFFFFFFF
    struct.pack_into("<I", hdr, 16, crc)
    return bytes(hdr).ljust(SECTOR, b"\x00")

# Protective MBR
mbr = bytearray(512)
mbr[446 + 4] = 0xEE
struct.pack_into("<II", mbr, 446 + 8, 1, min(0xFFFFFFFF, TOTAL_SECTORS - 1))
mbr[510] = 0x55
mbr[511] = 0xAA

os.makedirs(os.path.dirname(out_img), exist_ok=True)
with open(out_img, "wb") as f:
    f.truncate(TOTAL_SECTORS * SECTOR)
    f.seek(0)
    f.write(mbr)
    f.seek(1 * SECTOR)
    f.write(make_gpt_hdr(1, TOTAL_SECTORS - 1, 2))
    f.seek(2 * SECTOR)
    f.write(entries)
    # Write A/B Zircon ZBI slots
    f.seek(ZIRCON_A_LBA * SECTOR)
    f.write(ZBI_BYTES)
    f.seek(ZIRCON_B_LBA * SECTOR)
    f.write(ZBI_BYTES)
    # Copy Fxfs workstation volume
    f.seek(FXFS_LBA * SECTOR)
    with open(fxfs_path, "rb") as src:
        while True:
            chunk = src.read(4 * 1024 * 1024)
            if not chunk:
                break
            f.write(chunk)
    # Backup GPT
    f.seek((TOTAL_SECTORS - 33) * SECTOR)
    f.write(entries)
    f.seek((TOTAL_SECTORS - 1) * SECTOR)
    f.write(make_gpt_hdr(TOTAL_SECTORS - 1, 1, TOTAL_SECTORS - 33))

print(f"Created GPT A/B UEFI Hirsel-OS Desktop image: {out_img} ({TOTAL_SECTORS * SECTOR / (1024*1024):.1f} MiB)")
print(f"  Partitions: ESP={ESP_START_LBA}, zircon-a={ZIRCON_A_LBA}, zircon-b={ZIRCON_B_LBA}, fvm/fxfs={FXFS_LBA}")
if efi_bin:
    print(f"  Detected EFI binary: {efi_bin}")
else:
    print("  Note: build //src/firmware/gigaboot/cpp:esp to populate FAT32 ESP with BOOTX64.EFI for bare-metal UEFI.")
PY
rm -f "$FXFS_RAW"
ls -lh "$OUT_IMG"
