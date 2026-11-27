// Compositor-owned CPU pixel surfaces (sysmem2 + Flatland Image) for trusted chrome:
//   - Top 36px Trust Bar (unforgeable domain pills, Bank spend, Caretaker status, capture badge)
//   - Split vm-dev terminal surface
//   - Topmost Protected Approval Ceremony / SAS Lockdown card

use crate::font8x16;
use crate::wm_client::ScenePlan;
use anyhow::{Context, Result};
use fidl::endpoints::{Proxy, create_endpoints};
use fidl_fuchsia_images2 as fimages2;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_sysmem2 as fsysmem2;
use fidl_fuchsia_ui_composition as fland;
use fuchsia_component::client::connect_to_protocol;

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

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, rgba: [u8; 4]) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for py in y..y_end {
            for px in x..x_end {
                self.put_px(px, py, rgba[0], rgba[1], rgba[2], rgba[3]);
            }
        }
    }

    pub fn stroke_rect(&mut self, x: usize, y: usize, w: usize, h: usize, t: usize, rgba: [u8; 4]) {
        if w == 0 || h == 0 {
            return;
        }
        self.fill_rect(x, y, w, t, rgba);
        self.fill_rect(x, y + h.saturating_sub(t), w, t, rgba);
        self.fill_rect(x, y, t, h, rgba);
        self.fill_rect(x + w.saturating_sub(t), y, t, h, rgba);
    }

    pub fn draw_ansi_text(&mut self, mut x: usize, y: usize, text: &str, default_rgba: [u8; 4]) {
        let mut color = default_rgba;
        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // Parse ANSI CSI SGR sequence: ESC [ <codes> m
                i += 2;
                let start = i;
                while i < bytes.len() && !(bytes[i] >= b'@' && bytes[i] <= b'~') {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'm' {
                    if let Ok(params) = std::str::from_utf8(&bytes[start..i]) {
                        for code in params.split(';') {
                            match code {
                                "" | "0" => color = default_rgba,
                                "30" => color = [71, 85, 105, 255],
                                "31" | "91" => color = [248, 113, 113, 255],
                                "32" | "92" => color = [74, 222, 128, 255],
                                "33" | "93" => color = [250, 204, 21, 255],
                                "34" | "94" => color = [96, 165, 250, 255],
                                "35" | "95" => color = [192, 132, 252, 255],
                                "36" | "96" => color = [34, 211, 238, 255],
                                "37" | "97" => color = [248, 250, 252, 255],
                                _ => {}
                            }
                        }
                    }
                }
                i += 1;
                continue;
            }
            if x + 8 > self.width {
                break;
            }
            let bits = font8x16::glyph(bytes[i]);
            for (row, &mask) in bits.iter().enumerate() {
                for col in 0..8 {
                    if (mask & (0x80 >> col)) != 0 {
                        self.put_px(x + col, y + row, color[0], color[1], color[2], color[3]);
                    }
                }
            }
            x += 8;
            i += 1;
        }
    }

    pub fn draw_text(&mut self, mut x: usize, y: usize, text: &str, scale: usize, rgba: [u8; 4]) {
        let step = 8 * scale;
        for byte in text.bytes() {
            if x + step > self.width {
                break;
            }
            let bits = font8x16::glyph(byte);
            for (row, &mask) in bits.iter().enumerate() {
                for col in 0..8 {
                    if (mask & (0x80 >> col)) != 0 {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                self.put_px(
                                    x + col * scale + sx,
                                    y + row * scale + sy,
                                    rgba[0],
                                    rgba[1],
                                    rgba[2],
                                    rgba[3],
                                );
                            }
                        }
                    }
                }
            }
            x += step;
        }
    }
}

