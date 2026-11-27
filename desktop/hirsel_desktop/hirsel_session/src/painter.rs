// Compositor-owned CPU pixel surfaces (sysmem2 + Flatland Image) for trusted chrome:
//   - Floating 3-island Top Bar (Waybar/Quickshell × Figments "One Sun" capsule pills)
//   - Minimal Foot/Kitty-style `vm-dev` VT100 terminal surface
//   - Unforgeable isolated Web #2 Origin Bar
//   - Topmost rounded Protected Approval Ceremony / SAS Lockdown card
//
// Renders with pre-baked 8-bit anti-aliased FreeType atlases of Figments' real
// `Host Grotesk` (UI/Titles) + `Fragment Mono` (Terminal/Telemetry) with exact
// FreeType cap_height optical vertical centering across all 6 curated OKLCH rooms.

use crate::ui_fonts::{BakedFont, FONT_MONO, FONT_TITLE, FONT_UI};
use crate::vt100::Vt100Screen;
use crate::wm_client::ScenePlan;
use anyhow::{Context, Result};
use fidl::endpoints::{Proxy, create_endpoints};
use fidl_fuchsia_images2 as fimages2;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_sysmem2 as fsysmem2;
use fidl_fuchsia_ui_composition as fland;
use fuchsia_component::client::connect_to_protocol;

#[derive(Clone, Copy)]
pub struct RoomPalette {
    pub label: &'static str,
    pub light: bool,
    pub ground: [u8; 4],
    pub paper: [u8; 4],
    pub raised: [u8; 4],
    pub sunk: [u8; 4],
    pub ink: [u8; 4],
    pub ink2: [u8; 4],
    pub ink3: [u8; 4],
    pub line: [u8; 4],
    pub line_strong: [u8; 4],
    pub cobalt: [u8; 4],
    pub cobalt_tint: [u8; 4],
    pub sap: [u8; 4],
    pub sap_tint: [u8; 4],
    pub vermilion: [u8; 4],
    pub vermilion_tint: [u8; 4],
    pub ochre: [u8; 4],
    pub ochre_tint: [u8; 4],
}

