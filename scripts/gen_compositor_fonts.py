#!/usr/bin/env python3
"""Pre-bake anti-aliased FreeType glyph atlases for Hirsel-OS compositor surfaces:
  - FONT_UI    : Host Grotesk SemiBold 12.5px (Top Bar pills, headers, buttons)
  - FONT_TITLE : Host Grotesk SemiBold 14.5px (Ceremony / SAS modal titles)
  - FONT_MONO  : Fragment Mono Regular 12.0px (vm-dev VT100 grid + tabular telemetry)

Zero runtime font dependencies; exact FreeType cap_height + bearing metrics for
pixel-accurate optical vertical centering across all compositor surfaces.
"""
import ctypes
import os
import sys

FT_LOAD_RENDER = 1 << 2
FT_LOAD_TARGET_LIGHT = (1 & 15) << 16


class FT_Vector(ctypes.Structure):
    _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]


class FT_Glyph_Metrics(ctypes.Structure):
    _fields_ = [
        ("width", ctypes.c_long),
        ("height", ctypes.c_long),
        ("horiBearingX", ctypes.c_long),
        ("horiBearingY", ctypes.c_long),
        ("horiAdvance", ctypes.c_long),
        ("vertBearingX", ctypes.c_long),
        ("vertBearingY", ctypes.c_long),
        ("vertAdvance", ctypes.c_long),
    ]


class FT_Bitmap(ctypes.Structure):
    _fields_ = [
        ("rows", ctypes.c_uint),
        ("width", ctypes.c_uint),
        ("pitch", ctypes.c_int),
        ("buffer", ctypes.POINTER(ctypes.c_ubyte)),
        ("num_grays", ctypes.c_ushort),
        ("pixel_mode", ctypes.c_ubyte),
        ("palette_mode", ctypes.c_ubyte),
        ("palette", ctypes.c_void_p),
    ]


class FT_GlyphSlotRec(ctypes.Structure):
    _fields_ = [
        ("library", ctypes.c_void_p),
        ("face", ctypes.c_void_p),
        ("next", ctypes.c_void_p),
        ("glyph_index", ctypes.c_uint),
        ("generic_data", ctypes.c_void_p),
        ("generic_finalizer", ctypes.c_void_p),
        ("metrics", FT_Glyph_Metrics),
        ("linearHoriAdvance", ctypes.c_long),
        ("linearVertAdvance", ctypes.c_long),
        ("advance", FT_Vector),
        ("format", ctypes.c_uint),
        ("bitmap", FT_Bitmap),
        ("bitmap_left", ctypes.c_int),
        ("bitmap_top", ctypes.c_int),
    ]


class FT_FaceRec(ctypes.Structure):
    _fields_ = [
        ("num_faces", ctypes.c_long),
        ("face_index", ctypes.c_long),
        ("face_flags", ctypes.c_long),
        ("style_flags", ctypes.c_long),
        ("num_glyphs", ctypes.c_long),
        ("family_name", ctypes.c_char_p),
        ("style_name", ctypes.c_char_p),
        ("num_fixed_sizes", ctypes.c_int),
        ("available_sizes", ctypes.c_void_p),
        ("num_charmaps", ctypes.c_int),
        ("charmaps", ctypes.c_void_p),
        ("generic_data", ctypes.c_void_p),
        ("generic_finalizer", ctypes.c_void_p),
        ("bbox_xMin", ctypes.c_long),
        ("bbox_yMin", ctypes.c_long),
        ("bbox_xMax", ctypes.c_long),
        ("bbox_yMax", ctypes.c_long),
        ("units_per_EM", ctypes.c_ushort),
        ("ascender", ctypes.c_short),
        ("descender", ctypes.c_short),
        ("height", ctypes.c_short),
        ("max_advance_width", ctypes.c_short),
        ("max_advance_height", ctypes.c_short),
        ("underline_position", ctypes.c_short),
        ("underline_thickness", ctypes.c_short),
        ("glyph", ctypes.POINTER(FT_GlyphSlotRec)),
    ]


# Printable ASCII 32..=126 plus key UI punctuation (middot, arrow, chevron)
EXTRA_CHARS = [0x00B7, 0x2192, 0x203A]  # · → ›
CHAR_LIST = list(range(32, 127)) + EXTRA_CHARS


def bake_font(ft, lib, ttf_path: str, px_size: float, wght: int | None):
    face_ptr = ctypes.POINTER(FT_FaceRec)()
    rc = ft.FT_New_Face(lib, ttf_path.encode(), 0, ctypes.byref(face_ptr))
    assert rc == 0, f"FT_New_Face({ttf_path}) failed: {rc}"

    if wght is not None:
        coord = ctypes.c_long(wght << 16)
        ft.FT_Set_Var_Design_Coordinates(face_ptr, 1, ctypes.byref(coord))

    # 26.6 fixed point size at 72 dpi (1 pt == 1 px)
    sz_26_6 = int(round(px_size * 64.0))
    rc = ft.FT_Set_Char_Size(face_ptr, 0, sz_26_6, 72, 72)
    assert rc == 0, f"FT_Set_Char_Size failed: {rc}"

    glyphs = []
    bitmap_pool = bytearray()
    cap_height = 9
    max_adv = 0

    for cp in CHAR_LIST:
        gidx = ft.FT_Get_Char_Index(face_ptr, ctypes.c_ulong(cp))
        rc = ft.FT_Load_Glyph(face_ptr, gidx, FT_LOAD_RENDER | FT_LOAD_TARGET_LIGHT)
        assert rc == 0, f"FT_Load_Glyph({cp}) failed: {rc}"
        slot = face_ptr.contents.glyph.contents
        bm = slot.bitmap
        w = int(bm.width)
        h = int(bm.rows)
        pitch = int(bm.pitch)
        bx = int(slot.bitmap_left)
        by = int(slot.bitmap_top)
        adv = int((slot.advance.x + 32) >> 6)
        max_adv = max(max_adv, adv)
        if cp == ord("H") and by > 0:
            cap_height = by

        off = len(bitmap_pool)
        for row in range(h):
            row_start = row * abs(pitch)
            for col in range(w):
                bitmap_pool.append(bm.buffer[row_start + col])

        glyphs.append((cp, adv, bx, by, w, h, off))

    fam = face_ptr.contents.family_name.decode(errors="replace")
    ft.FT_Done_Face(face_ptr)
    print(f"  Baked {fam} @ {px_size}px (wght={wght}): cap_h={cap_height}px, max_adv={max_adv}px, pool={len(bitmap_pool)}B")
    return cap_height, max_adv, glyphs, bytes(bitmap_pool)