fn fmt_dollars(micros: u64) -> String {
    let cents = micros / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

pub fn paint_top_bar(c: &mut Canvas<'_>, plan: &ScenePlan) {
    let daylight = plan.room == "daylight";
    let bg = if plan.sas || plan.outbox.status == "staged" {
        [68, 16, 22, 255]
    } else if daylight {
        [232, 237, 245, 255]
    } else {
        [15, 23, 36, 255]
    };
    let fg = if daylight && !plan.sas && plan.outbox.status != "staged" {
        [20, 29, 44, 255]
    } else {
        [236, 242, 250, 255]
    };
    let muted = if daylight && !plan.sas && plan.outbox.status != "staged" {
        [82, 97, 120, 255]
    } else {
        [148, 163, 184, 255]
    };

    c.fill_rect(0, 0, c.width, c.height, bg);
    c.fill_rect(0, c.height.saturating_sub(2), c.width, 2, [59, 130, 246, 255]);

    // Brand badge
    c.fill_rect(8, 6, 108, 24, [37, 99, 235, 255]);
    c.draw_text(16, 14, "HIRSEL-OS", 1, [255, 255, 255, 255]);

    // Domain pills
    let mut x = 128usize;
    let pills = [
        ("F1:FIGMENTS", plan.focused == "figments", [59, 130, 246, 255]),
        ("F2:VM-DEV", plan.layout.vm_dev.is_some(), [16, 185, 129, 255]),
        ("F3:WEB", plan.layout.web.is_some(), [245, 158, 11, 255]),
    ];
    for (label, active, color) in pills {
        let w = label.len() * 8 + 16;
        let pill_bg = if active { color } else { [30, 41, 59, 255] };
        let pill_fg = if active { [255, 255, 255, 255] } else { [180, 195, 214, 255] };
        c.fill_rect(x, 7, w, 22, pill_bg);
        c.draw_text(x + 8, 14, label, 1, pill_fg);
        x += w + 8;
    }

    // Center focus + room indicator
    let mode_txt = format!(
        "FOCUS:{}  ROOM:{}",
        plan.focused.to_uppercase(),
        plan.room.to_uppercase()
    );
    c.draw_text(x + 8, 14, &mode_txt, 1, fg);

    // Right status badges: Bank + Caretaker Conn + Capture
    let bank_txt = format!(
        "BANK {}/{}",
        fmt_dollars(plan.bank_spent_micros),
        fmt_dollars(plan.bank_limit_micros)
    );
    let conn_txt = if plan.agent_conn { "CONN:ACTIVE" } else { "CONN:REVOKED" };
    let conn_col = if plan.agent_conn { [16, 185, 129, 255] } else { [239, 68, 68, 255] };
    let cap_txt = if plan.capture_allowed { "CAP:OK" } else { "CAP:LOCKED" };
    let cap_col = if plan.capture_allowed { [51, 65, 85, 255] } else { [220, 38, 38, 255] };

    let mut rx = c.width.saturating_sub(12);
    for (txt, col) in [(cap_txt, cap_col), (conn_txt, conn_col)] {
        let bw = txt.len() * 8 + 14;
        rx = rx.saturating_sub(bw);
        c.fill_rect(rx, 7, bw, 22, col);
        c.draw_text(rx + 7, 14, txt, 1, [255, 255, 255, 255]);
        rx = rx.saturating_sub(8);
    }

    let bank_w = bank_txt.len() * 8 + 12;
    rx = rx.saturating_sub(bank_w);
    c.draw_text(rx, 14, &bank_txt, 1, muted);
}

pub fn paint_dev_pane(c: &mut Canvas<'_>, plan: &ScenePlan, lines: &[String], input_buf: &str, vm_ready: bool) {
    c.fill_rect(0, 0, c.width, c.height, [10, 17, 24, 255]);
    c.fill_rect(0, 0, c.width, 30, [16, 78, 59, 255]);
    let header = if vm_ready {
        "VM-DEV // LINUX 6.1.102 AMD-SVM MICROVM (LIVE /dev/hvc0 + /mnt/data)"
    } else {
        "VM-DEV // BOOTING LINUX 6.1.102 AMD-SVM MICROVM..."
    };
    c.draw_text(12, 11, header, 1, [236, 253, 245, 255]);

    let status = format!(
        "VM:{} | Powerbox:{} | Outbox:{}",
        if vm_ready { "READY" } else { "BOOTING" },
        plan.powerbox_transfers,
        plan.outbox.status.to_uppercase()
    );
    c.draw_text(12, 40, &status, 1, [110, 231, 183, 255]);
    c.fill_rect(12, 54, c.width.saturating_sub(24), 1, [30, 64, 55, 255]);

    let mut y = 64usize;
    let max_lines = (c.height.saturating_sub(114)) / 14;
    let start = lines.len().saturating_sub(max_lines);
    for line in &lines[start..] {
        c.draw_ansi_text(12, y, line, [209, 250, 229, 255]);
        y += 14;
    }

    // Live interactive prompt line when vm-dev is focused
    let prompt_y = c.height.saturating_sub(44);
    c.fill_rect(8, prompt_y.saturating_sub(4), c.width.saturating_sub(16), 20, [14, 34, 42, 255]);
    let cursor = if plan.focused == "vm_dev" { "_" } else { "" };
    let prompt = format!("vm-dev# {input_buf}{cursor}");
    c.draw_text(14, prompt_y + 2, &prompt, 1, [167, 243, 208, 255]);

    let footer_y = c.height.saturating_sub(20);
    c.fill_rect(0, footer_y.saturating_sub(4), c.width, 24, [15, 28, 38, 255]);
    c.draw_text(
        12,
        footer_y + 4,
        "F1:Figments F2:Dev F3:Web F4:Room F5:Stage F6:SAS F7:Revoke",
        1,
        [148, 163, 184, 255],
    );
}

pub fn paint_ceremony_card(c: &mut Canvas<'_>, plan: &ScenePlan) {
    let is_outbox = plan.outbox.status == "staged";
    c.fill_rect(0, 0, c.width, c.height, [20, 14, 22, 255]);
    c.stroke_rect(0, 0, c.width, c.height, 4, [239, 68, 68, 255]);

    // Header band
    c.fill_rect(4, 4, c.width.saturating_sub(8), 44, [153, 27, 27, 255]);
    let title = if is_outbox {
        "PROTECTED EFFECT OUTBOX APPROVAL CEREMONY"
    } else {
        "SECURE ATTENTION SEQUENCE (SAS) LOCKDOWN"
    };
    c.draw_text(18, 18, title, 1, [255, 255, 255, 255]);

    c.draw_text(
        18,
        62,
        "COMPOSITOR-OWNED TRUSTED PIXELS // SCREEN CAPTURE DISABLED",
        1,
        [252, 165, 165, 255],
    );
    c.fill_rect(18, 78, c.width.saturating_sub(36), 1, [90, 40, 50, 255]);

    if is_outbox {
        let l1 = format!("EFFECT ID : #{}", plan.outbox.id);
        let l2 = format!("TARGET    : {}", plan.outbox.target);
        let l3 = format!("ACTION    : {}", plan.outbox.summary);
        let l4 = format!("DIGEST    : 0x{:08X} ({})", plan.outbox.digest, plan.outbox.digest);
        let l5 = format!(
            "COST/BANK : {}  (spent {} / {})",
            fmt_dollars(plan.outbox.cost_micros),
            fmt_dollars(plan.bank_spent_micros),
            fmt_dollars(plan.bank_limit_micros)
        );
        c.draw_text(18, 96, &l1, 1, [248, 250, 252, 255]);
        c.draw_text(18, 118, &l2, 1, [147, 197, 253, 255]);
        c.draw_text(18, 140, &l3, 1, [253, 224, 71, 255]);
        c.draw_text(18, 162, &l4, 1, [167, 243, 208, 255]);
        c.draw_text(18, 184, &l5, 1, [248, 250, 252, 255]);

        // Approve / Deny action buttons
        let by = c.height.saturating_sub(62);
        c.fill_rect(18, by, 240, 40, [22, 163, 74, 255]);
        c.draw_text(38, by + 16, "[ENTER] APPROVE HASH", 1, [255, 255, 255, 255]);

        c.fill_rect(276, by, 240, 40, [185, 28, 28, 255]);
        c.draw_text(308, by + 16, "[ESC] DENY EFFECT", 1, [255, 255, 255, 255]);
    } else {
        c.draw_text(18, 102, "UNTRUSTED APPS CANNOT RECEIVE INPUT OR READ PIXELS.", 1, [248, 250, 252, 255]);
        let l2 = format!(
            "APPROVED: {}   DENIED: {}   POWERBOX: {}",
            plan.approved, plan.denied, plan.powerbox_transfers
        );
        let l3 = format!(
            "CARETAKER AGENT CONNECTION: {}",
            if plan.agent_conn { "ACTIVE (F7 TO REVOKE)" } else { "REVOKED (F7 TO RESTORE)" }
        );
        c.draw_text(18, 134, &l2, 1, [167, 243, 208, 255]);
        c.draw_text(18, 162, &l3, 1, [253, 224, 71, 255]);

        let by = c.height.saturating_sub(62);
        c.fill_rect(18, by, 280, 40, [37, 99, 235, 255]);
        c.draw_text(38, by + 16, "[F6 / ESC] EXIT SAS LOCK", 1, [255, 255, 255, 255]);
    }
}
