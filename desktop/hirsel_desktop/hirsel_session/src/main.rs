// hirsel_shell: Thin Rust Flatland Presenter + Native Chromium 160 Launcher +
// Compositor-Owned Trusted Chrome Painter + Local egress-gw Control Plane.
//
// Policy & layout geometry live strictly inside the zero-ambient pure-Hirsel
// child process `/pkg/bin/hirsel-wm` (wm_client.rs).

mod font8x16;
mod painter;
mod web_content;
mod wm_client;

use anyhow::{Context, Result};
use fidl::endpoints::{Proxy, create_proxy};
use fidl_fuchsia_input as finput;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_session_scene as scene;
use fidl_fuchsia_ui_composition as fland;
use fidl_fuchsia_ui_input3 as finput3;
use fidl_fuchsia_ui_views as fviews;
use fidl_fuchsia_web as fweb;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use log::{error, info, warn};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use wm_client::{RectJson, ScenePlan, WmClient};

struct IdGen(u64);
impl IdGen {
    fn new() -> Self { Self(1) }
    fn next_transform_id(&mut self) -> fland::TransformId {
        let id = self.0;
        self.0 += 1;
        fland::TransformId { value: id }
    }
    fn next_content_id(&mut self) -> fland::ContentId {
        let id = self.0;
        self.0 += 1;
        fland::ContentId { value: id }
    }
}

fn new_view_creation_token_pair() -> (fviews::ViewCreationToken, fviews::ViewportCreationToken) {
    let (v, vp) = zx::Channel::create();
    (fviews::ViewCreationToken { value: v }, fviews::ViewportCreationToken { value: vp })
}

fn new_view_identity() -> Result<(fviews::ViewIdentityOnCreation, fviews::ViewRef, fviews::ViewRef)> {
    let (control_ep, view_ep) = zx::EventPair::create();
    let ctrl_rights = (zx::Rights::BASIC | zx::Rights::SIGNAL | zx::Rights::SIGNAL_PEER) - zx::Rights::DUPLICATE;
    let control_ep = control_ep.replace_handle(ctrl_rights)?;
    let view_ep = view_ep.replace_handle(zx::Rights::BASIC)?;
    let vr1 = fviews::ViewRef { reference: view_ep.duplicate_handle(zx::Rights::SAME_RIGHTS)? };
    let vr2 = fviews::ViewRef { reference: view_ep.duplicate_handle(zx::Rights::SAME_RIGHTS)? };
    let ident = fviews::ViewIdentityOnCreation {
        view_ref: fviews::ViewRef { reference: view_ep },
        view_ref_control: fviews::ViewRefControl { reference: control_ep },
    };
    Ok((ident, vr1, vr2))
}

const TOP_BAR_H: u32 = 36;
const BORDER_PX: i32 = 3;

enum ShellMsg {
    WmCommand {
        cmd: String,
        reply: Option<oneshot::Sender<Result<ScenePlan>>>,
    },
    GetState {
        reply: oneshot::Sender<ScenePlan>,
    },
    DevLog(String),
    StageDemo {
        reply: Option<oneshot::Sender<Result<ScenePlan>>>,
    },
    ApprovePending {
        reply: Option<oneshot::Sender<Result<ScenePlan>>>,
    },
    ChildViewRef {
        domain: &'static str,
        view_ref: fviews::ViewRef,
    },
    Relayout {
        width: u32,
        height: u32,
    },
}

struct WebViewport {
    _context: fweb::ContextProxy,
    _frame: fweb::FrameProxy,
    _nav: fweb::NavigationControllerProxy,
    viewport_transform: fland::TransformId,
    viewport_content: fland::ContentId,
    border_transform: fland::TransformId,
    border_content: fland::ContentId,
    view_ref: Option<fviews::ViewRef>,
    attached: bool,
}

struct HirselShell {
    flatland: fland::FlatlandProxy,
    id_gen: IdGen,
    view_focuser: fviews::FocuserProxy,
    root_view_ref: fviews::ViewRef,
    root_transform: fland::TransformId,

    // Background solid fill
    _bg_transform: fland::TransformId,
    bg_content: fland::ContentId,

    // Compositor-owned Top Bar (36px)
    top_bar_transform: fland::TransformId,
    top_bar_surface: painter::CpuSurface,
    top_bar_attached: bool,

    // Split vm-dev surface + border
    dev_border_transform: fland::TransformId,
    dev_border_content: fland::ContentId,
    dev_transform: fland::TransformId,
    dev_surface: Option<painter::CpuSurface>,
    dev_attached: bool,
    dev_logs: Vec<String>,

