// Spawns `/pkg/bin/hirsel-vm-host` inside an isolated Zircon Job to boot a REAL
// 2-vCPU upstream Linux 6.1.102 MicroVM (AMD SVM + NPT + persistent Fxfs /mnt/data
// + virtio-console /dev/hvc0 agent) backing the desktop `vm-dev` pane!

use anyhow::{Context, Result, anyhow};
use fidl::endpoints::Proxy;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_kernel as fkernel;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_runtime::{HandleInfo, HandleType, job_default};
use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use futures::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use log::{error, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub fn b64_encode(input: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
        out.push(TBL[(b0 >> 2) as usize] as char);
        out.push(TBL[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TBL[(((b1 & 0xf) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TBL[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Vec<u8> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let Some(v0) = val(chunk[0]) else { break };
        let Some(v1) = val(chunk[1]) else { break };
        out.push((v0 << 2) | (v1 >> 4));
        if chunk.len() > 2 && chunk[2] != b'=' {
            if let Some(v2) = val(chunk[2]) {
                out.push((v1 << 4) | (v2 >> 2));
                if chunk.len() > 3 && chunk[3] != b'=' {
                    if let Some(v3) = val(chunk[3]) {
                        out.push((v2 << 6) | v3);
                    }
                }
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct VmExecResult {
    pub exit_code: i32,
    pub stdout: String,
}

pub enum VmEvent {
    Log(String),
    Ready,
}

pub struct VmClient {
    _job: zx::Job,
    _process: zx::Process,
    stdin: Arc<futures::lock::Mutex<fasync::Socket>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<VmExecResult>>>>,
    next_id: Arc<Mutex<u64>>,
    ready: Arc<Mutex<bool>>,
}

impl VmClient {
    pub async fn spawn(on_event: UnboundedSender<VmEvent>) -> Result<Self> {
        let hyp_proxy = connect_to_protocol::<fkernel::HypervisorResourceMarker>()
            .context("connect fuchsia.kernel.HypervisorResource")?;
        let hyp_res = hyp_proxy.get().await.context("HypervisorResource.get()")?;

        let vmex_proxy = connect_to_protocol::<fkernel::VmexResourceMarker>()
            .context("connect fuchsia.kernel.VmexResource")?;
        let vmex_res = vmex_proxy.get().await.context("VmexResource.get()")?;

        let job = job_default()
            .create_child_job()
            .context("create child Zircon Job for hirsel-vm-host")?;

        let (stdin_host, stdin_child) = zx::Socket::create_stream();
        let (stdout_host, stdout_child) = zx::Socket::create_stream();

        let pkg_dir = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )
        .context("open /pkg for hirsel-vm-host")?
        .into_client_end()
        .map_err(|_| anyhow!("into_client_end(/pkg) failed"))?;

        let mut actions = vec![
            fdio::SpawnAction::set_name(c"hirsel-vm-host"),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 0),
                stdin_child.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 1),
                stdout_child.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::User0, 0),
                hyp_res.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::User0, 1),
                vmex_res.into_handle(),
            ),
            fdio::SpawnAction::add_namespace_entry(c"/pkg", pkg_dir.into_channel().into_handle()),
        ];

        if let Ok(data_dir) = fuchsia_fs::directory::open_in_namespace(
            "/data",
            fio::PERM_READABLE | fio::PERM_WRITABLE,
        ) {
            if let Ok(data_end) = data_dir.into_client_end() {
                actions.push(fdio::SpawnAction::add_namespace_entry(
                    c"/data",
                    data_end.into_channel().into_handle(),
                ));
            }
        }
        if let Ok(svc_dir) = fuchsia_fs::directory::open_in_namespace("/svc", fio::PERM_READABLE) {
            if let Ok(svc_end) = svc_dir.into_client_end() {
                actions.push(fdio::SpawnAction::add_namespace_entry(
                    c"/svc",
                    svc_end.into_channel().into_handle(),
                ));
            }
        }
        if let Ok(utc) = fuchsia_runtime::duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS) {
            actions.push(fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::ClockUtc, 0),
                utc.into_handle(),
            ));
        }

        let process = fdio::spawn_etc(
            &job,
            fdio::SpawnOptions::DEFAULT_LOADER,
            c"/pkg/bin/hirsel-vm-host",
            &[c"hirsel-vm-host"],
            None,
            &mut actions,
        )
        .map_err(|(s, m)| anyhow!("spawn /pkg/bin/hirsel-vm-host failed: {s}: {m}"))?;

        let pid = process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        info!("Spawned Tier-2 Linux 6.1.102 MicroVM host (pid koid={pid})");

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<VmExecResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let ready = Arc::new(Mutex::new(false));

        let pending_reader = Arc::clone(&pending);
        let ready_reader = Arc::clone(&ready);
        fasync::Task::local(async move {
            let mut reader = BufReader::new(fasync::Socket::from_socket(stdout_host));
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => {
                        warn!("hirsel-vm-host stdout closed");
                        break;
                    }
                    Ok(_) => {
                        let t = line.trim();
                        if t == "READY" {
                            if let Ok(mut g) = ready_reader.lock() {
                                *g = true;
                            }
                            let _ = on_event.unbounded_send(VmEvent::Ready);
                        } else if let Some(b64) = t.strip_prefix("LOG ") {
                            let msg = String::from_utf8_lossy(&b64_decode(b64)).to_string();
                            let _ = on_event.unbounded_send(VmEvent::Log(msg));
                        } else if let Some(rest) = t.strip_prefix("EXEC_RES ") {
                            let mut parts = rest.splitn(3, ' ');
                            let id = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                            let rc = parts.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(-1);
                            let b64 = parts.next().unwrap_or("");
                            let out = String::from_utf8_lossy(&b64_decode(b64)).to_string();
                            if let Ok(mut map) = pending_reader.lock() {
                                if let Some(tx) = map.remove(&id) {
                                    let _ = tx.send(VmExecResult { exit_code: rc, stdout: out });
                                }
                            }
                        } else if let Some(err) = t.strip_prefix("ERR ") {
                            error!("hirsel-vm-host error: {err}");
                            let _ = on_event.unbounded_send(VmEvent::Log(format!("[vm-err] {err}")));
                        }
                    }
                }
            }
        })
        .detach();

        Ok(Self {
            _job: job,
            _process: process,
            stdin: Arc::new(futures::lock::Mutex::new(fasync::Socket::from_socket(stdin_host))),
            pending,
            next_id: Arc::new(Mutex::new(1)),
            ready,
        })
    }

    pub fn is_ready(&self) -> bool {
        self.ready.lock().map(|g| *g).unwrap_or(false)
    }

    pub async fn exec(&self, cmd: &str) -> Result<VmExecResult> {
        let id = {
            let mut g = self.next_id.lock().unwrap();
            let id = *g;
            *g += 1;
            id
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let wire = format!("EXEC {id} {}\n", b64_encode(cmd.as_bytes()));
        {
            let mut w = self.stdin.lock().await;
            w.write_all(wire.as_bytes()).await.context("write EXEC to hirsel-vm-host")?;
            w.flush().await.ok();
        }
        rx.await.context("wait for VM EXEC_RES")
    }
}
