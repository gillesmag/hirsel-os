// Spawns pure-Hirsel `/pkg/bin/hirsel-agent` (zero ambient capabilities, std.model.explore verified)
// inside a child Zircon Job. Drives deterministic multi-tool agent workflows (js@1 -> vm-dev ->
// hirsel-board -> Protected Outbox Ceremony) with zero direct external mutation authority.

use anyhow::{Context, Result, anyhow};
use fuchsia_async as fasync;
use fuchsia_runtime::{HandleInfo, HandleType, job_default};
use futures::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use log::info;

pub struct AgentClient {
    _job: zx::Job,
    _process: zx::Process,
    stdin: fasync::Socket,
    stdout: BufReader<fasync::Socket>,
    pub latest: serde_json::Value,
}

impl AgentClient {
    pub async fn spawn() -> Result<Self> {
        let job = job_default()
            .create_child_job()
            .context("create child Zircon Job for hirsel-agent")?;

        let (stdin_host, stdin_child) = zx::Socket::create_stream();
        let (stdout_host, stdout_child) = zx::Socket::create_stream();

        let mut actions = vec![
            fdio::SpawnAction::set_name(c"hirsel-agent"),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 0),
                stdin_child.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 1),
                stdout_child.into_handle(),
            ),
        ];
        if let Ok(utc) = fuchsia_runtime::duplicate_utc_clock_handle(zx::Rights::SAME_RIGHTS) {
            actions.push(fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::ClockUtc, 0),
                utc.into_handle(),
            ));
        }

        let process = fdio::spawn_etc(
            &job,
            fdio::SpawnOptions::DEFAULT_LOADER,
            c"/pkg/bin/hirsel-agent",
            &[c"hirsel-agent"],
            None,
            &mut actions,
        )
        .map_err(|(s, m)| anyhow!("spawn /pkg/bin/hirsel-agent failed: {s}: {m}"))?;

        let pid = process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        let stdin = fasync::Socket::from_socket(stdin_host);
        let mut stdout = BufReader::new(fasync::Socket::from_socket(stdout_host));

        let mut line = String::new();
        let n = stdout.read_line(&mut line).await.context("read initial hirsel-agent state")?;
        if n == 0 {
            return Err(anyhow!("hirsel-agent exited before initial state"));
        }
        let latest: serde_json::Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-agent JSON: {}", line.trim()))?;
        info!("Spawned pure-Hirsel hirsel-agent (pid koid={pid})");

        Ok(Self { _job: job, _process: process, stdin, stdout, latest })
    }

    pub fn process(&self) -> &zx::Process {
        &self._process
    }

    pub async fn send(&mut self, line_cmd: &str) -> Result<serde_json::Value> {
        let mut payload = line_cmd.replace(['\r', '\n'], " ");
        payload.push('\n');
        self.stdin.write_all(payload.as_bytes()).await.context("write hirsel-agent")?;
        self.stdin.flush().await.ok();

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await.context("read hirsel-agent")?;
        if n == 0 {
            return Err(anyhow!("hirsel-agent closed stdout"));
        }
        let val: serde_json::Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-agent JSON: {}", line.trim()))?;
        self.latest = val.clone();
        Ok(val)
    }
}