    // Topmost Protected Ceremony / SAS dim backdrop + card
    dim_transform: fland::TransformId,
    dim_content: fland::ContentId,
    ceremony_transform: fland::TransformId,
    ceremony_surface: Option<painter::CpuSurface>,
    ceremony_attached: bool,

    // Native Chromium 160 WebEngine contexts
    web_provider: Option<fweb::ContextProviderProxy>,
    figments_web: Option<WebViewport>,
    untrusted_web: Option<WebViewport>,

    // Pure-Hirsel WM policy brain
    wm: WmClient,
    next_effect_id: u64,
    tx: UnboundedSender<ShellMsg>,
}

fn rgba_f(r: f32, g: f32, b: f32, a: f32) -> fland::ColorRgba {
    fland::ColorRgba { red: r, green: g, blue: b, alpha: a }
}

impl HirselShell {
    async fn new(tx: UnboundedSender<ShellMsg>) -> Result<Self> {
        // 1. Spawn pure-Hirsel hirsel-wm first so it owns all state & geometry from frame 0.
        let mut wm = WmClient::spawn().await.context("spawn pure-Hirsel hirsel-wm")?;

        // 2. Connect to Scene Manager & Flatland root view.
        let scene_manager = connect_to_protocol::<scene::ManagerMarker>()
            .context("connect fuchsia.session.scene.Manager")?;
        let (view_creation_token, viewport_creation_token) = new_view_creation_token_pair();
        let present_fut = scene_manager.present_root_view(viewport_creation_token);

        let flatland = connect_to_protocol::<fland::FlatlandMarker>()
            .context("connect fuchsia.ui.composition.Flatland")?;
        flatland.set_debug_name("HirselShell")?;
        let mut flatland_events = flatland.take_event_stream();
        fasync::Task::local(async move {
            while let Some(ev) = flatland_events.next().await {
                match ev {
                    Ok(fland::FlatlandEvent::OnError { error }) => {
                        error!("FLATLAND PROTOCOL ERROR: {error:?}");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!("FLATLAND EVENT STREAM CLOSED: {e:?}");
                        break;
                    }
                }
            }
        })
        .detach();

        let mut id_gen = IdGen::new();
        let root_transform = id_gen.next_transform_id();
        flatland.create_transform(&root_transform)?;
        flatland.set_root_transform(&root_transform)?;

        let (parent_watcher, parent_watcher_req) =
            create_proxy::<fland::ParentViewportWatcherMarker>();
        let (view_focuser, view_focuser_req) = create_proxy::<fviews::FocuserMarker>();
        let (view_identity, root_view_ref_for_kb, root_view_ref) = new_view_identity()?;

        flatland.create_view2(
            view_creation_token,
            view_identity,
            fland::ViewBoundProtocols {
                view_focuser: Some(view_focuser_req),
                ..Default::default()
            },
            parent_watcher_req,
        )?;

        flatland.present(fland::PresentArgs {
            requested_presentation_time: Some(0),
            ..Default::default()
        })?;

        let layout_info = parent_watcher.get_layout().await.context("get initial Flatland layout")?;
        let _ = present_fut.await.context("present_root_view")?;

        let init_size = layout_info.logical_size.unwrap_or(fmath::SizeU { width: 1280, height: 800 });
        let width = init_size.width.max(640);
        let height = init_size.height.max(480);
        info!("Flatland root view ready: {width}x{height}");

        // Watch subsequent display resizes
        let resize_tx = tx.clone();
        fasync::Task::local(async move {
            while let Ok(info) = parent_watcher.get_layout().await {
                if let Some(sz) = info.logical_size {
                    let _ = resize_tx.unbounded_send(ShellMsg::Relayout {
                        width: sz.width.max(640),
                        height: sz.height.max(480),
                    });
                }
            }
        })
        .detach();

        // Register global keyboard listener on root ViewRef for OS shortcuts + ceremony keys
        spawn_keyboard_listener(root_view_ref_for_kb, tx.clone());

        // Sync initial display dimensions into pure-Hirsel hirsel-wm
        let _ = wm.send(&format!("RESIZE {width} {height}")).await?;

        // Background solid rect
        let bg_transform = id_gen.next_transform_id();
        let bg_content = id_gen.next_content_id();
        flatland.create_transform(&bg_transform)?;
        flatland.create_filled_rect(&bg_content)?;
        flatland.set_content(&bg_transform, &bg_content)?;
        flatland.add_child(&root_transform, &bg_transform)?;

        // Top 36px compositor-owned trust bar
        let top_bar_transform = id_gen.next_transform_id();
        let top_bar_content = id_gen.next_content_id();
        flatland.create_transform(&top_bar_transform)?;
        let top_bar_surface = painter::CpuSurface::new(
            "hirsel-top-bar",
            width,
            TOP_BAR_H,
            &flatland,
            top_bar_content,
            fland::BlendMode::Src,
        )
        .await
        .context("allocate top bar surface")?;
        flatland.set_content(&top_bar_transform, &top_bar_surface.content_id)?;
        flatland.set_translation(&top_bar_transform, &fmath::Vec_ { x: 0, y: 0 })?;

        // Dev pane transforms (attached lazily when show_dev=true)
        let dev_border_transform = id_gen.next_transform_id();
        let dev_border_content = id_gen.next_content_id();
        flatland.create_transform(&dev_border_transform)?;
        flatland.create_filled_rect(&dev_border_content)?;
        flatland.set_content(&dev_border_transform, &dev_border_content)?;

        let dev_transform = id_gen.next_transform_id();
        flatland.create_transform(&dev_transform)?;

        // Protected Ceremony / SAS dim backdrop + card transforms (always topmost!)
        let dim_transform = id_gen.next_transform_id();
        let dim_content = id_gen.next_content_id();
        flatland.create_transform(&dim_transform)?;
        flatland.create_filled_rect(&dim_content)?;
        flatland.set_content(&dim_transform, &dim_content)?;

        let ceremony_transform = id_gen.next_transform_id();
        flatland.create_transform(&ceremony_transform)?;

        // Connect to Native Chromium 160 WebEngine ContextProvider
        let web_provider = match connect_to_protocol::<fweb::ContextProviderMarker>() {
            Ok(p) => {
                info!("Connected to fuchsia.web.ContextProvider (Native Chromium 160)");
                Some(p)
            }
            Err(e) => {
                warn!("WebEngine ContextProvider unavailable: {e:?}");
                None
            }
        };

        let mut shell = Self {
            flatland,
            id_gen,
            view_focuser,
            root_view_ref,
            root_transform,
            _bg_transform: bg_transform,
            bg_content,
            top_bar_transform,
            top_bar_surface,
            top_bar_attached: false,
            dev_border_transform,
            dev_border_content,
            dev_transform,
            dev_surface: None,
            dev_attached: false,
            dev_logs: vec![
                "hirsel-os$ uname -a".to_string(),
                "Zircon x86_64 (AMD-SVM + NPT + Native Flatland)".to_string(),
                "hirsel-os$ ps | grep hirsel".to_string(),
                "  hirsel-wm    [Tier-1 Pure Hirsel | 0 ambient handles]".to_string(),
                "  hirsel-shell [Flatland Presenter + Trusted Chrome]".to_string(),
                "  web_engine   [Native Chromium 160.0.8155.0]".to_string(),
                "hirsel-os$ # Ready for Powerbox paste or /_hirsel/dev-exec".to_string(),
            ],
            dim_transform,
            dim_content,
            ceremony_transform,
            ceremony_surface: None,
            ceremony_attached: false,
            web_provider,
            figments_web: None,
            untrusted_web: None,
            wm,
            next_effect_id: 1,
            tx,
        };

        // Launch Native Chromium 160 Context #1 for Figments Shell
        if let Err(e) = shell.launch_web_context("figments", "http://127.0.0.1:7711/").await {
            error!("Failed to launch Figments Chromium context: {e:?}");
        }

        shell.apply_scene().await?;
        Ok(shell)
    }