pub fn palette_for(room: &str) -> RoomPalette {
    match room {
        "paper" | "daylight" => RoomPalette {
            label: "Paper",
            light: true,
            ground: [0xf4, 0xee, 0xe6, 0xff],
            paper: [0xfa, 0xf7, 0xf2, 0xff],
            raised: [0xfe, 0xfc, 0xf9, 0xff],
            sunk: [0xeb, 0xe4, 0xdb, 0xff],
            ink: [0x1b, 0x16, 0x13, 0xff],
            ink2: [0x52, 0x4c, 0x46, 0xff],
            ink3: [0x76, 0x6f, 0x68, 0xff],
            line: [0xde, 0xd7, 0xcd, 0xff],
            line_strong: [0xc7, 0xbe, 0xb2, 0xff],
            cobalt: [0x1f, 0x43, 0xd9, 0xff],
            cobalt_tint: [0xdc, 0xe6, 0xfe, 0xff],
            sap: [0x1d, 0x72, 0x3e, 0xff],
            sap_tint: [0xd5, 0xf0, 0xdb, 0xff],
            vermilion: [0xee, 0x4c, 0x22, 0xff],
            vermilion_tint: [0xfd, 0xdc, 0xd1, 0xff],
            ochre: [0xaf, 0x71, 0x12, 0xff],
            ochre_tint: [0xfb, 0xe7, 0xc4, 0xff],
        },
        "folio" => RoomPalette {
            label: "Folio",
            light: true,
            ground: [0xec, 0xe2, 0xd2, 0xff],
            paper: [0xf6, 0xef, 0xe4, 0xff],
            raised: [0xfc, 0xf8, 0xf1, 0xff],
            sunk: [0xe3, 0xd7, 0xc4, 0xff],
            ink: [0x2a, 0x1d, 0x15, 0xff],
            ink2: [0x5d, 0x48, 0x3b, 0xff],
            ink3: [0x7d, 0x66, 0x57, 0xff],
            line: [0xd8, 0xca, 0xb7, 0xff],
            line_strong: [0xc1, 0xb0, 0x99, 0xff],
            cobalt: [0xb2, 0x53, 0x2a, 0xff],
            cobalt_tint: [0xf2, 0xd9, 0xca, 0xff],
            sap: [0x33, 0x6a, 0x3e, 0xff],
            sap_tint: [0xd8, 0xea, 0xd6, 0xff],
            vermilion: [0xc8, 0x3d, 0x22, 0xff],
            vermilion_tint: [0xf6, 0xd7, 0xcd, 0xff],
            ochre: [0x9b, 0x67, 0x18, 0xff],
            ochre_tint: [0xf4, 0xe2, 0xb9, 0xff],
        },
        "gallery" => RoomPalette {
            label: "Gallery",
            light: true,
            ground: [0xf3, 0xf5, 0xf8, 0xff],
            paper: [0xfc, 0xfd, 0xff, 0xff],
            raised: [0xff, 0xff, 0xff, 0xff],
            sunk: [0xe8, 0xec, 0xf2, 0xff],
            ink: [0x1c, 0x20, 0x29, 0xff],
            ink2: [0x4c, 0x53, 0x61, 0xff],
            ink3: [0x6d, 0x74, 0x82, 0xff],
            line: [0xdc, 0xe1, 0xe9, 0xff],
            line_strong: [0xc4, 0xcc, 0xd8, 0xff],
            cobalt: [0x1b, 0x7e, 0x78, 0xff],
            cobalt_tint: [0xd7, 0xf0, 0xee, 0xff],
            sap: [0x22, 0x73, 0x53, 0xff],
            sap_tint: [0xd8, 0xf0, 0xe4, 0xff],
            vermilion: [0xd4, 0x3c, 0x2a, 0xff],
            vermilion_tint: [0xfb, 0xde, 0xda, 0xff],
            ochre: [0xa6, 0x6d, 0x16, 0xff],
            ochre_tint: [0xf8, 0xe9, 0xc8, 0xff],
        },
        "blueprint" => RoomPalette {
            label: "Blueprint",
            light: false,
            ground: [0x14, 0x1f, 0x34, 0xff],
            paper: [0x1b, 0x29, 0x42, 0xff],
            raised: [0x22, 0x33, 0x50, 0xff],
            sunk: [0x0f, 0x18, 0x29, 0xff],
            ink: [0xea, 0xf4, 0xfc, 0xff],
            ink2: [0xa8, 0xc3, 0xdc, 0xff],
            ink3: [0x7c, 0x99, 0xb8, 0xff],
            line: [0x2b, 0x3e, 0x60, 0xff],
            line_strong: [0x3d, 0x55, 0x7f, 0xff],
            cobalt: [0x63, 0xc6, 0xde, 0xff],
            cobalt_tint: [0x1c, 0x3f, 0x56, 0xff],
            sap: [0x61, 0xd2, 0x9d, 0xff],
            sap_tint: [0x16, 0x3c, 0x35, 0xff],
            vermilion: [0xf4, 0x72, 0x46, 0xff],
            vermilion_tint: [0x4b, 0x24, 0x22, 0xff],
            ochre: [0xec, 0xc6, 0x5b, 0xff],
            ochre_tint: [0x42, 0x36, 0x1b, 0xff],
        },
        "conservatory" => RoomPalette {
            label: "Conservatory",
            light: false,
            ground: [0x11, 0x1c, 0x16, 0xff],
            paper: [0x18, 0x26, 0x1e, 0xff],
            raised: [0x20, 0x31, 0x27, 0xff],
            sunk: [0x0c, 0x15, 0x10, 0xff],
            ink: [0xed, 0xe9, 0xd5, 0xff],
            ink2: [0xb4, 0xbf, 0xa8, 0xff],
            ink3: [0x89, 0x96, 0x80, 0xff],
            line: [0x28, 0x3a, 0x2f, 0xff],
            line_strong: [0x3b, 0x51, 0x43, 0xff],
            cobalt: [0x74, 0xc9, 0x7b, 0xff],
            cobalt_tint: [0x1e, 0x3b, 0x26, 0xff],
            sap: [0x6f, 0xcf, 0x89, 0xff],
            sap_tint: [0x1a, 0x39, 0x25, 0xff],
            vermilion: [0xed, 0x6f, 0x49, 0xff],
            vermilion_tint: [0x45, 0x22, 0x1b, 0xff],
            ochre: [0xe6, 0xb8, 0x5c, 0xff],
            ochre_tint: [0x3d, 0x31, 0x18, 0xff],
        },
        _ => RoomPalette {
            label: "Darkroom",
            light: false,
            ground: [0x12, 0x0f, 0x0c, 0xff],
            paper: [0x1b, 0x18, 0x15, 0xff],
            raised: [0x25, 0x21, 0x1d, 0xff],
            sunk: [0x0c, 0x0a, 0x08, 0xff],
            ink: [0xee, 0xe9, 0xe0, 0xff],
            ink2: [0xb9, 0xb3, 0xab, 0xff],
            ink3: [0x8e, 0x87, 0x7f, 0xff],
            line: [0x2d, 0x29, 0x25, 0xff],
            line_strong: [0x41, 0x3c, 0x36, 0xff],
            cobalt: [0x63, 0x93, 0xfd, 0xff],
            cobalt_tint: [0x1b, 0x2b, 0x56, 0xff],
            sap: [0x5d, 0xc4, 0x7e, 0xff],
            sap_tint: [0x13, 0x33, 0x1d, 0xff],
            vermilion: [0xf8, 0x60, 0x32, 0xff],
            vermilion_tint: [0x46, 0x1e, 0x13, 0xff],
            ochre: [0xeb, 0xb3, 0x53, 0xff],
            ochre_tint: [0x3d, 0x2a, 0x08, 0xff],
        },
    }
}

pub fn rgba_to_f(rgba: [u8; 4]) -> fland::ColorRgba {
    fland::ColorRgba {
        red: rgba[0] as f32 / 255.0,
        green: rgba[1] as f32 / 255.0,
        blue: rgba[2] as f32 / 255.0,
        alpha: rgba[3] as f32 / 255.0,
    }
}

pub struct CpuSurface {
    pub width: u32,
    pub height: u32,
    row_pitch: usize,
    vmo: zx::Vmo,
    pub content_id: fland::ContentId,
    _collection: fsysmem2::BufferCollectionProxy,
    _buffer_info: fsysmem2::BufferCollectionInfo,
    _import_token: fland::BufferCollectionImportToken,
}

