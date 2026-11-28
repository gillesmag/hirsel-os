// Spawns the pure-Hirsel /pkg/bin/hirsel-wm binary inside a dedicated Zircon Job
// with ZERO ambient namespace/directory handles — only stdin and stdout zx::Sockets.

use anyhow::{Context, Result, anyhow};
use fuchsia_async as fasync;
use fuchsia_runtime::{HandleInfo, HandleType, job_default};
use futures::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use log::info;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RectJson {
    pub x: u64,
    pub y: u64,
    pub w: u64,
    pub h: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboxJson {
    pub id: u64,
    pub digest: u64,
    pub cost_micros: u64,
    pub status: String,
    pub target: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutJson {
    pub figments: RectJson,
    pub vm_dev: Option<RectJson>,
    #[serde(default)]
    pub activity: Option<RectJson>,
    pub web: Option<RectJson>,
    pub ceremony: Option<RectJson>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenePlan {
    pub w: u64,
    pub h: u64,
    pub room: String,
    pub focused: String,
    pub sas: bool,
    pub capture_allowed: bool,
    pub agent_conn: bool,
    pub bank_spent_micros: u64,
    pub bank_limit_micros: u64,
    pub approved: u64,
    pub denied: u64,
    pub powerbox_transfers: u64,
    pub outbox: OutboxJson,
    pub layout: LayoutJson,
}

pub struct WmClient {
    _job: zx::Job,
    process: zx::Process,
    stdin: fasync::Socket,
    stdout: BufReader<fasync::Socket>,
    stderr: zx::Socket,
    pub latest: ScenePlan,
}

impl WmClient {
    pub async fn spawn() -> Result<Self> {
        // Confine pure-Hirsel hirsel-wm inside its own child Zircon Job with zero ambient handles.
        let job = job_default()
            .create_child_job()
            .context("create child Zircon Job for hirsel-wm")?;

        let (stdin_host, stdin_child) = zx::Socket::create_stream();
        let (stdout_host, stdout_child) = zx::Socket::create_stream();
        let (stderr_host, stderr_child) = zx::Socket::create_stream();

        let mut actions = vec![
            fdio::SpawnAction::set_name(c"hirsel-wm"),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 0),
                stdin_child.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 1),
                stdout_child.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 2),
                stderr_child.into_handle(),
            ),
        ];
        if let Ok(utc) = fuchsia_runtime::duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS) {
            actions.push(fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::ClockUtc, 0),
                utc.into_handle(),
            ));
        }

        let options = fdio::SpawnOptions::DEFAULT_LOADER;
        let process = fdio::spawn_etc(
            &job,
            options,
            c"/pkg/bin/hirsel-wm",
            &[c"hirsel-wm"],
            None,
            &mut actions,
        )
        .map_err(|(status, msg)| anyhow!("fdio::spawn_etc(/pkg/bin/hirsel-wm) failed: {status}: {msg}"))?;

        let pid = process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        info!("Spawned pure-Hirsel hirsel-wm in isolated Zircon Job (pid koid={pid}, zero ambient handles)");

        let stdin = fasync::Socket::from_socket(stdin_host);
        let mut stdout = BufReader::new(fasync::Socket::from_socket(stdout_host));

        let mut line = String::new();
        let n = stdout.read_line(&mut line).await.context("read initial ScenePlan from hirsel-wm")?;
        if n == 0 {
            return Err(anyhow!("hirsel-wm closed stdout before initial ScenePlan"));
        }
        let latest: ScenePlan =
            serde_json::from_str(line.trim()).context("parse initial hirsel-wm ScenePlan JSON")?;
        info!(
            "hirsel-wm initial ScenePlan: {}x{} room={} focused={} bank={}/{}",
            latest.w, latest.h, latest.room, latest.focused, latest.bank_spent_micros, latest.bank_limit_micros
        );

        Ok(Self { _job: job, process, stdin, stdout, stderr: stderr_host, latest })
    }

    pub fn process(&self) -> &zx::Process {
        &self.process
    }

    fn diag_child(&self) -> String {
        let rc = self.process.info().map(|i| i.return_code).unwrap_or(-9999);
        let mut ebuf = [0u8; 512];
        let err_txt = match self.stderr.read(&mut ebuf) {
            Ok(n) => String::from_utf8_lossy(&ebuf[..n]).to_string(),
            Err(_) => String::new(),
        };
        format!("return_code={rc} stderr={err_txt:?}")
    }

    pub async fn send(&mut self, cmd: &str) -> Result<ScenePlan> {
        let mut payload = cmd.trim().to_string();
        payload.push('\n');
        if let Err(e) = self.stdin.write_all(payload.as_bytes()).await {
            let mut extra = String::new();
            let _ = self.stdout.read_line(&mut extra).await;
            return Err(anyhow!(
                "write `{cmd}` failed: {e}; child=({}) extra_stdout={extra:?}",
                self.diag_child()
            ));
        }
        self.stdin.flush().await.ok();

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await.with_context(|| {
            format!("read ScenePlan after `{cmd}` ({})", self.diag_child())
        })?;
        if n == 0 {
            return Err(anyhow!(
                "hirsel-wm exited unexpectedly after `{cmd}` ({})",
                self.diag_child()
            ));
        }
        let plan: ScenePlan = serde_json::from_str(line.trim()).with_context(|| {
            format!("parse hirsel-wm JSON: {} ({})", line.trim(), self.diag_child())
        })?;
        self.latest = plan.clone();
        Ok(plan)
    }
}