    async fn launch_web_context(&mut self, domain: &'static str, url: &str) -> Result<()> {
        let provider = match &self.web_provider {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        let svc_dir = fuchsia_fs::directory::open_in_namespace("/svc", fio::PERM_READABLE)
            .context("open /svc for WebEngine Context")?;
        let svc_client = svc_dir
            .into_client_end()
            .map_err(|_| anyhow::anyhow!("into_client_end(/svc) failed"))?;

        let (context, context_server) = create_proxy::<fweb::ContextMarker>();
        provider.create(
            fweb::CreateContextParams {
                service_directory: Some(svc_client),
                features: Some(
                    fweb::ContextFeatureFlags::NETWORK | fweb::ContextFeatureFlags::KEYBOARD,
                ),
                user_agent_product: Some(format!("HirselOS-{domain}")),
                user_agent_version: Some("0.1".to_string()),
                ..Default::default()
            },
            context_server,
        )?;

        let (frame, frame_server) = create_proxy::<fweb::FrameMarker>();
        context.create_frame_with_params(
            fweb::CreateFrameParams {
                debug_name: Some(format!("hirsel-{domain}")),
                ..Default::default()
            },
            frame_server,
        )?;

        let (view_creation_token, viewport_creation_token) = new_view_creation_token_pair();
        frame.create_view2(fweb::CreateView2Args {
            view_creation_token: Some(view_creation_token),
            ..Default::default()
        })?;

        let (nav, nav_server) = create_proxy::<fweb::NavigationControllerMarker>();
        frame.get_navigation_controller(nav_server)?;
        let nav_clone = nav.clone();
        let target_url = url.to_string();
        fasync::Task::local(async move {
            match nav_clone.load_url(&target_url, fweb::LoadUrlParams::default()).await {
                Ok(Ok(())) => info!("Chromium ({domain}) loaded {target_url}"),
                Ok(Err(e)) => warn!("Chromium ({domain}) load_url err: {e:?}"),
                Err(e) => warn!("Chromium ({domain}) load_url fidl err: {e:?}"),
            }
        })
        .detach();

        let (child_watcher, child_watcher_req) = create_proxy::<fland::ChildViewWatcherMarker>();
        let viewport_content = self.id_gen.next_content_id();
        let viewport_transform = self.id_gen.next_transform_id();
        let border_content = self.id_gen.next_content_id();
        let border_transform = self.id_gen.next_transform_id();

        let init_rect = &self.wm.latest.layout.figments;
        self.flatland.create_viewport(
            &viewport_content,
            viewport_creation_token,
            &fland::ViewportProperties {
                logical_size: Some(fmath::SizeU {
                    width: init_rect.w.max(64) as u32,
                    height: init_rect.h.max(64) as u32,
                }),
                ..Default::default()
            },
            child_watcher_req,
        )?;
        self.flatland.create_transform(&viewport_transform)?;
        self.flatland.set_content(&viewport_transform, &viewport_content)?;

        self.flatland.create_transform(&border_transform)?;
        self.flatland.create_filled_rect(&border_content)?;
        self.flatland.set_content(&border_transform, &border_content)?;

        let tx_ref = self.tx.clone();
        fasync::Task::local(async move {
            if let Ok(view_ref) = child_watcher.get_view_ref().await {
                let _ = tx_ref.unbounded_send(ShellMsg::ChildViewRef { domain, view_ref });
            }
        })
        .detach();

        let vp = WebViewport {
            _context: context,
            _frame: frame,
            _nav: nav,
            viewport_transform,
            viewport_content,
            border_transform,
            border_content,
            view_ref: None,
            attached: false,
        };
        if domain == "figments" {
            self.figments_web = Some(vp);
        } else {
            self.untrusted_web = Some(vp);
        }
        info!("Launched Native Chromium 160 context `{domain}` -> {url}");
        Ok(())
    }

    fn place_border_and_inner(
        flatland: &fland::FlatlandProxy,
        border_transform: &fland::TransformId,
        border_content: &fland::ContentId,
        inner_transform: &fland::TransformId,
        rect: &RectJson,
        color: fland::ColorRgba,
    ) -> Result<()> {
        let bx = (rect.x as i32 - BORDER_PX).max(0);
        let by = (rect.y as i32 - BORDER_PX).max(TOP_BAR_H as i32);
        let bw = rect.w as u32 + (BORDER_PX as u32) * 2;
        let bh = rect.h as u32 + (BORDER_PX as u32) * 2;
        flatland.set_solid_fill(border_content, &color, &fmath::SizeU { width: bw, height: bh })?;
        flatland.set_translation(border_transform, &fmath::Vec_ { x: bx, y: by })?;
        flatland.set_translation(
            inner_transform,
            &fmath::Vec_ { x: rect.x as i32, y: rect.y as i32 },
        )?;
        Ok(())
    }

    async fn apply_scene(&mut self) -> Result<()> {
        let plan = self.wm.latest.clone();
        let sw = plan.w as u32;
        let sh = plan.h as u32;

        // Lazily spawn isolated Chromium Context #2 when web pane is toggled ON
        if plan.layout.web.is_some() && self.untrusted_web.is_none() {
            if let Err(e) =
                self.launch_web_context("web_untrusted", "http://127.0.0.1:7711/untrusted").await
            {
                error!("Failed to launch untrusted Chromium context: {e:?}");
            }
        }

        // Background color by room
        let bg_col = if plan.room == "daylight" {
            rgba_f(0.92, 0.94, 0.97, 1.0)
        } else {
            rgba_f(0.04, 0.06, 0.10, 1.0)
        };
        self.flatland.set_solid_fill(&self.bg_content, &bg_col, &fmath::SizeU { width: sw, height: sh })?;

        // Re-create top bar surface if screen width changed
        if self.top_bar_surface.width != sw {
            let cid = self.id_gen.next_content_id();
            self.top_bar_surface = painter::CpuSurface::new(
                "hirsel-top-bar",
                sw,
                TOP_BAR_H,
                &self.flatland,
                cid,
                fland::BlendMode::Src,
            )
            .await?;
            self.flatland.set_content(&self.top_bar_transform, &self.top_bar_surface.content_id)?;
        }
        self.top_bar_surface.paint_with(|c| painter::paint_top_bar(c, &plan));

        // Detach dynamic layers cleanly before re-attaching in exact security Z-order
        if self.top_bar_attached {
            let _ = self.flatland.remove_child(&self.root_transform, &self.top_bar_transform);
            self.top_bar_attached = false;
        }
        if let Some(ref mut fig) = self.figments_web {
            if fig.attached {
                let _ = self.flatland.remove_child(&self.root_transform, &fig.border_transform);
                let _ = self.flatland.remove_child(&self.root_transform, &fig.viewport_transform);
                fig.attached = false;
            }
        }
        if self.dev_attached {
            let _ = self.flatland.remove_child(&self.root_transform, &self.dev_border_transform);
            let _ = self.flatland.remove_child(&self.root_transform, &self.dev_transform);
            self.dev_attached = false;
        }
        if let Some(ref mut web) = self.untrusted_web {
            if web.attached {
                let _ = self.flatland.remove_child(&self.root_transform, &web.border_transform);
                let _ = self.flatland.remove_child(&self.root_transform, &web.viewport_transform);
                web.attached = false;
            }
        }
        if self.ceremony_attached {
            let _ = self.flatland.remove_child(&self.root_transform, &self.dim_transform);
            let _ = self.flatland.remove_child(&self.root_transform, &self.ceremony_transform);
            self.ceremony_attached = false;
        }

        // Layer 1: Figments Chromium Viewport #1 (Cobalt trust border)
        if let Some(ref mut fig) = self.figments_web {
            let r = &plan.layout.figments;
            let col = if plan.focused == "figments" {
                rgba_f(0.23, 0.51, 0.96, 1.0)
            } else {
                rgba_f(0.14, 0.25, 0.45, 1.0)
            };
            self.flatland.set_viewport_properties(
                &fig.viewport_content,
                &fland::ViewportProperties {
                    logical_size: Some(fmath::SizeU { width: r.w as u32, height: r.h as u32 }),
                    ..Default::default()
                },
            )?;
            Self::place_border_and_inner(
                &self.flatland,
                &fig.border_transform,
                &fig.border_content,
                &fig.viewport_transform,
                r,
                col,
            )?;
            self.flatland.add_child(&self.root_transform, &fig.border_transform)?;
            self.flatland.add_child(&self.root_transform, &fig.viewport_transform)?;
            fig.attached = true;
        }

        // Layer 2: vm-dev split console (Emerald trust border)
        if let Some(ref r) = plan.layout.vm_dev {
            let rw = r.w.max(64) as u32;
            let rh = r.h.max(64) as u32;
            let need_alloc = self
                .dev_surface
                .as_ref()
                .map(|s| s.width != rw || s.height != rh)
                .unwrap_or(true);
            if need_alloc {
                let cid = self.id_gen.next_content_id();
                let surf = painter::CpuSurface::new(
                    "hirsel-vm-dev",
                    rw,
                    rh,
                    &self.flatland,
                    cid,
                    fland::BlendMode::Src,
                )
                .await?;
                self.flatland.set_content(&self.dev_transform, &surf.content_id)?;
                self.dev_surface = Some(surf);
            }
            if let Some(ref surf) = self.dev_surface {
                surf.paint_with(|c| painter::paint_dev_pane(c, &plan, &self.dev_logs));
            }
            let col = if plan.focused == "vm_dev" {
                rgba_f(0.06, 0.72, 0.51, 1.0)
            } else {
                rgba_f(0.06, 0.36, 0.27, 1.0)
            };
            Self::place_border_and_inner(
                &self.flatland,
                &self.dev_border_transform,
                &self.dev_border_content,
                &self.dev_transform,
                r,
                col,
            )?;
            self.flatland.add_child(&self.root_transform, &self.dev_border_transform)?;
            self.flatland.add_child(&self.root_transform, &self.dev_transform)?;
            self.dev_attached = true;
        }

        // Layer 3: Isolated Untrusted Chromium Viewport #2 (Amber trust border)
        if let (Some(r), Some(web)) = (&plan.layout.web, &mut self.untrusted_web) {
            let col = if plan.focused == "web_untrusted" {
                rgba_f(0.96, 0.62, 0.04, 1.0)
            } else {
                rgba_f(0.55, 0.34, 0.05, 1.0)
            };
            self.flatland.set_viewport_properties(
                &web.viewport_content,
                &fland::ViewportProperties {
                    logical_size: Some(fmath::SizeU { width: r.w as u32, height: r.h as u32 }),
                    ..Default::default()
                },
            )?;
            Self::place_border_and_inner(
                &self.flatland,
                &web.border_transform,
                &web.border_content,
                &web.viewport_transform,
                r,
                col,
            )?;
            self.flatland.add_child(&self.root_transform, &web.border_transform)?;
            self.flatland.add_child(&self.root_transform, &web.viewport_transform)?;
            web.attached = true;
        }

        // Layer 4: Compositor-owned Top Trust Bar (above all viewports)
        self.flatland.add_child(&self.root_transform, &self.top_bar_transform)?;
        self.top_bar_attached = true;

        // Layer 5 (TOPMOST): Protected Approval Ceremony / SAS Lockdown overlay
        if let Some(ref cr) = plan.layout.ceremony {
            let cw = cr.w.max(240) as u32;
            let ch = cr.h.max(160) as u32;
            let need_alloc = self
                .ceremony_surface
                .as_ref()
                .map(|s| s.width != cw || s.height != ch)
                .unwrap_or(true);
            if need_alloc {
                let cid = self.id_gen.next_content_id();
                let surf = painter::CpuSurface::new(
                    "hirsel-ceremony",
                    cw,
                    ch,
                    &self.flatland,
                    cid,
                    fland::BlendMode::Src,
                )
                .await?;
                self.flatland.set_content(&self.ceremony_transform, &surf.content_id)?;
                self.ceremony_surface = Some(surf);
            }
            if let Some(ref surf) = self.ceremony_surface {
                surf.paint_with(|c| painter::paint_ceremony_card(c, &plan));
            }
            self.flatland.set_solid_fill(
                &self.dim_content,
                &rgba_f(0.02, 0.02, 0.05, 0.65),
                &fmath::SizeU { width: sw, height: sh.saturating_sub(TOP_BAR_H) },
            )?;
            self.flatland
                .set_translation(&self.dim_transform, &fmath::Vec_ { x: 0, y: TOP_BAR_H as i32 })?;
            self.flatland.set_translation(
                &self.ceremony_transform,
                &fmath::Vec_ { x: cr.x as i32, y: cr.y as i32 },
            )?;
            self.flatland.add_child(&self.root_transform, &self.dim_transform)?;
            self.flatland.add_child(&self.root_transform, &self.ceremony_transform)?;
            self.ceremony_attached = true;
        }

        // Enforce focus strictly according to pure-Hirsel hirsel-wm!
        self.apply_focus(&plan);

        // Single atomic Flatland commit
        self.flatland.present(fland::PresentArgs {
            requested_presentation_time: Some(0),
            ..Default::default()
        })?;
        Ok(())
    }

    fn apply_focus(&self, plan: &ScenePlan) {
        let target_ref: Option<&fviews::ViewRef> = match plan.focused.as_str() {
            "trusted_chrome" | "vm_dev" => Some(&self.root_view_ref),
            "figments" => self
                .figments_web
                .as_ref()
                .and_then(|w| w.view_ref.as_ref())
                .or(Some(&self.root_view_ref)),
            "web_untrusted" => self
                .untrusted_web
                .as_ref()
                .and_then(|w| w.view_ref.as_ref())
                .or(Some(&self.root_view_ref)),
            _ => Some(&self.root_view_ref),
        };
        if let Some(vr) = target_ref {
            if let Ok(dup) = vr.reference.duplicate_handle(zx::Rights::SAME_RIGHTS) {
                let fut = self.view_focuser.request_focus(fviews::ViewRef { reference: dup });
                fasync::Task::local(async move {
                    let _ = fut.await;
                })
                .detach();
            }
        }
    }

    async fn handle(&mut self, msg: ShellMsg) {
        match msg {
            ShellMsg::WmCommand { cmd, reply } => {
                let res = self.wm.send(&cmd).await;
                if res.is_ok() {
                    if let Err(e) = self.apply_scene().await {
                        error!("apply_scene error: {e:?}");
                    }
                }
                if let Some(tx) = reply {
                    let _ = tx.send(res);
                }
            }
            ShellMsg::GetState { reply } => {
                let _ = reply.send(self.wm.latest.clone());
            }
            ShellMsg::DevLog(line) => {
                self.dev_logs.push(line);
                if self.dev_logs.len() > 64 {
                    self.dev_logs.remove(0);
                }
                let _ = self.apply_scene().await;
            }
            ShellMsg::StageDemo { reply } => {
                let id = self.next_effect_id;
                self.next_effect_id += 1;
                let digest = 0x5A17_0000u64 ^ (id * 0x9E37_79B9);
                let cmd = format!(
                    "STAGE {id} {digest} 250000 api.github.com POST_/repos/Ascending-AI/figments/pulls"
                );
                let res = self.wm.send(&cmd).await;
                if res.is_ok() {
                    let _ = self.apply_scene().await;
                }
                if let Some(tx) = reply {
                    let _ = tx.send(res);
                }
            }
            ShellMsg::ApprovePending { reply } => {
                let outbox = &self.wm.latest.outbox;
                let cmd = format!("APPROVE {} {}", outbox.id, outbox.digest);
                let res = self.wm.send(&cmd).await;
                if res.is_ok() {
                    let _ = self.apply_scene().await;
                }
                if let Some(tx) = reply {
                    let _ = tx.send(res);
                }
            }
            ShellMsg::ChildViewRef { domain, view_ref } => {
                info!("Received Flatland ChildViewRef for `{domain}`");
                if domain == "figments" {
                    if let Some(ref mut fig) = self.figments_web {
                        fig.view_ref = Some(view_ref);
                    }
                } else if let Some(ref mut web) = self.untrusted_web {
                    web.view_ref = Some(view_ref);
                }
                self.apply_focus(&self.wm.latest);
            }
            ShellMsg::Relayout { width, height } => {
                if self.wm.send(&format!("RESIZE {width} {height}")).await.is_ok() {
                    let _ = self.apply_scene().await;
                }
            }
        }
    }
}

fn spawn_keyboard_listener(view_ref: fviews::ViewRef, tx: UnboundedSender<ShellMsg>) {
    fasync::Task::local(async move {
        let keyboard = match connect_to_protocol::<finput3::KeyboardMarker>() {
            Ok(k) => k,
            Err(e) => {
                warn!("Keyboard protocol unavailable: {e:?}");
                return;
            }
        };
        let (client_end, mut stream) =
            fidl::endpoints::create_request_stream::<finput3::KeyboardListenerMarker>();
        if let Err(e) = keyboard.add_listener(view_ref, client_end).await {
            warn!("Keyboard.add_listener failed: {e:?}");
            return;
        }
        info!("Registered root KeyboardListener for Hirsel-OS global hotkeys & SAS");
        while let Some(Ok(finput3::KeyboardListenerRequest::OnKeyEvent { event, responder })) =
            stream.next().await
        {
            let mut handled = finput3::KeyEventStatus::NotHandled;
            if event.type_ == Some(finput3::KeyEventType::Pressed) {
                match event.key {
                    Some(finput::Key::F1) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "FOCUS figments".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::F2) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "TOGGLE_DEV".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::F3) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "TOGGLE_WEB".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::F4) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "TOGGLE_ROOM".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::F5) => {
                        let _ = tx.unbounded_send(ShellMsg::StageDemo { reply: None });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::F6) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "TOGGLE_SAS".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::F7) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "REVOKE".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::Enter) | Some(finput::Key::KeypadEnter) => {
                        let _ = tx.unbounded_send(ShellMsg::ApprovePending { reply: None });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::Escape) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "DENY".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    _ => {}
                }
            }
            let _ = responder.send(handled);
        }
    })
    .detach();
}