fn calc_row_pitch(constraints: &fsysmem2::ImageFormatConstraints, width: u32) -> usize {
    let min_bpr = constraints.min_bytes_per_row.unwrap_or(0);
    let divisor = constraints.bytes_per_row_divisor.unwrap_or(1).max(1);
    let raw = (width * 4).max(min_bpr);
    (((raw + divisor - 1) / divisor) * divisor) as usize
}

impl CpuSurface {
    pub async fn new(
        name: &str,
        width: u32,
        height: u32,
        flatland: &fland::FlatlandProxy,
        content_id: fland::ContentId,
        blend: fland::BlendMode,
    ) -> Result<Self> {
        let sysmem = connect_to_protocol::<fsysmem2::AllocatorMarker>()
            .context("connect fuchsia.sysmem2.Allocator")?;

        let (local_token, local_token_req) =
            create_endpoints::<fsysmem2::BufferCollectionTokenMarker>();
        sysmem.allocate_shared_collection(fsysmem2::AllocatorAllocateSharedCollectionRequest {
            token_request: Some(local_token_req),
            ..Default::default()
        })?;
        let local_token = local_token.into_proxy();
        let _ = local_token.set_name(&fsysmem2::NodeSetNameRequest {
            priority: Some(100),
            name: Some(name.to_string()),
            ..Default::default()
        });

        let (scenic_token, scenic_token_req) =
            create_endpoints::<fsysmem2::BufferCollectionTokenMarker>();
        local_token.duplicate(fsysmem2::BufferCollectionTokenDuplicateRequest {
            rights_attenuation_mask: Some(fidl::Rights::SAME_RIGHTS),
            token_request: Some(scenic_token_req),
            ..Default::default()
        })?;
        local_token.sync().await.context("sysmem token sync")?;

        let (export_ep, import_ep) = zx::EventPair::create();
        let export_token = fland::BufferCollectionExportToken { value: export_ep };
        let import_token = fland::BufferCollectionImportToken { value: import_ep };
        let scenic_allocator = connect_to_protocol::<fland::AllocatorMarker>()
            .context("connect fuchsia.ui.composition.Allocator")?;
        scenic_allocator
            .register_buffer_collection(fland::RegisterBufferCollectionArgs {
                export_token: Some(export_token),
                buffer_collection_token2: Some(scenic_token),
                ..Default::default()
            })
            .await
            .context("register_buffer_collection fidl")?
            .map_err(|e| anyhow::anyhow!("register_buffer_collection error: {e:?}"))?;

        let (collection_client, collection_req) =
            create_endpoints::<fsysmem2::BufferCollectionMarker>();
        sysmem.bind_shared_collection(fsysmem2::AllocatorBindSharedCollectionRequest {
            token: Some(
                local_token
                    .into_client_end()
                    .map_err(|_| anyhow::anyhow!("into_client_end(local_token) failed"))?,
            ),
            buffer_collection_request: Some(collection_req),
            ..Default::default()
        })?;
        let collection = collection_client.into_proxy();
        collection.set_constraints(fsysmem2::BufferCollectionSetConstraintsRequest {
            constraints: Some(fsysmem2::BufferCollectionConstraints {
                usage: Some(fsysmem2::BufferUsage {
                    cpu: Some(fsysmem2::CPU_USAGE_WRITE_OFTEN | fsysmem2::CPU_USAGE_READ_OFTEN),
                    ..Default::default()
                }),
                min_buffer_count: Some(1),
                buffer_memory_constraints: Some(fsysmem2::BufferMemoryConstraints {
                    min_size_bytes: Some(width as u64 * height as u64 * 4),
                    physically_contiguous_required: Some(false),
                    secure_required: Some(false),
                    ram_domain_supported: Some(true),
                    cpu_domain_supported: Some(true),
                    inaccessible_domain_supported: Some(false),
                    ..Default::default()
                }),
                image_format_constraints: Some(vec![fsysmem2::ImageFormatConstraints {
                    pixel_format: Some(fimages2::PixelFormat::B8G8R8A8),
                    pixel_format_modifier: Some(fimages2::PixelFormatModifier::Linear),
                    color_spaces: Some(vec![fimages2::ColorSpace::Srgb]),
                    required_min_size: Some(fmath::SizeU { width, height }),
                    required_max_size: Some(fmath::SizeU { width, height }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        })?;

        let buffer_info = collection
            .wait_for_all_buffers_allocated()
            .await
            .context("wait_for_all_buffers_allocated fidl")?
            .map_err(|e| anyhow::anyhow!("sysmem allocation failed: {e:?}"))?
            .buffer_collection_info
            .context("missing buffer_collection_info")?;

        let settings = buffer_info.settings.as_ref().context("missing sysmem settings")?;
        let constraints = settings
            .image_format_constraints
            .as_ref()
            .context("missing image_format_constraints")?;
        let row_pitch = calc_row_pitch(constraints, width);

        let vmo = buffer_info
            .buffers
            .as_ref()
            .and_then(|b| b.first())
            .and_then(|b| b.vmo.as_ref())
            .context("missing sysmem VMO")?
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .context("duplicate sysmem VMO")?;

        let image_props = fland::ImageProperties {
            size: Some(fmath::SizeU { width, height }),
            ..Default::default()
        };
        let dup_import = fland::BufferCollectionImportToken {
            value: import_token.value.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
        };
        flatland
            .create_image(&content_id, dup_import, 0, &image_props)
            .context("flatland.create_image")?;
        flatland
            .set_image_blending_function(&content_id, blend)
            .context("flatland.set_image_blending_function")?;

        Ok(Self {
            width,
            height,
            row_pitch,
            vmo,
            content_id,
            _collection: collection,
            _buffer_info: buffer_info,
            _import_token: import_token,
        })
    }

    pub fn paint_with<F: FnOnce(&mut Canvas<'_>)>(&self, f: F) {
        let mut buf = vec![0u8; self.row_pitch * self.height as usize];
        let mut canvas = Canvas {
            buf: &mut buf,
            width: self.width as usize,
            height: self.height as usize,
            row_pitch: self.row_pitch,
        };
        f(&mut canvas);
        let _ = self.vmo.write(&buf, 0);
    }
}

pub struct Canvas<'a> {
    buf: &'a mut [u8],
    pub width: usize,
    pub height: usize,
    row_pitch: usize,
}

impl<'a> Canvas<'a> {
    #[inline]
    pub fn put_px(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8, a: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let off = y * self.row_pitch + x * 4;
        if off + 4 <= self.buf.len() {
            // B8G8R8A8
            self.buf[off] = b;
            self.buf[off + 1] = g;
            self.buf[off + 2] = r;
            self.buf[off + 3] = a;
        }
    }

    #[inline]
    pub fn blend_px(&mut self, x: usize, y: usize, rgba: [u8; 4], cov: f32) {
        if x >= self.width || y >= self.height || cov <= 0.002 {
            return;
        }
        let alpha = (rgba[3] as f32 / 255.0) * cov.clamp(0.0, 1.0);
        let off = y * self.row_pitch + x * 4;
        if off + 4 > self.buf.len() {
            return;
        }
        if alpha >= 0.996 {
            self.buf[off] = rgba[2];
            self.buf[off + 1] = rgba[1];
            self.buf[off + 2] = rgba[0];
            self.buf[off + 3] = 255;
            return;
        }
        let db = self.buf[off] as f32;
        let dg = self.buf[off + 1] as f32;
        let dr = self.buf[off + 2] as f32;
        let da = self.buf[off + 3] as f32 / 255.0;
        let out_a = alpha + da * (1.0 - alpha);
        if out_a <= 0.001 {
            return;
        }
        let inv = 1.0 - alpha;
        let ob = (rgba[2] as f32 * alpha + db * da * inv) / out_a;
        let og = (rgba[1] as f32 * alpha + dg * da * inv) / out_a;
        let or = (rgba[0] as f32 * alpha + dr * da * inv) / out_a;
        self.buf[off] = ob.clamp(0.0, 255.0) as u8;
        self.buf[off + 1] = og.clamp(0.0, 255.0) as u8;
        self.buf[off + 2] = or.clamp(0.0, 255.0) as u8;
        self.buf[off + 3] = (out_a * 255.0).clamp(0.0, 255.0) as u8;
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, rgba: [u8; 4]) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for py in y..y_end {
            for px in x..x_end {
                self.put_px(px, py, rgba[0], rgba[1], rgba[2], rgba[3]);
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, x: usize, y: usize, w: usize, h: usize, radius: f32, rgba: [u8; 4]) {
        if w == 0 || h == 0 {
            return;
        }
        let r = radius.min(w as f32 * 0.5).min(h as f32 * 0.5);
        let cx = x as f32 + w as f32 * 0.5;
        let cy = y as f32 + h as f32 * 0.5;
        let hx = w as f32 * 0.5 - r;
        let hy = h as f32 * 0.5 - r;
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for py in y..y_end {
            let dy = (py as f32 + 0.5 - cy).abs() - hy;
            let qy = dy.max(0.0);
            for px in x..x_end {
                let dx = (px as f32 + 0.5 - cx).abs() - hx;
                let qx = dx.max(0.0);
                let dist = (qx * qx + qy * qy).sqrt() + dx.max(dy).min(0.0) - r;
                if dist <= -0.5 {
                    self.blend_px(px, py, rgba, 1.0);
                } else if dist < 0.5 {
                    self.blend_px(px, py, rgba, 0.5 - dist);
                }
            }
        }
    }

    pub fn stroke_rounded_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: f32,
        thickness: f32,
        rgba: [u8; 4],
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let r = radius.min(w as f32 * 0.5).min(h as f32 * 0.5);
        let cx = x as f32 + w as f32 * 0.5;
        let cy = y as f32 + h as f32 * 0.5;
        let hx = w as f32 * 0.5 - r;
        let hy = h as f32 * 0.5 - r;
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for py in y..y_end {
            let dy = (py as f32 + 0.5 - cy).abs() - hy;
            let qy = dy.max(0.0);
            for px in x..x_end {
                let dx = (px as f32 + 0.5 - cx).abs() - hx;
                let qx = dx.max(0.0);
                let dist = (qx * qx + qy * qy).sqrt() + dx.max(dy).min(0.0) - r;
                if dist <= 0.5 && dist >= -thickness - 0.5 {
                    let outer = (0.5 - dist).clamp(0.0, 1.0);
                    let inner = (dist + thickness + 0.5).clamp(0.0, 1.0);
                    let cov = outer.min(inner);
                    if cov > 0.002 {
                        self.blend_px(px, py, rgba, cov);
                    }
                }
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, rgba: [u8; 4]) {
        let x0 = (cx - radius - 1.0).max(0.0) as usize;
        let y0 = (cy - radius - 1.0).max(0.0) as usize;
        let x1 = ((cx + radius + 2.0) as usize).min(self.width);
        let y1 = ((cy + radius + 2.0) as usize).min(self.height);
        for py in y0..y1 {
            let dy = py as f32 + 0.5 - cy;
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - cx;
                let dist = (dx * dx + dy * dy).sqrt() - radius;
                if dist <= -0.5 {
                    self.blend_px(px, py, rgba, 1.0);
                } else if dist < 0.5 {
                    self.blend_px(px, py, rgba, 0.5 - dist);
                }
            }
        }
    }

    /// Draw a single anti-aliased FreeType glyph at `(pen_x, baseline_y)`.
    #[inline]
    pub fn draw_glyph_aa(&mut self, font: &BakedFont, ch: char, pen_x: i32, baseline_y: i32, rgba: [u8; 4]) -> i32 {
        let Some(g) = font.glyph(ch) else {
            return font.max_advance / 2;
        };
        let gw = g.width as usize;
        let gh = g.height as usize;
        let off = g.offset as usize;
        let gx0 = pen_x + g.bearing_x;
        let gy0 = baseline_y - g.bearing_y;
        for row in 0..gh {
            let py = gy0 + row as i32;
            if py < 0 || py >= self.height as i32 {
                continue;
            }
            let row_off = off + row * gw;
            for col in 0..gw {
                let cov_u8 = font.bitmaps[row_off + col];
                if cov_u8 == 0 {
                    continue;
                }
                let px = gx0 + col as i32;
                if px >= 0 && px < self.width as i32 {
                    self.blend_px(px as usize, py as usize, rgba, cov_u8 as f32 / 255.0);
                }
            }
        }
        g.advance
    }

    /// Draw anti-aliased text along `baseline_y` starting at `x`, clipped to `max_x`.
    pub fn draw_text_aa(&mut self, font: &BakedFont, mut x: i32, baseline_y: i32, max_x: i32, text: &str, rgba: [u8; 4]) -> i32 {
        for ch in text.chars() {
            let adv = font.glyph(ch).map(|g| g.advance).unwrap_or(font.max_advance / 2);
            if x + adv > max_x {
                break;
            }
            x += self.draw_glyph_aa(font, ch, x, baseline_y, rgba);
        }
        x
    }

    /// Optically vertically center text inside `(rect_y .. rect_y + rect_h)` using exact FreeType `cap_height`.
    pub fn draw_text_cy(&mut self, font: &BakedFont, x: i32, rect_y: usize, rect_h: usize, max_x: i32, text: &str, rgba: [u8; 4]) -> i32 {
        let baseline = rect_y as i32 + ((rect_h as i32 + font.cap_height) / 2);
        self.draw_text_aa(font, x, baseline, max_x, text, rgba)
    }

    /// Optically center text both horizontally and vertically inside a box (buttons, centered pills).
    pub fn draw_text_center(&mut self, font: &BakedFont, rx: usize, ry: usize, rw: usize, rh: usize, text: &str, rgba: [u8; 4]) {
        let tw = font.measure_width(text);
        let x = rx as i32 + ((rw as i32 - tw) / 2).max(4);
        let baseline = ry as i32 + ((rh as i32 + font.cap_height) / 2);
        self.draw_text_aa(font, x, baseline, (rx + rw) as i32 - 4, text, rgba);
    }
}

fn fmt_dollars(micros: u64) -> String {
    let cents = micros / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// Floating 3-island Top Bar (Waybar / Quickshell × Figments "One Sun") in real Host Grotesk + Fragment Mono.
/// Click hitboxes (kept clean & deterministic):
///   - `1 Figments`: x = 90..198
///   - `2 vm-dev`:   x = 198..290
///   - `3 Web`:      x = 290..378
///   - `egress`:     right island middle (w - 190 .. w - 70)
///   - `sys` (gear): far right of right island (w - 70 .. w - 10) -> toggles Top-Right Settings panel!
pub fn paint_top_bar(c: &mut Canvas<'_>, plan: &ScenePlan, theme: &str, powerbox_toast: &str) {
    let pal = palette_for(theme);
    let locked = plan.sas || plan.outbox.status == "staged";

    c.fill_rect(0, 0, c.width, c.height, pal.ground);

    let bar_y = 8usize;
    let bar_h = 28usize;
    let rad = 14.0f32;
    let cy_f = (bar_y + bar_h / 2) as f32;

    // ── LEFT ISLAND: Brand + Workspace / Domain pills ──
    let left_x = 10usize;
    let left_w = 368usize;
    c.fill_rounded_rect(left_x, bar_y, left_w, bar_h, rad, pal.paper);
    c.stroke_rounded_rect(left_x, bar_y, left_w, bar_h, rad, 1.0, pal.line_strong);

    let brand_dot = if locked { pal.vermilion } else { pal.cobalt };
    c.fill_circle((left_x + 16) as f32, cy_f, 3.8, brand_dot);
    c.draw_text_cy(&FONT_UI, (left_x + 26) as i32, bar_y, bar_h, 90, "hirsel", pal.ink);
    c.fill_rect(left_x + 76, bar_y + 7, 1, bar_h - 14, pal.line_strong);

    let pills: [(&str, usize, usize, bool, [u8; 4], [u8; 4]); 3] = [
        ("1 Figments", 94, 102, plan.focused == "figments" && !locked, pal.cobalt_tint, pal.cobalt),
        ("2 vm-dev", 200, 88, plan.layout.vm_dev.is_some(), pal.sap_tint, pal.sap),
        ("3 Web", 292, 80, plan.layout.web.is_some(), pal.ochre_tint, pal.ochre),
    ];
    for (label, px, pw, active, tint, accent) in pills {
        if active {
            c.fill_rounded_rect(px, bar_y + 3, pw, bar_h - 6, 11.0, tint);
            c.fill_circle((px + 11) as f32, cy_f, 3.0, accent);
            c.draw_text_cy(&FONT_UI, (px + 20) as i32, bar_y, bar_h, (px + pw - 6) as i32, label, pal.ink);
        } else {
            c.fill_circle((px + 11) as f32, cy_f, 2.4, pal.line_strong);
            c.draw_text_cy(&FONT_UI, (px + 20) as i32, bar_y, bar_h, (px + pw - 6) as i32, label, pal.ink2);
        }
    }

    // ── CENTER DYNAMIC ISLAND: Room · Focus, Powerbox Toast, or Trusted Chrome Lock ──
    let (mid_txt, mid_bg, mid_border, mid_dot, mid_fg) = if locked {
        (
            "Trusted Chrome · Capture Locked".to_string(),
            pal.vermilion_tint,
            pal.vermilion,
            pal.vermilion,
            pal.ink,
        )
    } else if !powerbox_toast.is_empty() {
        (
            format!("Powerbox · {powerbox_toast}"),
            pal.sap_tint,
            pal.sap,
            pal.sap,
            pal.ink,
        )
    } else {
        let focus_lbl = match plan.focused.as_str() {
            "figments" => "Figments",
            "vm_dev" => "vm-dev",
            "web_untrusted" => "Web #2",
            _ => "Trusted",
        };
        (
            format!("{} · {}", pal.label, focus_lbl),
            pal.paper,
            pal.line_strong,
            pal.cobalt,
            pal.ink2,
        )
    };
    let mid_tw = FONT_UI.measure_width(&mid_txt) as usize;
    let mid_w = (mid_tw + 38).min(380);
    let mid_x = (c.width.saturating_sub(mid_w)) / 2;
    if mid_x > left_x + left_w + 10 {
        c.fill_rounded_rect(mid_x, bar_y, mid_w, bar_h, rad, mid_bg);
        c.stroke_rounded_rect(mid_x, bar_y, mid_w, bar_h, rad, 1.0, mid_border);
        c.fill_circle((mid_x + 14) as f32, cy_f, 3.2, mid_dot);
        c.draw_text_cy(
            &FONT_UI,
            (mid_x + 24) as i32,
            bar_y,
            bar_h,
            (mid_x + mid_w - 10) as i32,
            &mid_txt,
            mid_fg,
        );
    }

    // ── RIGHT ISLAND: Bank Budget · Egress Caretaker · Capture · Settings Trigger ──
    let bank_txt = format!(
        "{}/{}",
        fmt_dollars(plan.bank_spent_micros),
        fmt_dollars(plan.bank_limit_micros)
    );
    let conn_txt = if plan.agent_conn { "egress" } else { "revoked" };
    let conn_dot = if plan.agent_conn { pal.sap } else { pal.vermilion };
    let cap_txt = if plan.capture_allowed { "sys" } else { "locked" };
    let cap_col = if plan.capture_allowed { pal.ink2 } else { pal.vermilion };

    let bank_w = FONT_MONO.measure_width(&bank_txt) as usize;
    let conn_w = FONT_UI.measure_width(conn_txt) as usize;
    let cap_w = FONT_UI.measure_width(cap_txt) as usize;
    let right_w = 14 + bank_w + 10 + 1 + 10 + 11 + conn_w + 10 + 1 + 10 + cap_w + 14;
    let right_x = c.width.saturating_sub(right_w + 10);

    c.fill_rounded_rect(right_x, bar_y, right_w, bar_h, rad, pal.paper);
    c.stroke_rounded_rect(right_x, bar_y, right_w, bar_h, rad, 1.0, pal.line_strong);

    let mut rx = (right_x + 14) as i32;
    let rmax = (right_x + right_w - 8) as i32;
    rx = c.draw_text_cy(&FONT_MONO, rx, bar_y, bar_h, rmax, &bank_txt, pal.ink2) + 10;
    c.fill_rect(rx as usize, bar_y + 7, 1, bar_h - 14, pal.line_strong);
    rx += 11;

    c.fill_circle((rx + 3) as f32, cy_f, 3.2, conn_dot);
    rx += 11;
    rx = c.draw_text_cy(
        &FONT_UI,
        rx,
        bar_y,
        bar_h,
        rmax,
        conn_txt,
        if plan.agent_conn { pal.ink } else { pal.vermilion },
    ) + 10;
    c.fill_rect(rx as usize, bar_y + 7, 1, bar_h - 14, pal.line_strong);
    rx += 11;

    c.draw_text_cy(&FONT_UI, rx, bar_y, bar_h, rmax, cap_txt, cap_col);
}

/// Sleek compositor-owned unforgeable Origin Bar above Isolated Web #2.
pub fn paint_web_origin_bar(c: &mut Canvas<'_>, theme: &str, origin: &str, focused: bool) {
    let pal = palette_for(theme);
    let bar_bg = if focused { pal.ochre_tint } else { pal.paper };
    let border = if focused { pal.ochre } else { pal.line_strong };
    c.fill_rect(0, 0, c.width, c.height, bar_bg);
    c.fill_rect(0, c.height.saturating_sub(1), c.width, 1, border);
    c.fill_circle(14.0, (c.height as f32) * 0.5, 3.5, pal.ochre);

    let mut x = c.draw_text_cy(&FONT_UI, 25, 0, c.height, c.width as i32 - 12, "isolated · ", pal.ink2);
    c.draw_text_cy(&FONT_MONO, x, 0, c.height, c.width as i32 - 110, origin, pal.ink);

    let badge = "zero /_hirsel/*";
    let bw = FONT_MONO.measure_width(badge) as usize;
    if c.width > bw + 260 {
        x = (c.width - bw - 14) as i32;
        c.draw_text_cy(&FONT_MONO, x, 0, c.height, c.width as i32 - 6, badge, pal.ink3);
    }
}

/// Minimal Foot / Kitty-style `vm-dev` VT100 terminal in crisp anti-aliased Fragment Mono + Host Grotesk.
pub fn paint_dev_pane(
    c: &mut Canvas<'_>,
    plan: &ScenePlan,
    theme: &str,
    vt: &Vt100Screen,
    input_buf: &str,
    vm_ready: bool,
) {
    let pal = palette_for(theme);
    let focused = plan.focused == "vm_dev";

    c.fill_rect(0, 0, c.width, c.height, pal.paper);

    // Subtle 32px frameless top strip
    let hdr_h = 32usize;
    c.fill_rect(0, 0, c.width, hdr_h, pal.raised);
    c.fill_rect(0, hdr_h - 1, c.width, 1, pal.line);

    let dot_col = if vm_ready { pal.sap } else { pal.ochre };
    c.fill_circle(16.0, (hdr_h as f32) * 0.5, 3.5, dot_col);
    c.draw_text_cy(&FONT_UI, 27, 0, hdr_h, 160, "vm-dev", pal.ink);

    let meta = format!("linux 6.1.102 · 2 vcpu · {}x{}", vt.cols, vt.rows);
    let mw = FONT_MONO.measure_width(&meta) as usize;
    if c.width > mw + 100 {
        c.draw_text_cy(
            &FONT_MONO,
            (c.width - mw - 14) as i32,
            0,
            hdr_h,
            c.width as i32 - 8,
            &meta,
            pal.ink3,
        );
    }

    // VT100 screen cell grid rendered with anti-aliased Fragment Mono (8px cell pitch, 16px row pitch)
    let pad_x = 16usize;
    let pad_y = hdr_h + 12;
    let prompt_h = 32usize;
    let cell_w = FONT_MONO.max_advance as usize;
    let row_h = FONT_MONO.line_height as usize;
    let avail_h = c.height.saturating_sub(pad_y + prompt_h + 12);
    let max_rows = avail_h / row_h;
    let draw_rows = vt.rows.min(max_rows);

    for r in 0..draw_rows {
        let baseline_y = (pad_y + r * row_h) as i32 + FONT_MONO.cap_height + 2;
        for col in 0..vt.cols {
            let px = pad_x + col * cell_w;
            if px + cell_w > c.width.saturating_sub(pad_x) {
                break;
            }
            let cell = vt.cells[r * vt.cols + col];
            if cell.ch != b' ' {
                let fg = if cell.fg == crate::vt100::DEFAULT_FG {
                    pal.ink
                } else if pal.light {
                    [
                        cell.fg[0].saturating_sub(55),
                        cell.fg[1].saturating_sub(55),
                        cell.fg[2].saturating_sub(55),
                        255,
                    ]
                } else {
                    cell.fg
                };
                c.draw_glyph_aa(&FONT_MONO, cell.ch as char, px as i32, baseline_y, fg);
            }
        }
    }

    // Minimal floating prompt capsule at bottom of terminal
    let py = c.height.saturating_sub(prompt_h + 8);
    let pw = c.width.saturating_sub(24);
    c.fill_rounded_rect(12, py, pw, 28, 9.0, pal.sunk);
    c.stroke_rounded_rect(
        12,
        py,
        pw,
        28,
        9.0,
        1.0,
        if focused { pal.sap } else { pal.line },
    );
    let x_after = c.draw_text_cy(&FONT_MONO, 22, py, 28, 40, "›", pal.sap);
    let cursor = if focused { "_" } else { "" };
    let line = format!(" {input_buf}{cursor}");
    c.draw_text_cy(&FONT_MONO, x_after + 2, py, 28, (12 + pw - 10) as i32, &line, pal.ink);
}

/// Topmost Protected Approval Ceremony / SAS Vault Modal (rounded alpha-blended obsidian card).
pub fn paint_ceremony_card(
    c: &mut Canvas<'_>,
    plan: &ScenePlan,
    sas_host: &str,
    sas_secret_len: usize,
    sas_status: &str,
) {
    let is_outbox = plan.outbox.status == "staged";
    let pal = palette_for("darkroom");

    c.fill_rect(0, 0, c.width, c.height, [0, 0, 0, 0]);

    let w = c.width;
    let h = c.height;
    c.fill_rounded_rect(0, 0, w, h, 16.0, pal.paper);
    c.stroke_rounded_rect(0, 0, w, h, 16.0, 2.0, pal.vermilion);

    // Header band
    c.fill_rounded_rect(16, 14, w.saturating_sub(32), 38, 10.0, pal.vermilion_tint);
    c.fill_circle(32.0, 33.0, 4.0, pal.vermilion);
    let title = if is_outbox {
        "Approval Ceremony · Protected Outbox"
    } else {
        "Secure Vault (SAS) · Direct Keyboard"
    };
    c.draw_text_cy(&FONT_TITLE, 45, 14, 38, w as i32 - 24, title, pal.ink);

    c.draw_text_cy(
        &FONT_UI,
        20,
        58,
        22,
        w as i32 - 20,
        "Compositor-owned trusted chrome · Screen capture disabled",
        pal.ink3,
    );
    c.fill_rect(20, 84, w.saturating_sub(40), 1, pal.line);

    if is_outbox {
        let rows = [
            ("Effect", format!("#{}", plan.outbox.id), pal.ink),
            ("Target", plan.outbox.target.clone(), pal.cobalt),
            ("Action", plan.outbox.summary.clone(), pal.ochre),
            ("Digest", format!("0x{:08X}", plan.outbox.digest), pal.sap),
            (
                "Budget",
                format!(
                    "{}  (spent {} / {})",
                    fmt_dollars(plan.outbox.cost_micros),
                    fmt_dollars(plan.bank_spent_micros),
                    fmt_dollars(plan.bank_limit_micros)
                ),
                pal.ink,
            ),
        ];
        for (idx, (k, v, vcol)) in rows.iter().enumerate() {
            let ry = 94 + idx * 25;
            c.draw_text_cy(&FONT_UI, 22, ry, 20, 96, k, pal.ink3);
            c.draw_text_cy(&FONT_MONO, 102, ry, 20, w as i32 - 22, v, *vcol);
        }

        let by = h.saturating_sub(60);
        c.fill_rounded_rect(18, by, 244, 40, 10.0, pal.sap_tint);
        c.stroke_rounded_rect(18, by, 244, 40, 10.0, 1.5, pal.sap);
        c.draw_text_center(&FONT_UI, 18, by, 244, 40, "[Enter] Approve Hash", pal.ink);

        c.fill_rounded_rect(276, by, 266, 40, 10.0, pal.vermilion_tint);
        c.stroke_rounded_rect(276, by, 266, 40, 10.0, 1.5, pal.vermilion);
        c.draw_text_center(&FONT_UI, 276, by, 266, 40, "[Esc] Deny Effect", pal.ink);
    } else {
        c.draw_text_cy(
            &FONT_UI,
            20,
            92,
            22,
            w as i32 - 20,
            "Seals straight to isolated egress_gw.cm over private FIDL.",
            pal.ink2,
        );
        c.draw_text_cy(&FONT_UI, 20, 118, 22, 110, "Host [Tab]", pal.ink3);
        c.draw_text_cy(&FONT_MONO, 112, 118, 22, w as i32 - 22, sas_host, pal.cobalt);

        c.fill_rounded_rect(20, 148, w.saturating_sub(40), 34, 10.0, pal.sunk);
        c.stroke_rounded_rect(20, 148, w.saturating_sub(40), 34, 10.0, 1.0, pal.vermilion);
        let stars: String = "*".repeat(sas_secret_len.min(36));
        let mask_line = format!("Key   {stars}_");
        c.draw_text_cy(&FONT_MONO, 32, 148, 34, w as i32 - 30, &mask_line, pal.sap);

        let status_line = if sas_status.is_empty() {
            format!(
                "Caretaker: {} · Approved: {} · Powerbox: {}",
                if plan.agent_conn { "active" } else { "revoked" },
                plan.approved,
                plan.powerbox_transfers
            )
        } else {
            sas_status.to_string()
        };
        c.draw_text_cy(&FONT_UI, 20, 194, 22, w as i32 - 20, &status_line, pal.ochre);

        let by = h.saturating_sub(60);
        c.fill_rounded_rect(18, by, 244, 40, 10.0, pal.sap_tint);
        c.stroke_rounded_rect(18, by, 244, 40, 10.0, 1.5, pal.sap);
        c.draw_text_center(&FONT_UI, 18, by, 244, 40, "[Enter] Seal Key", pal.ink);

        c.fill_rounded_rect(276, by, 266, 40, 10.0, pal.raised);
        c.stroke_rounded_rect(276, by, 266, 40, 10.0, 1.0, pal.line_strong);
        c.draw_text_center(&FONT_UI, 276, by, 266, 40, "[F6 / Esc] Close", pal.ink2);
    }
}
