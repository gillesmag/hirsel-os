// Unified Live Application Internals Registry (`apps.rs`).
//
// Exposes live Hirsel source code, revision history, transactional hot-swap, and
// 1-click rollback on Fxfs (`/data/apps/<name>/`) for running OS apps & Figments applets,
// plus read-only source inspection for security-verified core Places (`hirsel-wm`,
// `hirsel-board`, `hirsel-agent`) and the Hirsel `os` library itself.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

const APPS_ROOT: &str = "/data/apps";

pub const DEFAULT_ACTIVITY_VIEW_HIRSEL: &str = r#"// Live Activity Manager View & Alert Policy (edit inline in Figments!)
// Runs against real Zircon kernel/process telemetry and hot-swaps live.
let mem_pct = os.mem_used_pct(ctx)
let used = os.mem_used_mb(ctx)
let total = os.mem_total_mb(ctx)
let procs = os.proc_count(ctx)
let top_name = os.top_proc_name(ctx)
let top_rss = os.top_proc_rss_mb(ctx)

if mem_pct >= 75u64 {
    os.emit_alert(mut ctx, "warn", "memory", "High Zircon RAM pressure")
}
if top_rss >= 64u64 {
    os.emit_alert(mut ctx, "info", top_name, "Top RSS consumer >= 64 MiB")
}

let mut banner = string.from("Zircon live · ")
string.push_u64(mut banner, procs)
string.push(mut banner, " tasks · ")
string.push_u64(mut banner, used)
string.push(mut banner, "/")
string.push_u64(mut banner, total)
string.push(mut banner, " MiB · top: ")
string.push(mut banner, top_name)
os.emit_banner(mut ctx, take banner)
"#;

pub const DEFAULT_SHELL_INIT_HIRSEL: &str = r#"// Live System Shell Profile (`hirsel-sh` — edit inline in Figments!)
// Defines persistent structured pipeline commands and Hirsel shortcuts.
os.shell_def(mut ctx, "top", "ps | sort rss_mb desc | first 6")
os.shell_def(mut ctx, "heavy", "ps | where rss_mb > 0 | sort rss_mb desc")
os.print(mut ctx, "hirsel-sh profile applied")
"#;

