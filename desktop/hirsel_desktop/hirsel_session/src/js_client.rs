// Spawns pure-Hirsel `/pkg/bin/hirsel-js` (Figments `js@1` deterministic JS engine)
// inside a zero-ambient Zircon Job (only stdin/stdout zx::Sockets) for sandboxed codemode eval.

use anyhow::{Context, Result, anyhow};
use fuchsia_async as fasync;
use fuchsia_runtime::{HandleInfo, HandleType, job_default};
use futures::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use log::info;

pub struct JsSandboxClient {
    _job: zx::Job,
    _process: zx::Process,
    stdin: fasync::Socket,
    stdout: BufReader<fasync::Socket>,
}

impl JsSandboxClient {
    pub async fn spawn() -> Result<Self> {
        let job = job_default()
            .create_child_job()
            .context("create child Zircon Job for hirsel-js")?;

        let (stdin_host, stdin_child) = zx::Socket::create_stream();
        let (stdout_host, stdout_child) = zx::Socket::create_stream();

        let mut actions = vec![
            fdio::SpawnAction::set_name(c"hirsel-js"),
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
            c"/pkg/bin/hirsel-js",
            &[c"hirsel-js"],
            None,
            &mut actions,
        )
        .map_err(|(s, m)| anyhow!("spawn /pkg/bin/hirsel-js failed: {s}: {m}"))?;

        let pid = process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        let stdin = fasync::Socket::from_socket(stdin_host);
        let mut stdout = BufReader::new(fasync::Socket::from_socket(stdout_host));

        let mut banner = String::new();
        let n = stdout.read_line(&mut banner).await.context("read hirsel-js ready banner")?;
        if n == 0 {
            return Err(anyhow!("hirsel-js exited before ready banner"));
        }
        info!("Spawned pure-Hirsel Figments js@1 sandbox (pid koid={pid}): {}", banner.trim());

        Ok(Self { _job: job, _process: process, stdin, stdout })
    }

    pub fn process(&self) -> &zx::Process {
        &self._process
    }

    pub async fn eval(&mut self, code: &str) -> Result<serde_json::Value> {
        // Collapse embedded newlines into spaces for single-line framing
        let one_line = code.replace(['\r', '\n'], " ");
        let mut payload = one_line;
        payload.push('\n');
        self.stdin.write_all(payload.as_bytes()).await.context("write JS to hirsel-js")?;
        self.stdin.flush().await.ok();

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await.context("read hirsel-js result")?;
        if n == 0 {
            return Err(anyhow!("hirsel-js closed stdout"));
        }
        let val: serde_json::Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-js JSON: {}", line.trim()))?;
        Ok(val)
    }
}
