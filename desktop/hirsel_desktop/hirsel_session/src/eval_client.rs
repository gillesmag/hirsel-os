// Spawns OS-resident `/pkg/bin/hirsel-eval` (real Hirsel compiler + typechecker + evaluator)
// inside an isolated child Zircon Job with ONLY stdin/stdout zx::Sockets and read-only `/pkg`
// (for `/pkg/data/hirsel/core` and `/pkg/data/hirsel/os`). Zero `/svc`, zero `/data`, zero net!

use anyhow::{Context, Result, anyhow};
use fidl::endpoints::Proxy;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_runtime::{HandleInfo, HandleType, job_default};
use futures::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use log::info;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HirselEvalResult {
    pub ok: bool,
    #[serde(default)]
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub logs: Vec<String>,
    #[serde(default)]
    pub ops: Vec<Value>,
    #[serde(default)]
    pub used_bytes: u64,
    #[serde(default)]
    pub compile_ms: u64,
    #[serde(default)]
    pub run_ms: u64,
}

pub struct EvalClient {
    _job: zx::Job,
    process: zx::Process,
    stdin: fasync::Socket,
    stdout: BufReader<fasync::Socket>,
    pub banner: Value,
}

impl EvalClient {
    pub async fn spawn() -> Result<Self> {
        let job = job_default()
            .create_child_job()
            .context("create child Zircon Job for hirsel-eval")?;

        let (stdin_host, stdin_child) = zx::Socket::create_stream();
        let (stdout_host, stdout_child) = zx::Socket::create_stream();

        let pkg_dir = fuchsia_fs::directory::open_in_namespace(
            "/pkg",
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )
        .context("open read-only /pkg for hirsel-eval")?
        .into_client_end()
        .map_err(|_| anyhow!("into_client_end(/pkg) failed"))?;

        let mut actions = vec![
            fdio::SpawnAction::set_name(c"hirsel-eval"),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 0),
                stdin_child.into_handle(),
            ),
            fdio::SpawnAction::add_handle(
                HandleInfo::new(HandleType::FileDescriptor, 1),
                stdout_child.into_handle(),
            ),
            fdio::SpawnAction::add_namespace_entry(c"/pkg", pkg_dir.into_channel().into_handle()),
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
            c"/pkg/bin/hirsel-eval",
            &[c"hirsel-eval", c"--core", c"/pkg/data/hirsel/core", c"--os", c"/pkg/data/hirsel/os"],
            None,
            &mut actions,
        )
        .map_err(|(s, m)| anyhow!("spawn /pkg/bin/hirsel-eval failed: {s}: {m}"))?;

        let pid = process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        let stdin = fasync::Socket::from_socket(stdin_host);
        let mut stdout = BufReader::new(fasync::Socket::from_socket(stdout_host));

        let mut line = String::new();
        let n = stdout.read_line(&mut line).await.context("read hirsel-eval ready banner")?;
        if n == 0 {
            return Err(anyhow!("hirsel-eval exited before ready banner"));
        }
        let banner: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-eval banner: {}", line.trim()))?;
        if banner["ready"].as_bool() != Some(true) {
            return Err(anyhow!("hirsel-eval startup failed: {banner}"));
        }
        info!("Spawned OS-resident Hirsel compiler/evaluator (pid koid={pid}): {banner}");

        Ok(Self {
            _job: job,
            process,
            stdin,
            stdout,
            banner,
        })
    }

    pub fn process(&self) -> &zx::Process {
        &self.process
    }

    async fn call(&mut self, payload: Value) -> Result<HirselEvalResult> {
        let mut wire = payload.to_string();
        wire.push('\n');
        self.stdin.write_all(wire.as_bytes()).await.context("write hirsel-eval")?;
        self.stdin.flush().await.ok();

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await.context("read hirsel-eval response")?;
        if n == 0 {
            return Err(anyhow!("hirsel-eval closed stdout"));
        }
        let res: HirselEvalResult = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-eval response: {}", line.trim()))?;
        Ok(res)
    }

    pub async fn check(&mut self, source: &str) -> Result<HirselEvalResult> {
        self.call(json!({
            "op": "check",
            "source": source,
        }))
        .await
    }

    pub async fn eval(&mut self, source: &str, stdin: &str, limit_bytes: u64) -> Result<HirselEvalResult> {
        self.call(json!({
            "op": "eval",
            "source": source,
            "stdin": stdin,
            "limit_bytes": limit_bytes,
        }))
        .await
    }
}