pub const DEFAULT_APPLET_MONITOR_HIRSEL: &str = r#"// Live Figments Spatial Applet — System Pulse (edit inline in Figments!)
// Renders live computed telemetry straight onto a spatial card on the canvas.
let mut body = string.from("RAM ")
string.push_u64(mut body, os.mem_used_mb(ctx))
string.push(mut body, "/")
string.push_u64(mut body, os.mem_total_mb(ctx))
string.push(mut body, "M (")
string.push_u64(mut body, os.mem_used_pct(ctx))
string.push(mut body, "%) · ")
string.push_u64(mut body, os.proc_count(ctx))
string.push(mut body, " tasks · ")
string.push_u64(mut body, os.window_count(ctx))
string.push(mut body, " panes · scale ")
string.push_u64(mut body, os.ui_scale(ctx))
string.push(mut body, "%")
os.emit_card_view(mut ctx, "Live Pulse (Hirsel)", take body, "sap")
"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityAlert {
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LiveActivityView {
    pub banner: String,
    pub alerts: Vec<ActivityAlert>,
    pub last_compile_ms: u64,
    pub last_run_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppRevisionMeta {
    pub rev: u64,
    pub bytes: usize,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveAppRecord {
    pub name: String,
    pub title: String,
    pub kind: String,
    pub editable: bool,
    pub rev: u64,
    pub source: String,
    pub revisions: Vec<AppRevisionMeta>,
    pub last_logs: Vec<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedAppMeta {
    title: String,
    kind: String,
    rev: u64,
    revisions: Vec<AppRevisionMeta>,
}

pub struct AppRegistry {
    pub apps: BTreeMap<String, LiveAppRecord>,
    pub activity_view: LiveActivityView,
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

fn app_dir(name: &str) -> PathBuf {
    PathBuf::from(APPS_ROOT).join(name)
}

fn write_sync(path: &PathBuf, content: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(path) {
        let _ = f.write_all(content).and_then(|_| f.sync_all());
    }
}

impl AppRegistry {
    pub fn load() -> Self {
        let mut apps = BTreeMap::new();

        // 1. Live-editable Hirsel apps (hydrated from Fxfs `/data/apps/<name>/` or seeded with defaults)
        let defaults = [
            ("activity", "Activity Manager View", "live_view", DEFAULT_ACTIVITY_VIEW_HIRSEL),
            ("shell", "System Shell Profile", "live_shell", DEFAULT_SHELL_INIT_HIRSEL),
            ("applet_monitor", "Live Pulse Applet", "live_applet", DEFAULT_APPLET_MONITOR_HIRSEL),
        ];
        for (name, title, kind, default_src) in defaults {
            let rec = Self::load_or_seed_editable(name, title, kind, default_src);
            apps.insert(name.to_string(), rec);
        }

        // Also discover any extra user-created live apps persisted under `/data/apps/`
        if let Ok(rd) = std::fs::read_dir(APPS_ROOT) {
            for ent in rd.flatten() {
                if let Ok(ft) = ent.file_type() {
                    if ft.is_dir() {
                        let name = ent.file_name().to_string_lossy().to_string();
                        if let std::collections::btree_map::Entry::Vacant(e) = apps.entry(name.clone()) {
                            let cur_path = app_dir(&name).join("current.hirsel");
                            if let Ok(src) = std::fs::read_to_string(&cur_path) {
                                let rec = Self::load_or_seed_editable(&name, &name, "live_applet", &src);
                                e.insert(rec);
                            }
                        }
                    }
                }
            }
        }

        // 2. Read-only bundled system Place & OS library sources (for live inspection in Figments!)
        let readonly_sources = [
            ("os", "Hirsel OS Library (os@1)", "os_library", "/pkg/data/hirsel/os/os.hirsel"),
            ("hirsel-wm", "Window Manager (Verified Place)", "verified_place", "/pkg/data/hirsel/places/hirsel-wm/wm.hirsel"),
            ("hirsel-board", "Spatial Board (Verified Place)", "verified_place", "/pkg/data/hirsel/places/hirsel-board/board.hirsel"),
            ("hirsel-agent", "Autonomous Agent (Verified Place)", "verified_place", "/pkg/data/hirsel/places/hirsel-agent/planner.hirsel"),
        ];
        for (name, title, kind, pkg_path) in readonly_sources {
            let source = std::fs::read_to_string(pkg_path)
                .unwrap_or_else(|_| format!("// Source not bundled at {pkg_path}\n"));
            apps.insert(
                name.to_string(),
                LiveAppRecord {
                    name: name.to_string(),
                    title: title.to_string(),
                    kind: kind.to_string(),
                    editable: false,
                    rev: 1,
                    revisions: vec![AppRevisionMeta {
                        rev: 1,
                        bytes: source.len(),
                        note: "bundled in /pkg".to_string(),
                    }],
                    source,
                    last_logs: Vec::new(),
                    last_error: None,
                },
            );
        }

        Self {
            apps,
            activity_view: LiveActivityView::default(),
        }
    }

    fn load_or_seed_editable(name: &str, title: &str, kind: &str, default_src: &str) -> LiveAppRecord {
        let dir = app_dir(name);
        let cur_path = dir.join("current.hirsel");
        let meta_path = dir.join("meta.json");

        if let (Ok(source), Ok(meta_raw)) = (
            std::fs::read_to_string(&cur_path),
            std::fs::read_to_string(&meta_path),
        ) {
            if let Ok(meta) = serde_json::from_str::<PersistedAppMeta>(&meta_raw) {
                return LiveAppRecord {
                    name: name.to_string(),
                    title: meta.title,
                    kind: meta.kind,
                    editable: true,
                    rev: meta.rev,
                    source,
                    revisions: meta.revisions,
                    last_logs: Vec::new(),
                    last_error: None,
                };
            }
        }

        let rev1 = AppRevisionMeta {
            rev: 1,
            bytes: default_src.len(),
            note: "initial".to_string(),
        };
        let meta = PersistedAppMeta {
            title: title.to_string(),
            kind: kind.to_string(),
            rev: 1,
            revisions: vec![rev1.clone()],
        };
        write_sync(&cur_path, default_src.as_bytes());
        write_sync(&dir.join("rev_1.hirsel"), default_src.as_bytes());
        if let Ok(mj) = serde_json::to_string_pretty(&meta) {
            write_sync(&meta_path, mj.as_bytes());
        }

        LiveAppRecord {
            name: name.to_string(),
            title: title.to_string(),
            kind: kind.to_string(),
            editable: true,
            rev: 1,
            source: default_src.to_string(),
            revisions: vec![rev1],
            last_logs: Vec::new(),
            last_error: None,
        }
    }

    pub fn summary_list(&self) -> Vec<Value> {
        self.apps
            .values()
            .map(|a| {
                json!({
                    "name": a.name,
                    "title": a.title,
                    "kind": a.kind,
                    "editable": a.editable,
                    "rev": a.rev,
                    "bytes": a.source.len(),
                    "revisions": a.revisions.len(),
                    "last_error": a.last_error,
                })
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&LiveAppRecord> {
        self.apps.get(name)
    }

    pub fn is_editable(&self, name: &str) -> bool {
        self.apps.get(name).map(|a| a.editable).unwrap_or(true)
    }

    /// Commit a pre-checked Hirsel source update to Fxfs + revision history.
    pub fn commit_update(&mut self, raw_name: &str, new_source: String, note: &str) -> Result<LiveAppRecord, String> {
        let name = sanitize_name(raw_name);
        if name.is_empty() {
            return Err("invalid app name".to_string());
        }
        if let Some(existing) = self.apps.get(&name) {
            if !existing.editable {
                return Err(format!(
                    "`{name}` is a protected verified system module (read-only); inspect its source or create a live applet"
                ));
            }
        }
        let dir = app_dir(&name);
        let rec = self.apps.entry(name.clone()).or_insert_with(|| LiveAppRecord {
            name: name.clone(),
            title: name.clone(),
            kind: "live_applet".to_string(),
            editable: true,
            rev: 0,
            source: String::new(),
            revisions: Vec::new(),
            last_logs: Vec::new(),
            last_error: None,
        });

        let next_rev = rec.rev + 1;
        rec.rev = next_rev;
        rec.source = new_source.clone();
        rec.last_error = None;
        rec.revisions.push(AppRevisionMeta {
            rev: next_rev,
            bytes: new_source.len(),
            note: note.to_string(),
        });
        if rec.revisions.len() > 25 {
            rec.revisions.remove(0);
        }

        write_sync(&dir.join("current.hirsel"), new_source.as_bytes());
        write_sync(&dir.join(format!("rev_{next_rev}.hirsel")), new_source.as_bytes());
        let meta = PersistedAppMeta {
            title: rec.title.clone(),
            kind: rec.kind.clone(),
            rev: rec.rev,
            revisions: rec.revisions.clone(),
        };
        if let Ok(mj) = serde_json::to_string_pretty(&meta) {
            write_sync(&dir.join("meta.json"), mj.as_bytes());
        }
        Ok(rec.clone())
    }

    /// Roll back an editable app to its previous persisted revision.
    pub fn rollback(&mut self, raw_name: &str) -> Result<LiveAppRecord, String> {
        let name = sanitize_name(raw_name);
        let Some(rec) = self.apps.get(&name) else {
            return Err(format!("app `{name}` not found"));
        };
        if !rec.editable {
            return Err(format!("`{name}` is read-only"));
        }
        if rec.revisions.len() < 2 {
            return Err(format!("`{name}` has no previous revision to roll back to"));
        }
        let prev_rev = rec.revisions[rec.revisions.len() - 2].rev;
        let prev_path = app_dir(&name).join(format!("rev_{prev_rev}.hirsel"));
        let prev_source = std::fs::read_to_string(&prev_path)
            .map_err(|e| format!("read revision {prev_rev} failed: {e}"))?;
        self.commit_update(&name, prev_source, &format!("rollback to rev {prev_rev}"))
    }
}