fn spawn_http_server(port: u16, tx: UnboundedSender<ShellMsg>, diag: Arc<Mutex<Vec<String>>>) {
    fasync::Task::local(async move {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
        let listener = match fasync::net::TcpListener::bind(&addr) {
            Ok(l) => {
                info!("Hirsel-OS HTTP / egress-gw listening on 0.0.0.0:{port}");
                l
            }
            Err(e) => {
                error!("Failed to bind HTTP server on port {port}: {e:?}");
                return;
            }
        };
        let mut incoming = listener.accept_stream();
        while let Some(conn) = incoming.next().await {
            if let Ok((stream, _peer)) = conn {
                let tx_conn = tx.clone();
                let diag_conn = diag.clone();
                fasync::Task::local(async move {
                    let _ = handle_http_conn(stream, tx_conn, diag_conn).await;
                })
                .detach();
            }
        }
    })
    .detach();
}

async fn handle_http_conn(
    mut stream: fasync::net::TcpStream,
    tx: UnboundedSender<ShellMsg>,
    diag: Arc<Mutex<Vec<String>>>,
) -> Result<()> {
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    let body = req.split("\r\n\r\n").nth(1).unwrap_or("").trim();

    let (status, content_type, payload): (&str, &str, String) = match (method, path) {
        ("GET", "/_hirsel/diag") => {
            let log_lines = diag.lock().map(|g| g.clone()).unwrap_or_default();
            ("200 OK", "application/json", serde_json::json!({ "diag": log_lines }).to_string())
        }
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", web_content::FIGMENTS_INDEX_HTML.to_string())
        }
        ("GET", "/untrusted") => {
            ("200 OK", "text/html; charset=utf-8", web_content::UNTRUSTED_WEB_HTML.to_string())
        }
        ("GET", "/_hirsel/state") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetState { reply: otx });
            match orx.await {
                Ok(st) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"wm"}"#.into()),
            }
        }
        ("POST", "/_hirsel/stage") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::StageDemo { reply: Some(otx) });
            match orx.await {
                Ok(Ok(st)) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"stage"}"#.into()),
            }
        }
        ("POST", "/_hirsel/approve") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::ApprovePending { reply: Some(otx) });
            match orx.await {
                Ok(Ok(st)) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"approve"}"#.into()),
            }
        }
        ("POST", "/_hirsel/spoof-focus") => {
            let target = if body.is_empty() { "web_untrusted" } else { body };
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::WmCommand {
                cmd: format!("CLIENT_FOCUS {target}"),
                reply: Some(otx),
            });
            match orx.await {
                Ok(Ok(st)) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"spoof"}"#.into()),
            }
        }
        ("POST", "/_hirsel/cmd") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::WmCommand {
                cmd: body.to_string(),
                reply: Some(otx),
            });
            match orx.await {
                Ok(Ok(st)) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"cmd"}"#.into()),
            }
        }
        ("POST", "/_hirsel/dev-exec") => {
            let line = format!("vm-dev$ {}", if body.is_empty() { "echo ok" } else { body });
            let _ = tx.unbounded_send(ShellMsg::DevLog(line.clone()));
            ("200 OK", "application/json", serde_json::json!({"ok": true, "logged": line}).to_string())
        }
        ("POST", "/_hirsel/proxy") => {
            // Host-side capability-gated egress-gw stub: checks Caretaker connection & allowlist.
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetState { reply: otx });
            let allowed_hosts = ["api.github.com", "api.anthropic.com", "api.openai.com"];
            let host = if body.is_empty() { "api.github.com" } else { body };
            match orx.await {
                Ok(st) if !st.agent_conn => (
                    "403 Forbidden",
                    "application/json",
                    serde_json::json!({
                        "allowed": false,
                        "reason": "caretaker_connection_revoked",
                        "host": host
                    })
                    .to_string(),
                ),
                Ok(_) if allowed_hosts.contains(&host) => (
                    "200 OK",
                    "application/json",
                    serde_json::json!({
                        "allowed": true,
                        "gateway": "hirsel-egress-gw",
                        "host": host,
                        "credential_injection": "host-only (client holds zero raw API keys)"
                    })
                    .to_string(),
                ),
                _ => (
                    "403 Forbidden",
                    "application/json",
                    serde_json::json!({
                        "allowed": false,
                        "reason": "host_not_in_capability_allowlist",
                        "host": host
                    })
                    .to_string(),
                ),
            }
        }
        _ => ("404 Not Found", "text/plain", "Not Found".to_string()),
    };

    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<()> {
    info!("=== Hirsel-OS Desktop Shell starting (Native Flatland + Chromium 160 + Pure-Hirsel WM) ===");
    let (tx, mut rx) = unbounded::<ShellMsg>();
    let diag = Arc::new(Mutex::new(vec!["starting hirsel_shell".to_string()]));

    // Start local Figments UI + egress-gw control servers BEFORE Chromium navigates to 127.0.0.1:7711
    spawn_http_server(7711, tx.clone(), diag.clone());
    spawn_http_server(7780, tx.clone(), diag.clone());

    let mut shell = match HirselShell::new(tx).await {
        Ok(s) => {
            if let Ok(mut g) = diag.lock() {
                g.push("HirselShell initialized OK".to_string());
            }
            s
        }
        Err(e) => {
            let msg = format!("HirselShell::new failed: {e:#}");
            error!("{msg}");
            if let Ok(mut g) = diag.lock() {
                g.push(msg);
            }
            // Keep diagnostic HTTP server alive on :7780 so host can read /_hirsel/diag!
            futures::future::pending::<()>().await;
            unreachable!();
        }
    };
    info!("Hirsel-OS Desktop Shell active! Control plane on :7711 and :7780.");

    while let Some(msg) = rx.next().await {
        shell.handle(msg).await;
    }
    Ok(())
}
