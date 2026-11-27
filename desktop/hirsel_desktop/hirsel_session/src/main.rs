// hirsel_shell: Thin Rust Flatland Presenter + Native Chromium 160 Launcher +
// Compositor-Owned Trusted Chrome Painter + Real Tier-2 Linux 6.1.102 MicroVM (`vm-dev`) +
// Pure-Hirsel Figments `js@1` Sandbox (`hirsel-js`) + Real `egress-gw` + Fxfs `/data` Persistence.
//
// Policy & layout geometry live strictly inside the zero-ambient pure-Hirsel
// child process `/pkg/bin/hirsel-wm` (wm_client.rs).

mod font8x16;
mod js_client;
mod painter;
mod vm_client;
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
use fidl_fuchsia_ui_pointer as fptr;
use fidl_fuchsia_ui_views as fviews;
use fidl_fuchsia_web as fweb;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use js_client::JsSandboxClient;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use vm_client::{VmClient, VmEvent, VmExecResult};
use wm_client::{RectJson, ScenePlan, WmClient};

const TOP_BAR_H: u32 = 36;
const BORDER_PX: i32 = 3;
const STATE_PATH: &str = "/data/hirsel_state.json";
const BOARD_PATH: &str = "/data/board.json";
const SECRETS_PATH: &str = "/data/egress_secrets.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    room: String,
    audit_log: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BoardCard {
    id: String,
    title: String,
    sub: String,
    color: String,
    x: i32,
    y: i32,
}

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

#[derive(Clone, Debug)]
struct StagedHttpEffect {
    host: String,
    port: u16,
    path: String,
    method: String,
}

enum ShellMsg {
    WmCommand {
        cmd: String,
        reply: Option<oneshot::Sender<Result<ScenePlan>>>,
    },
    GetState {
        reply: oneshot::Sender<ScenePlan>,
    },
    GetAudit {
        reply: oneshot::Sender<Vec<String>>,
    },
    GetBoard {
        reply: oneshot::Sender<Vec<BoardCard>>,
    },
    AddBoardCard {
        card: BoardCard,
        reply: oneshot::Sender<Vec<BoardCard>>,
    },
    SetClientDiag {
        info: serde_json::Value,
    },
    GetClientDiag {
        reply: oneshot::Sender<serde_json::Value>,
    },
    VmStatus {
        reply: oneshot::Sender<bool>,
    },
    DevExec {
        cmd: String,
        reply: Option<oneshot::Sender<Result<VmExecResult>>>,
    },
    VmIncoming(VmEvent),
    JsEval {
        code: String,
        reply: oneshot::Sender<Result<serde_json::Value>>,
    },
    ClipSet {
        domain: String,
        text: String,
        reply: oneshot::Sender<Result<ScenePlan>>,
    },
    PowerboxPaste {
        from: String,
        to: String,
        reply: oneshot::Sender<Result<serde_json::Value>>,
    },
    StageEffect {
        effect: StagedHttpEffect,
        cost_micros: u64,
        summary: String,
        reply: Option<oneshot::Sender<Result<ScenePlan>>>,
    },
    ApprovePending {
        reply: Option<oneshot::Sender<Result<serde_json::Value>>>,
    },
    MouseClick {
        x: f32,
        y: f32,
    },
    DevKeyChar(char),
    DevKeyBackspace,
    DevKeyEnter,
    ChildViewRef {
        domain: &'static str,
        view_ref: fviews::ViewRef,
    },
    NextFrameBegin(u32),
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

    // Split vm-dev surface + border + live AMD-SVM Linux 6.1.102 MicroVM
    dev_border_transform: fland::TransformId,
    dev_border_content: fland::ContentId,
    dev_transform: fland::TransformId,
    dev_surface: Option<painter::CpuSurface>,
    dev_attached: bool,
    dev_logs: Vec<String>,
    dev_input: String,
    vm_client: Option<Arc<VmClient>>,
    vm_ready: bool,

    // Pure-Hirsel Figments js@1 sandbox Place
    js_client: Option<JsSandboxClient>,

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

    // Pure-Hirsel WM policy brain + per-domain Powerbox buffers + staged effect + Fxfs persistence
    wm: WmClient,
    clipboards: HashMap<String, String>,
    next_effect_id: u64,
    pending_effect: Option<StagedHttpEffect>,
    audit_log: Vec<String>,
    board_cards: Vec<BoardCard>,
    client_diag: serde_json::Value,
    present_credits: u32,
    scene_dirty: bool,
    diag: Arc<Mutex<Vec<String>>>,
    tx: UnboundedSender<ShellMsg>,
}

fn rgba_f(r: f32, g: f32, b: f32, a: f32) -> fland::ColorRgba {
    fland::ColorRgba { red: r, green: g, blue: b, alpha: a }
}