def emit_font_rust(name: str, cap_h: int, max_adv: int, line_h: int, glyphs, pool: bytes) -> str:
    lines = []
    lines.append(f"static {name}_BITMAPS: &[u8] = &[")
    for i in range(0, len(pool), 24):
        chunk = ", ".join(f"{b}" for b in pool[i : i + 24])
        lines.append(f"    {chunk},")
    lines.append("];\n")

    lines.append(f"static {name}_GLYPHS: &[GlyphMeta] = &[")
    for cp, adv, bx, by, w, h, off in glyphs:
        lines.append(
            f"    GlyphMeta {{ cp: {cp}, advance: {adv}, bearing_x: {bx}, bearing_y: {by}, width: {w}, height: {h}, offset: {off} }},"
        )
    lines.append("];\n")

    lines.append(f"pub static {name}: BakedFont = BakedFont {{")
    lines.append(f"    cap_height: {cap_h},")
    lines.append(f"    max_advance: {max_adv},")
    lines.append(f"    line_height: {line_h},")
    lines.append(f"    glyphs: {name}_GLYPHS,")
    lines.append(f"    bitmaps: {name}_BITMAPS,")
    lines.append("};\n")
    return "\n".join(lines)


def main() -> int:
    ft = ctypes.CDLL("libfreetype.so.6")
    lib = ctypes.c_void_p()
    assert ft.FT_Init_FreeType(ctypes.byref(lib)) == 0

    ui_cap, ui_adv, ui_glyphs, ui_pool = bake_font(ft, lib, "/tmp/host-grotesk.ttf", 13.5, 580)
    ti_cap, ti_adv, ti_glyphs, ti_pool = bake_font(ft, lib, "/tmp/host-grotesk.ttf", 16.0, 640)
    mo_cap, mo_adv, mo_glyphs, mo_pool = bake_font(ft, lib, "/tmp/fragment-mono.ttf", 12.5, None)

    ft.FT_Done_FreeType(lib)

    # Sanity-check 'H' glyph pixels aren't blank
    h_meta = next(g for g in ui_glyphs if g[0] == ord("H"))
    h_pixels = ui_pool[h_meta[6] : h_meta[6] + h_meta[4] * h_meta[5]]
    assert max(h_pixels) > 200, "Rasterized 'H' glyph is empty!"

    header = """// Auto-generated by scripts/gen_compositor_fonts.py from Figments' Host Grotesk & Fragment Mono.
// Silky 8-bit FreeType anti-aliased glyph atlases with exact cap_height & bearing metrics.

#[derive(Clone, Copy)]
pub struct GlyphMeta {
    pub cp: u32,
    pub advance: i32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub width: u16,
    pub height: u16,
    pub offset: u32,
}

pub struct BakedFont {
    pub cap_height: i32,
    pub max_advance: i32,
    pub line_height: i32,
    pub glyphs: &'static [GlyphMeta],
    pub bitmaps: &'static [u8],
}

impl BakedFont {
    #[inline]
    pub fn glyph(&self, ch: char) -> Option<&'static GlyphMeta> {
        let cp = ch as u32;
        if (32..=126).contains(&cp) {
            self.glyphs.get((cp - 32) as usize)
        } else {
            self.glyphs.iter().find(|g| g.cp == cp)
        }
    }

    pub fn measure_width(&self, text: &str) -> i32 {
        let mut w = 0i32;
        for ch in text.chars() {
            if let Some(g) = self.glyph(ch) {
                w += g.advance;
            } else {
                w += self.max_advance / 2;
            }
        }
        w
    }
}

"""
    body = (
        header
        + emit_font_rust("FONT_UI", ui_cap, ui_adv, 16, ui_glyphs, ui_pool)
        + "\n"
        + emit_font_rust("FONT_TITLE", ti_cap, ti_adv, 20, ti_glyphs, ti_pool)
        + "\n"
        + emit_font_rust("FONT_MONO", mo_cap, mo_adv, 16, mo_glyphs, mo_pool)
    )

    for dst in (
        "/workspace/hirsel-os/desktop/hirsel_desktop/hirsel_session/src/ui_fonts.rs",
        "/workspace/hirsel-os/third-party/fuchsia/products/hirsel_desktop/hirsel_session/src/ui_fonts.rs",
    ):
        with open(dst, "w") as f:
            f.write(body)
        print(f"Wrote {dst} ({len(body)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
