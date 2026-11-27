// Spawns pure-Hirsel `/pkg/bin/hirsel-board` (zero ambient capabilities, std.model.explore verified)
// inside a child Zircon Job to own canonical Figments Spatial Board state (cards, edges, camera).
// Hydrates and persists snapshots to Fxfs `/data/board.json` via sync_all().

use anyhow::{Context, Result, anyhow};
use fuchsia_async as fasync;
use fuchsia_runtime::{HandleInfo, HandleType, job_default};
use futures::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use log::info;
use serde::{Deserialize, Serialize};
use std::io::Write as _;

const BOARD_SNAPSHOT_PATH: &str = "/data/board.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoardCard {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub tone: String,
    pub x: u64,
    pub y: u64,
    pub w: u64,
    pub h: u64,
    pub rev: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoardEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoardCamera {
    pub cx: u64,
    pub cy: u64,
    pub zoom_pct: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub rev: u64,
    pub camera: BoardCamera,
    pub cards: Vec<BoardCard>,
    pub edges: Vec<BoardEdge>,
}

fn clean_field(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

pub struct BoardClient {
    _job: zx::Job,
    _process: zx::Process,
    stdin: fasync::Socket,
    stdout: BufReader<fasync::Socket>,
    pub latest: BoardSnapshot,
}

impl BoardClient {
    pub async fn spawn() -> Result<Self> {
        let job = job_default()
            .create_child_job()
            .context("create child Zircon Job for hirsel-board")?;

        let (stdin_host, stdin_child) = zx::Socket::create_stream();
        let (stdout_host, stdout_child) = zx::Socket::create_stream();

        let mut actions = vec![
            fdio::SpawnAction::set_name(c"hirsel-board"),
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
            c"/pkg/bin/hirsel-board",
            &[c"hirsel-board"],
            None,
            &mut actions,
        )
        .map_err(|(s, m)| anyhow!("spawn /pkg/bin/hirsel-board failed: {s}: {m}"))?;

        let pid = process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        let stdin = fasync::Socket::from_socket(stdin_host);
        let mut stdout = BufReader::new(fasync::Socket::from_socket(stdout_host));

        let mut line = String::new();
        let n = stdout.read_line(&mut line).await.context("read initial hirsel-board snapshot")?;
        if n == 0 {
            return Err(anyhow!("hirsel-board exited before initial snapshot"));
        }
        let latest: BoardSnapshot = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-board JSON: {}", line.trim()))?;
        info!("Spawned pure-Hirsel hirsel-board (pid koid={pid}, cards={})", latest.cards.len());

        let mut client = Self { _job: job, _process: process, stdin, stdout, latest };
        client.hydrate_from_disk().await;
        Ok(client)
    }

    async fn hydrate_from_disk(&mut self) {
        let Ok(raw) = std::fs::read_to_string(BOARD_SNAPSHOT_PATH) else {
            return;
        };
        let Ok(saved) = serde_json::from_str::<BoardSnapshot>(&raw) else {
            return;
        };
        for c in &saved.cards {
            let cmd = format!(
                "PUT\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                clean_field(&c.id),
                clean_field(&c.kind),
                clean_field(&c.tone),
                c.x,
                c.y,
                c.w,
                c.h,
                clean_field(&c.title),
                clean_field(&c.body)
            );
            let _ = self.send_raw(&cmd, false).await;
        }
        for e in &saved.edges {
            let cmd = format!(
                "EDGE\t{}\t{}\t{}",
                clean_field(&e.from),
                clean_field(&e.to),
                clean_field(&e.label)
            );
            let _ = self.send_raw(&cmd, false).await;
        }
        let cam_cmd = format!(
            "CAM\t{}\t{}\t{}",
            saved.camera.cx, saved.camera.cy, saved.camera.zoom_pct
        );
        let _ = self.send_raw(&cam_cmd, false).await;
        info!(
            "Hydrated hirsel-board from {BOARD_SNAPSHOT_PATH}: cards={}, edges={}, zoom={}%",
            self.latest.cards.len(),
            self.latest.edges.len(),
            self.latest.camera.zoom_pct
        );
    }

    fn persist_to_disk(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.latest) {
            if let Ok(mut f) = std::fs::File::create(BOARD_SNAPSHOT_PATH) {
                let _ = f.write_all(json.as_bytes()).and_then(|_| f.sync_all());
            }
        }
    }

    async fn send_raw(&mut self, cmd: &str, persist: bool) -> Result<BoardSnapshot> {
        let mut payload = cmd.to_string();
        payload.push('\n');
        self.stdin.write_all(payload.as_bytes()).await.context("write hirsel-board")?;
        self.stdin.flush().await.ok();

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await.context("read hirsel-board")?;
        if n == 0 {
            return Err(anyhow!("hirsel-board closed stdout"));
        }
        let snap: BoardSnapshot = serde_json::from_str(line.trim())
            .with_context(|| format!("parse hirsel-board JSON: {}", line.trim()))?;
        self.latest = snap.clone();
        if persist {
            self.persist_to_disk();
        }
        Ok(snap)
    }

    pub async fn put_card(
        &mut self,
        id: &str,
        kind: &str,
        tone: &str,
        x: u64,
        y: u64,
        w: u64,
        h: u64,
        title: &str,
        body: &str,
    ) -> Result<BoardSnapshot> {
        let cmd = format!(
            "PUT\t{}\t{}\t{}\t{x}\t{y}\t{w}\t{h}\t{}\t{}",
            clean_field(id),
            clean_field(kind),
            clean_field(tone),
            clean_field(title),
            clean_field(body)
        );
        self.send_raw(&cmd, true).await
    }

    pub async fn move_card(&mut self, id: &str, x: u64, y: u64) -> Result<BoardSnapshot> {
        self.send_raw(&format!("MOVE\t{}\t{x}\t{y}", clean_field(id)), true).await
    }

    pub async fn delete_card(&mut self, id: &str) -> Result<BoardSnapshot> {
        self.send_raw(&format!("DEL\t{}", clean_field(id)), true).await
    }

    pub async fn link_cards(&mut self, from: &str, to: &str, label: &str) -> Result<BoardSnapshot> {
        self.send_raw(
            &format!("EDGE\t{}\t{}\t{}", clean_field(from), clean_field(to), clean_field(label)),
            true,
        )
        .await
    }

    pub async fn set_camera(&mut self, cx: u64, cy: u64, zoom_pct: u64) -> Result<BoardSnapshot> {
        self.send_raw(&format!("CAM\t{cx}\t{cy}\t{zoom_pct}"), true).await
    }
}