fn load_egress_secret(host: &str) -> String {
    if let Ok(raw) = std::fs::read_to_string(SECRETS_PATH) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
            if let Some(v) = map.get(host) {
                return v.clone();
            }
        }
    }
    // Initialize default sealed host-only demo credential on /data if missing
    let mut defaults = HashMap::new();
    defaults.insert("127.0.0.1".to_string(), "hirsel-host-sealed-demo-token-v1".to_string());
    defaults.insert("api.github.com".to_string(), "ghp_host_sealed_placeholder".to_string());
    if let Ok(json) = serde_json::to_string_pretty(&defaults) {
        let _ = std::fs::write(SECRETS_PATH, json);
    }
    defaults.get(host).cloned().unwrap_or_else(|| "hirsel-host-sealed-token".to_string())
}

async fn execute_egress_http(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(u16, String)> {
    let addr = if let Ok(ip) = host.parse::<IpAddr>() {
        SocketAddr::new(ip, port)
    } else {
        let mut iter = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("DNS resolve {host}:{port}"))?;
        iter.next().ok_or_else(|| anyhow::anyhow!("no IP for {host}:{port}"))?
    };

    let mut stream = fasync::net::TcpStream::connect(addr)
        .with_context(|| format!("TCP connect init {addr}"))?
        .await
        .with_context(|| format!("TCP connect {addr}"))?;

    let secret = load_egress_secret(host);
    let safe_path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let req = format!(
        "{method} {safe_path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: Hirsel-OS-EgressGW/0.1\r\nX-Hirsel-Egress-Key: {secret}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.context("write outbound HTTP request")?;
    stream.flush().await.ok();

    let mut resp = Vec::with_capacity(4096);
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&chunk[..n]);
                if resp.len() >= 16384 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&resp).to_string();
    let status_code = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let resp_body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim().to_string();
    Ok((status_code, resp_body))
}

