#!/usr/bin/env python3
"""Unsparse a Fuchsia Android-sparse fxfs.sparse.blk image into a raw disk image."""
import os
import struct
import sys

SPARSE_MAGIC = 0xED26FF3A
CHUNK_RAW = 0xCAC1
CHUNK_FILL = 0xCAC2
CHUNK_DONT_CARE = 0xCAC3
CHUNK_CRC32 = 0xCAC4


def unsparse(src_path: str, dst_path: str, min_size_bytes: int = 2 * 1024 * 1024 * 1024) -> None:
    with open(src_path, "rb") as src:
        hdr = src.read(28)
        magic, major, minor, file_hdr_sz, chunk_hdr_sz, blk_sz, total_blks, total_chunks, _ = (
            struct.unpack("<IHHHHIIII", hdr)
        )
        if magic != SPARSE_MAGIC:
            raise ValueError(f"{src_path}: not a sparse image (magic=0x{magic:08x})")
        if file_hdr_sz > 28:
            src.seek(file_hdr_sz)

        total_bytes = max(total_blks * blk_sz, min_size_bytes)
        with open(dst_path, "wb") as dst:
            dst.truncate(total_bytes)
            out_pos = 0
            for _ in range(total_chunks):
                chdr = src.read(12)
                chunk_type, _reserved, chunk_sz, total_sz = struct.unpack("<HHII", chdr)
                if chunk_hdr_sz > 12:
                    src.seek(chunk_hdr_sz - 12, os.SEEK_CUR)
                data_sz = total_sz - chunk_hdr_sz
                out_bytes = chunk_sz * blk_sz

                if chunk_type == CHUNK_RAW:
                    dst.seek(out_pos)
                    remaining = data_sz
                    while remaining > 0:
                        buf = src.read(min(remaining, 4 * 1024 * 1024))
                        if not buf:
                            raise EOFError("unexpected EOF in CHUNK_RAW")
                        dst.write(buf)
                        remaining -= len(buf)
                    out_pos += out_bytes
                elif chunk_type == CHUNK_FILL:
                    fill_word = src.read(4)
                    if data_sz > 4:
                        src.seek(data_sz - 4, os.SEEK_CUR)
                    if fill_word != b"\x00\x00\x00\x00":
                        dst.seek(out_pos)
                        block = fill_word * (blk_sz // 4)
                        for _b in range(chunk_sz):
                            dst.write(block)
                    out_pos += out_bytes
                elif chunk_type == CHUNK_DONT_CARE:
                    if data_sz > 0:
                        src.seek(data_sz, os.SEEK_CUR)
                    out_pos += out_bytes
                elif chunk_type == CHUNK_CRC32:
                    if data_sz > 0:
                        src.seek(data_sz, os.SEEK_CUR)
                else:
                    raise ValueError(f"unknown sparse chunk type 0x{chunk_type:04x}")

    print(
        f"Unsparsed {src_path} -> {dst_path} "
        f"(logical {total_bytes // (1024 * 1024)} MiB, blk_sz={blk_sz}, chunks={total_chunks})"
    )


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <fxfs.sparse.blk> <out.blk>", file=sys.stderr)
        sys.exit(2)
    unsparse(sys.argv[1], sys.argv[2])