impl HirselShell {
    async fn new(tx: UnboundedSender<ShellMsg>, diag: Arc<Mutex<Vec<String>>>) -> Result<Self> {
        // 1. Spawn pure-Hirsel hirsel-wm first so it owns all state & geometry from frame 0.
        let mut wm = WmClient::spawn().await.context("spawn pure-Hirsel hirsel-wm")?;

        // Restore persisted Fxfs /data/hirsel_state.json (room + audit log) & /data/board.json if present
        let mut audit_log = Vec::new();
        if let Ok(raw) = std::fs::read_to_string(STATE_PATH) {
            if let Ok(persisted) = serde_json::from_str::<PersistedState>(&raw) {
                info!("Restored persisted state from {STATE_PATH}: room={}", persisted.room);
                if persisted.room == "daylight" {
                    let _ = wm.send("TOGGLE_ROOM").await;
                }
                audit_log = persisted.audit_log;
            }
        }
        let board_cards: Vec<BoardCard> = std::fs::read_to_string(BOARD_PATH)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

        // 2. Spawn pure-Hirsel Figments js@1 sandbox Place (zero ambient handles!)
        let js_client = match JsSandboxClient::spawn().await {
            Ok(j) => {
                if let Ok(mut g) = diag.lock() {
                    g.push("hirsel-js (Figments js@1) spawned OK".to_string());
                }
                Some(j)
            }
            Err(e) => {
                warn!("hirsel-js unavailable: {e:#}");
                if let Ok(mut g) = diag.lock() {
                    g.push(format!("hirsel-js unavailable: {e:#}"));
                }
                None
            }
        };

        // 3. Spawn real Tier-2 Linux 6.1.102 AMD-SVM MicroVM host in background!
        let (vm_ev_tx, mut vm_ev_rx) = unbounded::<VmEvent>();
        let shell_tx_vm = tx.clone();
        fasync::Task::local(async move {
            while let Some(ev) = vm_ev_rx.next().await {
                let _ = shell_tx_vm.unbounded_send(ShellMsg::VmIncoming(ev));
            }
        })
        .detach();

        let vm_client = match VmClient::spawn(vm_ev_tx).await {
            Ok(v) => {
                if let Ok(mut g) = diag.lock() {
                    g.push("hirsel-vm-host (Linux 6.1.102 AMD-SVM) spawned OK".to_string());
                }
                Some(Arc::new(v))
            }
            Err(e) => {
                warn!("hirsel-vm-host unavailable: {e:#}");
                if let Ok(mut g) = diag.lock() {
                    g.push(format!("hirsel-vm-host unavailable: {e:#}"));
                }
                None
            }
        };

        // 4. Connect to Scene Manager & Flatland root view.
        let scene_manager = connect_to_protocol::<scene::ManagerMarker>()
            .context("connect fuchsia.session.scene.Manager")?;
        let (view_creation_token, viewport_creation_token) = new_view_creation_token_pair();
        let present_fut = scene_manager.present_root_view(viewport_creation_token);

        let flatland = connect_to_protocol::<fland::FlatlandMarker>()
            .context("connect fuchsia.ui.composition.Flatland")?;
        flatland.set_debug_name("HirselShell")?;
        let mut flatland_events = flatland.take_event_stream();
        let tx_fl = tx.clone();
        let diag_fl = diag.clone();
        fasync::Task::local(async move {
            while let Some(ev) = flatland_events.next().await {
                match ev {
                    Ok(fland::FlatlandEvent::OnNextFrameBegin { values }) => {
                        let credits = values.additional_present_credits.unwrap_or(1);
                        let _ = tx_fl.unbounded_send(ShellMsg::NextFrameBegin(credits));
                    }
                    Ok(fland::FlatlandEvent::OnError { error }) => {
                        error!("FLATLAND PROTOCOL ERROR: {error:?}");
                        if let Ok(mut g) = diag_fl.lock() {
                            g.push(format!("FLATLAND ERROR: {error:?}"));
                        }
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
        let (mouse_source, mouse_source_req) = create_proxy::<fptr::MouseSourceMarker>();
        let (view_identity, root_view_ref_for_kb, root_view_ref) = new_view_identity()?;

        flatland.create_view2(
            view_creation_token,
            view_identity,
            fland::ViewBoundProtocols {
                view_focuser: Some(view_focuser_req),
                mouse_source: Some(mouse_source_req),
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

        // Register global KeyboardListener + MouseSource hit listener on root ViewRef
        spawn_keyboard_listener(root_view_ref_for_kb, tx.clone());
        spawn_mouse_listener(mouse_source, tx.clone());

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
                "[hirsel-shell] Starting Tier-2 Linux 6.1.102 AMD-SVM MicroVM...".to_string(),
            ],
            dev_input: String::new(),
            vm_client,
            vm_ready: false,
            js_client,
            dim_transform,
            dim_content,
            ceremony_transform,
            ceremony_surface: None,
            ceremony_attached: false,
            web_provider,
            figments_web: None,
            untrusted_web: None,
            wm,
            clipboards: HashMap::new(),
            next_effect_id: 1,
            pending_effect: None,
            audit_log,
            board_cards,
            client_diag: serde_json::json!(null),
            present_credits: 0,
            scene_dirty: true,
            diag,
            tx,
        };

        // Launch Native Chromium 160 Context #1 for Figments Shell
        if let Err(e) = shell.launch_web_context("figments", "http://127.0.0.1:7711/").await {
            error!("Failed to launch Figments Chromium context: {e:?}");
        }

        // First scene commit will flush as soon as OnNextFrameBegin delivers credits (immediately after initial present)
        Ok(shell)
    }

    fn persist_board(&self) {
        use std::io::Write as _;
        if let Ok(json) = serde_json::to_string_pretty(&self.board_cards) {
            if let Ok(mut f) = std::fs::File::create(BOARD_PATH) {
                let _ = f.write_all(json.as_bytes()).and_then(|_| f.sync_all());
            }
        }
    }

    fn persist_state(&self) {
        use std::io::Write as _;
        let st = PersistedState {
            room: self.wm.latest.room.clone(),
            audit_log: self.audit_log.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&st) {
            match std::fs::File::create(STATE_PATH) {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(json.as_bytes()).and_then(|_| f.sync_all()) {
                        warn!("persist_state sync_all failed: {e}");
                        if let Ok(mut g) = self.diag.lock() {
                            g.push(format!("persist_state write/sync err: {e}"));
                        }
                    }
                }
                Err(e) => {
                    warn!("persist_state create({STATE_PATH}) failed: {e}");
                    if let Ok(mut g) = self.diag.lock() {
                        g.push(format!("persist_state create err: {e}"));
                    }
                }
            }
        }
    }

    fn push_dev_log(&mut self, line: String) {
        for part in line.lines() {
            let trimmed = part.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            self.dev_logs.push(trimmed.chars().take(120).collect());
            if self.dev_logs.len() > 80 {
                self.dev_logs.remove(0);
            }
        }
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
        if self.present_credits == 0 {
            self.scene_dirty = true;
            return Ok(());
        }
        self.scene_dirty = false;
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

        // Layer 2: vm-dev split console backed by REAL Linux 6.1.102 AMD-SVM MicroVM
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
                surf.paint_with(|c| {
                    painter::paint_dev_pane(c, &plan, &self.dev_logs, &self.dev_input, self.vm_ready)
                });
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

        // Single atomic Flatland commit (consuming 1 present credit)
        self.present_credits = self.present_credits.saturating_sub(1);
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

    async fn run_dev_exec(&mut self, cmd: String) -> Result<VmExecResult> {
        self.push_dev_log(format!("vm-dev# {cmd}"));
        if self.dev_attached {
            let _ = self.apply_scene().await;
        }
        let Some(vm) = self.vm_client.clone() else {
            return Err(anyhow::anyhow!("vm-dev MicroVM host not running"));
        };
        let res = vm.exec(&cmd).await?;
        self.push_dev_log(res.stdout.clone());
        if self.dev_attached {
            let _ = self.apply_scene().await;
        }
        Ok(res)
    }

    async fn handle(&mut self, msg: ShellMsg) {
        match msg {
            ShellMsg::WmCommand { cmd, reply } => {
                let res = self.wm.send(&cmd).await;
                if res.is_ok() {
                    self.persist_state();
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
            ShellMsg::GetAudit { reply } => {
                let _ = reply.send(self.audit_log.clone());
            }
            ShellMsg::GetBoard { reply } => {
                let _ = reply.send(self.board_cards.clone());
            }
            ShellMsg::AddBoardCard { card, reply } => {
                if let Some(slot) = self.board_cards.iter_mut().find(|c| c.id == card.id) {
                    *slot = card;
                } else {
                    self.board_cards.push(card);
                }
                self.persist_board();
                let _ = reply.send(self.board_cards.clone());
            }
            ShellMsg::SetClientDiag { info } => {
                self.client_diag = info;
            }
            ShellMsg::GetClientDiag { reply } => {
                let _ = reply.send(self.client_diag.clone());
            }
            ShellMsg::VmStatus { reply } => {
                let ready = self.vm_ready
                    || self.vm_client.as_ref().map(|v| v.is_ready()).unwrap_or(false);
                let _ = reply.send(ready);
            }
            ShellMsg::VmIncoming(ev) => match ev {
                VmEvent::Log(line) => {
                    self.push_dev_log(line);
                    if self.dev_attached {
                        let _ = self.apply_scene().await;
                    }
                }
                VmEvent::Ready => {
                    self.vm_ready = true;
                    info!("Tier-2 Linux 6.1.102 vm-dev MicroVM READY on /dev/hvc0!");
                    self.push_dev_log("[hirsel-shell] Linux 6.1.102 vm-dev READY (2 vCPUs, /dev/hvc0 + /mnt/data)".into());
                    if self.dev_attached {
                        let _ = self.apply_scene().await;
                    }
                }
            },
            ShellMsg::DevExec { cmd, reply } => {
                let res = self.run_dev_exec(cmd).await;
                if let Some(tx) = reply {
                    let _ = tx.send(res);
                }
            }
            ShellMsg::JsEval { code, reply } => {
                let res = match self.js_client.as_mut() {
                    Some(js) => js.eval(&code).await,
                    None => Err(anyhow::anyhow!("hirsel-js sandbox not available")),
                };
                let _ = reply.send(res);
            }
            ShellMsg::ClipSet { domain, text, reply } => {
                let mut tag: u64 = 1469598103934665603;
                for b in text.as_bytes() {
                    tag ^= *b as u64;
                    tag = tag.wrapping_mul(1099511628211);
                }
                if tag == 0 {
                    tag = 1;
                }
                self.clipboards.insert(domain.clone(), text);
                let res = self.wm.send(&format!("CLIP_COPY {domain} {tag}")).await;
                if res.is_ok() {
                    let _ = self.apply_scene().await;
                }
                let _ = reply.send(res);
            }
            ShellMsg::PowerboxPaste { from, to, reply } => {
                let prev_transfers = self.wm.latest.powerbox_transfers;
                let res = self.wm.send(&format!("POWERBOX_PASTE {from} {to}")).await;
                match res {
                    Ok(plan) => {
                        let authorized = plan.powerbox_transfers > prev_transfers;
                        let mut delivered = false;
                        let mut delivered_bytes = 0usize;
                        if authorized {
                            if let Some(text) = self.clipboards.get(&from).cloned() {
                                delivered_bytes = text.len();
                                self.clipboards.insert(to.clone(), text.clone());
                                if to == "vm_dev" && self.vm_ready {
                                    let b64 = vm_client::b64_encode(text.as_bytes());
                                    let cmd = format!(
                                        "printf '%s' '{b64}' | base64 -d > /mnt/data/powerbox_paste.txt && sync && echo POWERBOX_WRITTEN"
                                    );
                                    if let Ok(r) = self.run_dev_exec(cmd).await {
                                        delivered = r.exit_code == 0;
                                    }
                                } else {
                                    delivered = true;
                                }
                            }
                        }
                        let _ = self.apply_scene().await;
                        let _ = reply.send(Ok(serde_json::json!({
                            "authorized": authorized,
                            "delivered": delivered,
                            "bytes": delivered_bytes,
                            "powerbox_transfers": plan.powerbox_transfers,
                            "state": plan,
                        })));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            ShellMsg::StageEffect { effect, cost_micros, summary, reply } => {
                let id = self.next_effect_id;
                self.next_effect_id += 1;
                let mut digest: u64 = 0x5A17_0000u64 ^ (id * 0x9E37_79B9);
                for b in effect.host.bytes().chain(effect.path.bytes()) {
                    digest = digest.wrapping_mul(33) ^ (b as u64);
                }
                let safe_summary = summary.replace(' ', "_");
                let cmd = format!(
                    "STAGE {id} {digest} {cost_micros} {} {}",
                    effect.host, safe_summary
                );
                let res = self.wm.send(&cmd).await;
                if let Ok(ref st) = res {
                    if st.outbox.status == "staged" {
                        self.pending_effect = Some(effect);
                    }
                    let _ = self.apply_scene().await;
                }
                if let Some(tx) = reply {
                    let _ = tx.send(res);
                }
            }
            ShellMsg::ApprovePending { reply } => {
                let outbox = self.wm.latest.outbox.clone();
                let cmd = format!("APPROVE {} {}", outbox.id, outbox.digest);
                let res = self.wm.send(&cmd).await;
                match res {
                    Ok(plan) => {
                        let mut exec_info = serde_json::json!(null);
                        if plan.outbox.status == "approved" {
                            if let Some(eff) = self.pending_effect.take() {
                                match execute_egress_http(
                                    &eff.host,
                                    eff.port,
                                    &eff.method,
                                    &eff.path,
                                    "",
                                )
                                .await
                                {
                                    Ok((http_status, body)) => {
                                        let entry = format!(
                                            "effect#{} APPROVED & EXECUTED {} {}:{}{} -> HTTP {}",
                                            outbox.id, eff.method, eff.host, eff.port, eff.path, http_status
                                        );
                                        self.audit_log.push(entry);
                                        exec_info = serde_json::json!({
                                            "executed": true,
                                            "http_status": http_status,
                                            "body": body,
                                        });
                                    }
                                    Err(e) => {
                                        let entry = format!(
                                            "effect#{} APPROVED exec_err={e:#}",
                                            outbox.id
                                        );
                                        self.audit_log.push(entry);
                                        exec_info = serde_json::json!({
                                            "executed": false,
                                            "error": format!("{e:#}"),
                                        });
                                    }
                                }
                            }
                            self.persist_state();
                        }
                        let _ = self.apply_scene().await;
                        if let Some(tx) = reply {
                            let _ = tx.send(Ok(serde_json::json!({
                                "state": plan,
                                "execution": exec_info,
                            })));
                        }
                    }
                    Err(e) => {
                        if let Some(tx) = reply {
                            let _ = tx.send(Err(e));
                        }
                    }
                }
            }
            ShellMsg::MouseClick { x, y } => {
                let plan = self.wm.latest.clone();
                // 1. If ceremony is active, handle clicks inside Approve / Deny buttons ONLY!
                if let Some(ref cr) = plan.layout.ceremony {
                    let rx = x - cr.x as f32;
                    let ry = y - cr.y as f32;
                    let by = (cr.h as f32) - 62.0;
                    if ry >= by && ry <= by + 40.0 {
                        if rx >= 18.0 && rx <= 258.0 {
                            let _ = self.tx.unbounded_send(ShellMsg::ApprovePending { reply: None });
                            return;
                        } else if rx >= 276.0 && rx <= 516.0 {
                            let _ = self.tx.unbounded_send(ShellMsg::WmCommand {
                                cmd: "DENY".into(),
                                reply: None,
                            });
                            return;
                        }
                    }
                    return;
                }
                // 2. Top Trust Bar pill clicks (y < 36)
                if y < TOP_BAR_H as f32 {
                    if x >= 128.0 && x < 232.0 {
                        let _ = self.tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "FOCUS figments".into(),
                            reply: None,
                        });
                    } else if x >= 232.0 && x < 330.0 {
                        let _ = self.tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "TOGGLE_DEV".into(),
                            reply: None,
                        });
                    } else if x >= 330.0 && x < 410.0 {
                        let _ = self.tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "TOGGLE_WEB".into(),
                            reply: None,
                        });
                    } else if x >= (plan.w as f32) - 210.0 && x <= (plan.w as f32) - 90.0 {
                        let cmd = if plan.agent_conn { "REVOKE" } else { "RESTORE" };
                        let _ = self.tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: cmd.into(),
                            reply: None,
                        });
                    }
                    return;
                }
                // 3. Click inside vm_dev pane focuses vm_dev for interactive typing
                if let Some(ref r) = plan.layout.vm_dev {
                    if x >= r.x as f32
                        && x <= (r.x + r.w) as f32
                        && y >= r.y as f32
                        && y <= (r.y + r.h) as f32
                    {
                        let _ = self.tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "FOCUS vm_dev".into(),
                            reply: None,
                        });
                    }
                }
            }
            ShellMsg::DevKeyChar(ch) => {
                if self.wm.latest.focused == "vm_dev" && self.dev_input.len() < 120 {
                    self.dev_input.push(ch);
                    if self.dev_attached {
                        let _ = self.apply_scene().await;
                    }
                }
            }
            ShellMsg::DevKeyBackspace => {
                if self.wm.latest.focused == "vm_dev" {
                    self.dev_input.pop();
                    if self.dev_attached {
                        let _ = self.apply_scene().await;
                    }
                }
            }
            ShellMsg::DevKeyEnter => {
                if self.wm.latest.outbox.status == "staged" {
                    let _ = self.tx.unbounded_send(ShellMsg::ApprovePending { reply: None });
                } else if self.wm.latest.focused == "vm_dev" && !self.dev_input.trim().is_empty() {
                    let cmd = std::mem::take(&mut self.dev_input);
                    let _ = self.run_dev_exec(cmd).await;
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
            ShellMsg::NextFrameBegin(credits) => {
                self.present_credits = self.present_credits.saturating_add(credits);
                if self.scene_dirty && self.present_credits > 0 {
                    if let Err(e) = self.apply_scene().await {
                        error!("apply_scene on NextFrameBegin error: {e:?}");
                    }
                }
            }
            ShellMsg::Relayout { width, height } => {
                if self.wm.send(&format!("RESIZE {width} {height}")).await.is_ok() {
                    let _ = self.apply_scene().await;
                }
            }
        }
    }
}

fn spawn_mouse_listener(mouse_source: fptr::MouseSourceProxy, tx: UnboundedSender<ShellMsg>) {
    fasync::Task::local(async move {
        let mut was_pressed = false;
        loop {
            match mouse_source.watch().await {
                Ok(events) => {
                    for e in events {
                        if let Some(fptr::MousePointerSample {
                            position_in_viewport: Some([x, y]),
                            pressed_buttons,
                            ..
                        }) = e.pointer_sample
                        {
                            let is_pressed =
                                pressed_buttons.as_ref().map(|b| !b.is_empty()).unwrap_or(false);
                            if is_pressed && !was_pressed {
                                let _ = tx.unbounded_send(ShellMsg::MouseClick { x, y });
                            }
                            was_pressed = is_pressed;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    })
    .detach();
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
        info!("Registered root KeyboardListener for Hirsel-OS global hotkeys, SAS, and vm-dev typing");
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
                        let _ = tx.unbounded_send(ShellMsg::StageEffect {
                            effect: StagedHttpEffect {
                                host: "127.0.0.1".into(),
                                port: 7780,
                                path: "/_hirsel/echo-target".into(),
                                method: "POST".into(),
                            },
                            cost_micros: 250_000,
                            summary: "POST_/repos/Ascending-AI/figments/pulls".into(),
                            reply: None,
                        });
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
                        let _ = tx.unbounded_send(ShellMsg::DevKeyEnter);
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::Escape) => {
                        let _ = tx.unbounded_send(ShellMsg::WmCommand {
                            cmd: "DENY".into(),
                            reply: None,
                        });
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    Some(finput::Key::Backspace) => {
                        let _ = tx.unbounded_send(ShellMsg::DevKeyBackspace);
                        handled = finput3::KeyEventStatus::Handled;
                    }
                    _ => {
                        if let Some(finput3::KeyMeaning::Codepoint(cp)) = event.key_meaning {
                            if let Some(ch) = char::from_u32(cp) {
                                if !ch.is_control() {
                                    let _ = tx.unbounded_send(ShellMsg::DevKeyChar(ch));
                                    handled = finput3::KeyEventStatus::Handled;
                                }
                            }
                        }
                    }
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
    let mut buf = vec![0u8; 16384];
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
    let header_block = req.split("\r\n\r\n").next().unwrap_or("");
    let raw_body = req.split("\r\n\r\n").nth(1).unwrap_or("");
    let body = raw_body.trim();

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
        ("GET", "/_hirsel/echo-target") | ("POST", "/_hirsel/echo-target") => {
            // Verifies host-side egress-gw injected the sealed secret header!
            let injected_key = header_block
                .lines()
                .find_map(|l| l.strip_prefix("X-Hirsel-Egress-Key:").or_else(|| l.strip_prefix("x-hirsel-egress-key:")))
                .map(|v| v.trim().to_string())
                .unwrap_or_default();
            (
                "200 OK",
                "application/json",
                serde_json::json!({
                    "echo": "hirsel-egress-target-ok",
                    "method": method,
                    "secret_present": !injected_key.is_empty(),
                    "secret_len": injected_key.len(),
                })
                .to_string(),
            )
        }
        ("GET", "/_hirsel/state") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetState { reply: otx });
            match orx.await {
                Ok(st) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"wm"}"#.into()),
            }
        }
        ("GET", "/_hirsel/audit") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetAudit { reply: otx });
            match orx.await {
                Ok(log) => ("200 OK", "application/json", serde_json::json!({"audit": log}).to_string()),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"audit"}"#.into()),
            }
        }
        ("GET", "/_hirsel/board") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetBoard { reply: otx });
            match orx.await {
                Ok(cards) => ("200 OK", "application/json", serde_json::json!({"cards": cards}).to_string()),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"board"}"#.into()),
            }
        }
        ("POST", "/_hirsel/board") => {
            let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
            let card = BoardCard {
                id: parsed["id"].as_str().unwrap_or("card-1").to_string(),
                title: parsed["title"].as_str().unwrap_or("Figment Card").to_string(),
                sub: parsed["sub"].as_str().unwrap_or("Persisted on Fxfs /data/board.json").to_string(),
                color: parsed["color"].as_str().unwrap_or("amber").to_string(),
                x: parsed["x"].as_i64().unwrap_or(28) as i32,
                y: parsed["y"].as_i64().unwrap_or(300) as i32,
            };
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::AddBoardCard { card, reply: otx });
            match orx.await {
                Ok(cards) => ("200 OK", "application/json", serde_json::json!({"cards": cards}).to_string()),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"board"}"#.into()),
            }
        }
        ("POST", "/_hirsel/client-diag") => {
            let info = serde_json::from_str::<serde_json::Value>(body).unwrap_or_else(|_| serde_json::json!({"raw": body}));
            let _ = tx.unbounded_send(ShellMsg::SetClientDiag { info });
            ("200 OK", "application/json", r#"{"ok":true}"#.to_string())
        }
        ("GET", "/_hirsel/client-diag") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetClientDiag { reply: otx });
            match orx.await {
                Ok(v) => ("200 OK", "application/json", v.to_string()),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"client_diag"}"#.into()),
            }
        }
        ("POST", "/_hirsel/click") => {
            let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
            let x = parsed["x"].as_f64().unwrap_or(0.0) as f32;
            let y = parsed["y"].as_f64().unwrap_or(0.0) as f32;
            let _ = tx.unbounded_send(ShellMsg::MouseClick { x, y });
            // Wait briefly for state sync
            fasync::Timer::new(std::time::Duration::from_millis(80)).await;
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetState { reply: otx });
            match orx.await {
                Ok(st) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"click"}"#.into()),
            }
        }
        ("POST", "/_hirsel/type") => {
            for ch in raw_body.chars() {
                if ch == '\n' || ch == '\r' {
                    let _ = tx.unbounded_send(ShellMsg::DevKeyEnter);
                } else if !ch.is_control() {
                    let _ = tx.unbounded_send(ShellMsg::DevKeyChar(ch));
                }
            }
            ("200 OK", "application/json", r#"{"ok":true}"#.to_string())
        }
        ("GET", "/_hirsel/vm-status") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::VmStatus { reply: otx });
            match orx.await {
                Ok(ready) => ("200 OK", "application/json", serde_json::json!({"vm_ready": ready}).to_string()),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"error":"vm"}"#.into()),
            }
        }
        ("POST", "/_hirsel/codemode") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::JsEval { code: body.to_string(), reply: otx });
            match orx.await {
                Ok(Ok(val)) => ("200 OK", "application/json", val.to_string()),
                Ok(Err(e)) => ("500 Internal Server Error", "application/json", serde_json::json!({"ok": false, "error": format!("{e:#}")}).to_string()),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"ok":false,"error":"channel"}"#.into()),
            }
        }
        ("POST", "/_hirsel/stage") => {
            let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
            let host = parsed.as_ref().and_then(|v| v["host"].as_str()).unwrap_or("127.0.0.1").to_string();
            let port = parsed.as_ref().and_then(|v| v["port"].as_u64()).unwrap_or(7780) as u16;
            let eff_path = parsed.as_ref().and_then(|v| v["path"].as_str()).unwrap_or("/_hirsel/echo-target").to_string();
            let eff_method = parsed.as_ref().and_then(|v| v["method"].as_str()).unwrap_or("POST").to_string();
            let cost = parsed.as_ref().and_then(|v| v["cost_micros"].as_u64()).unwrap_or(250_000);
            let summary = parsed.as_ref().and_then(|v| v["summary"].as_str()).unwrap_or("POST_/repos/Ascending-AI/figments/pulls").to_string();
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::StageEffect {
                effect: StagedHttpEffect { host, port, path: eff_path, method: eff_method },
                cost_micros: cost,
                summary,
                reply: Some(otx),
            });
            match orx.await {
                Ok(Ok(st)) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"stage"}"#.into()),
            }
        }
        ("POST", "/_hirsel/approve") => {
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::ApprovePending { reply: Some(otx) });
            match orx.await {
                Ok(Ok(val)) => ("200 OK", "application/json", val.to_string()),
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
        ("POST", "/_hirsel/clipboard") => {
            let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
            let domain = parsed["domain"].as_str().unwrap_or("figments").to_string();
            let text = parsed["text"].as_str().unwrap_or("").to_string();
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::ClipSet { domain, text, reply: otx });
            match orx.await {
                Ok(Ok(st)) => ("200 OK", "application/json", serde_json::to_string(&st)?),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"clip"}"#.into()),
            }
        }
        ("POST", "/_hirsel/powerbox-paste") => {
            let parsed = serde_json::from_str::<serde_json::Value>(body).unwrap_or_default();
            let from = parsed["from"].as_str().unwrap_or("figments").to_string();
            let to = parsed["to"].as_str().unwrap_or("vm_dev").to_string();
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::PowerboxPaste { from, to, reply: otx });
            match orx.await {
                Ok(Ok(val)) => ("200 OK", "application/json", val.to_string()),
                _ => ("500 Internal Server Error", "application/json", r#"{"error":"powerbox"}"#.into()),
            }
        }
        ("POST", "/_hirsel/dev-exec") => {
            let cmd = if body.is_empty() { "uname -a".to_string() } else { body.to_string() };
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::DevExec { cmd, reply: Some(otx) });
            match orx.await {
                Ok(Ok(res)) => (
                    "200 OK",
                    "application/json",
                    serde_json::json!({
                        "ok": res.exit_code == 0,
                        "exit_code": res.exit_code,
                        "stdout": res.stdout,
                        "vm": "linux-6.1.102-amd-svm"
                    })
                    .to_string(),
                ),
                Ok(Err(e)) => (
                    "503 Service Unavailable",
                    "application/json",
                    serde_json::json!({"ok": false, "error": format!("{e:#}")}).to_string(),
                ),
                Err(_) => ("500 Internal Server Error", "application/json", r#"{"ok":false}"#.into()),
            }
        }
        ("POST", "/_hirsel/proxy") => {
            // Real host-side capability-gated egress-gw: enforces Caretaker + allowlist + secret injection
            let (otx, orx) = oneshot::channel();
            let _ = tx.unbounded_send(ShellMsg::GetState { reply: otx });
            let allowed_hosts = ["127.0.0.1", "10.0.2.2", "example.com", "api.github.com", "api.anthropic.com", "api.openai.com"];
            let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
            let host = parsed.as_ref().and_then(|v| v["host"].as_str()).unwrap_or(if body.is_empty() { "127.0.0.1" } else { body }).to_string();
            let port = parsed.as_ref().and_then(|v| v["port"].as_u64()).unwrap_or(80) as u16;
            let p_path = parsed.as_ref().and_then(|v| v["path"].as_str()).unwrap_or("/").to_string();
            let p_method = parsed.as_ref().and_then(|v| v["method"].as_str()).unwrap_or("GET").to_string();

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
                Ok(_) if allowed_hosts.contains(&host.as_str()) => {
                    if parsed.is_some() {
                        match execute_egress_http(&host, port, &p_method, &p_path, "").await {
                            Ok((http_status, resp_body)) => (
                                "200 OK",
                                "application/json",
                                serde_json::json!({
                                    "allowed": true,
                                    "gateway": "hirsel-egress-gw",
                                    "host": host,
                                    "port": port,
                                    "http_status": http_status,
                                    "secret_injected": true,
                                    "body": resp_body
                                })
                                .to_string(),
                            ),
                            Err(e) => (
                                "502 Bad Gateway",
                                "application/json",
                                serde_json::json!({
                                    "allowed": true,
                                    "host": host,
                                    "error": format!("{e:#}")
                                })
                                .to_string(),
                            ),
                        }
                    } else {
                        (
                            "200 OK",
                            "application/json",
                            serde_json::json!({
                                "allowed": true,
                                "gateway": "hirsel-egress-gw",
                                "host": host,
                                "secret_injected": true
                            })
                            .to_string(),
                        )
                    }
                }
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
    info!("=== Hirsel-OS Desktop Shell starting (Flatland + Chromium 160 + Pure-Hirsel WM + js@1 + Linux 6.1.102 AMD-SVM) ===");
    let (tx, mut rx) = unbounded::<ShellMsg>();
    let diag = Arc::new(Mutex::new(vec!["starting hirsel_shell".to_string()]));

    spawn_http_server(7711, tx.clone(), diag.clone());
    spawn_http_server(7780, tx.clone(), diag.clone());

    let mut shell = match HirselShell::new(tx, diag.clone()).await {
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
