// Unified Live OS Visual & Application State Tree (`script.rs`).
//
// Core Figments Principle:
//   "Everything the user sees, the agent can also see and introspect —
//    including live application state and running Hirsel source —
//    except during F6 SAS / Ceremony lockdown."
//
// Provides:
//   1. Single source of truth for Top Bar pill geometry (`top_bar_left_pills`) shared by
//      painter.rs, MouseClick hit-testing, and `GET /_hirsel/tree`.
//   2. Unified Live Visual & State Tree (`build_os_tree`) with automatic SAS redaction.
//   3. Hirsel `os@1` stdin snapshot encoder (`encode_hirsel_stdin`) that feeds both
//      typed key=value headers AND the full JSON tree into `bin/hirsel-eval`.

use crate::activity::ActivitySnapshot;
use crate::apps::AppRegistry;
use crate::board_client::BoardSnapshot;
use crate::shell::ShellState;
use crate::wm_client::{RectJson, ScenePlan};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TopBarPillSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub role: &'static str,
    pub x: u64,
    pub y: u64,
    pub w: u64,
    pub h: u64,
    pub action: &'static str,
}

/// Canonical Top Bar pill coordinates shared by `painter.rs`, `main.rs` MouseClick, and `GET /_hirsel/tree`.
pub fn top_bar_left_pills() -> [TopBarPillSpec; 4] {
    [
        TopBarPillSpec {
            id: "pill-figments",
            label: "1 Figments",
            role: "tab",
            x: 92,
            y: 8,
            w: 98,
            h: 28,
            action: "FOCUS figments",
        },
        TopBarPillSpec {
            id: "pill-shell",
            label: "2 Shell",
            role: "toggle",
            x: 194,
            y: 8,
            w: 74,
            h: 28,
            action: "TOGGLE_DEV",
        },
        TopBarPillSpec {
            id: "pill-activity",
            label: "3 Activity",
            role: "toggle",
            x: 272,
            y: 8,
            w: 90,
            h: 28,
            action: "TOGGLE_ACTIVITY",
        },
        TopBarPillSpec {
            id: "pill-web",
            label: "4 Web",
            role: "toggle",
            x: 366,
            y: 8,
            w: 72,
            h: 28,
            action: "TOGGLE_WEB",
        },
    ]
}

pub const TOP_BAR_LEFT_ISLAND_W: usize = 434;

pub struct TreeInputs<'a> {
    pub plan: &'a ScenePlan,
    pub phys_w: u32,
    pub phys_h: u32,
    pub ui_scale_pct: u32,
    pub active_theme: &'a str,
    pub activity: &'a ActivitySnapshot,
    pub shell: &'a ShellState,
    pub board: &'a BoardSnapshot,
    pub app_registry: &'a AppRegistry,
    pub ink_count: usize,
    pub audit_log: &'a [String],
    pub figments_ui: &'a Value,
    pub vm_ready: bool,
    pub untrusted_origin: &'a str,
    pub powerbox_toast: &'a str,
    pub settings_json: Value,
}

fn phys_rect(r: &RectJson, scale: f64) -> Value {
    json!({
        "x": (r.x as f64 * scale).round() as u64,
        "y": (r.y as f64 * scale).round() as u64,
        "w": (r.w as f64 * scale).round() as u64,
        "h": (r.h as f64 * scale).round() as u64,
    })
}

/// Build the unified live OS visual & application state tree.
/// Automatically redacts sensitive user/app state when `plan.capture_allowed == false` (SAS / Ceremony).
pub fn build_os_tree(inp: &TreeInputs<'_>) -> Value {
    let plan = inp.plan;
    let scale = (inp.ui_scale_pct as f64) / 100.0;
    let locked = !plan.capture_allowed;
    let pills = top_bar_left_pills();

    let mut visible_elements = Vec::<Value>::new();
    let mut top_bar_pills_json = Vec::<Value>::new();

    for p in &pills {
        let active = match p.id {
            "pill-figments" => plan.focused == "figments" && !locked,
            "pill-shell" => plan.layout.vm_dev.is_some(),
            "pill-activity" => plan.layout.activity.is_some(),
            "pill-web" => plan.layout.web.is_some(),
            _ => false,
        };
        let bounds = json!({ "x": p.x, "y": p.y, "w": p.w, "h": p.h });
        let entry = json!({
            "id": p.id,
            "app": "top_bar",
            "label": p.label,
            "role": p.role,
            "active": active,
            "bounds": bounds,
            "action": p.action,
        });
        top_bar_pills_json.push(entry.clone());
        visible_elements.push(entry);
    }

    let egress_x = plan.w.saturating_sub(180);
    let sys_x = plan.w.saturating_sub(62);
    let egress_el = json!({
        "id": "pill-egress",
        "app": "top_bar",
        "label": if plan.agent_conn { "egress" } else { "revoked" },
        "role": "toggle",
        "active": plan.agent_conn,
        "bounds": { "x": egress_x, "y": 8, "w": 116, "h": 28 },
        "action": if plan.agent_conn { "REVOKE" } else { "RESTORE" },
    });
    let sys_el = json!({
        "id": "pill-settings",
        "app": "top_bar",
        "label": if plan.capture_allowed { "sys" } else { "locked" },
        "role": "button",
        "active": false,
        "bounds": { "x": sys_x, "y": 8, "w": 52, "h": 28 },
        "action": "TOGGLE_SETTINGS",
    });
    top_bar_pills_json.push(egress_el.clone());
    top_bar_pills_json.push(sys_el.clone());
    visible_elements.push(egress_el);
    visible_elements.push(sys_el);

    let mut windows = Vec::<Value>::new();

    // 1. Figments Spatial Canvas window
    let fig = &plan.layout.figments;
    windows.push(json!({
        "id": "figments",
        "app": "figments",
        "domain": "figments",
        "title": "Figments Spatial Workstation",
        "role": "workspace",
        "visible": true,
        "focused": plan.focused == "figments",
        "bounds": fig,
        "physical_bounds": phys_rect(fig, scale),
    }));

    if !locked {
        if let Some(els) = inp.figments_ui["elements"].as_array() {
            for el in els {
                let ex = el["x"].as_f64().unwrap_or(0.0).round() as i64 + fig.x as i64;
                let ey = el["y"].as_f64().unwrap_or(0.0).round() as i64 + fig.y as i64;
                let ew = el["w"].as_f64().unwrap_or(0.0).round().max(0.0) as u64;
                let eh = el["h"].as_f64().unwrap_or(0.0).round().max(0.0) as u64;
                visible_elements.push(json!({
                    "id": el["id"],
                    "app": "figments",
                    "label": el["label"],
                    "role": el["role"],
                    "bounds": { "x": ex.max(0), "y": ey.max(0), "w": ew, "h": eh },
                    "state": el["state"],
                }));
            }
        }
    }

    // 2. System Shell (`hirsel-sh` / `vm_dev`) window
    if let Some(ref r) = plan.layout.vm_dev {
        windows.push(json!({
            "id": "shell",
            "app": "shell",
            "domain": "vm_dev",
            "title": if inp.vm_ready { "System Shell (hirsel-sh + vm-dev)" } else { "System Shell (hirsel-sh)" },
            "role": "terminal",
            "visible": true,
            "focused": plan.focused == "vm_dev",
            "bounds": r,
            "physical_bounds": phys_rect(r, scale),
        }));
        visible_elements.push(json!({
            "id": "shell-prompt",
            "app": "shell",
            "label": "Shell Prompt",
            "role": "textbox",
            "focused": plan.focused == "vm_dev",
            "bounds": {
                "x": r.x + 12,
                "y": r.y + r.h.saturating_sub(42),
                "w": r.w.saturating_sub(24),
                "h": 28
            },
            "value": if locked { "[REDACTED]" } else { inp.shell.input.as_str() },
        }));
    }

    // 3. Activity Manager window
    if let Some(ref r) = plan.layout.activity {
        windows.push(json!({
            "id": "activity",
            "app": "activity",
            "domain": "activity",
            "title": "Activity Manager",
            "role": "system_monitor",
            "visible": true,
            "focused": plan.focused == "activity",
            "bounds": r,
            "physical_bounds": phys_rect(r, scale),
        }));
        let table_y = r.y + 132;
        let row_h = 22u64;
        for (idx, p) in inp.activity.processes.iter().take(12).enumerate() {
            let ry = table_y + (idx as u64) * row_h;
            if ry + row_h > r.y + r.h.saturating_sub(36) {
                break;
            }
            visible_elements.push(json!({
                "id": format!("activity-row-{}", p.name),
                "app": "activity",
                "label": p.name,
                "role": "row",
                "selected": idx == inp.activity.selected_idx,
                "bounds": { "x": r.x + 10, "y": ry, "w": r.w.saturating_sub(20), "h": row_h },
                "metrics": { "koid": p.koid, "rss_mb": p.rss_mb, "cpu_ms": p.cpu_ms, "status": p.status },
            }));
        }
    }

    // 4. Isolated Web #2 window
    if let Some(ref r) = plan.layout.web {
        windows.push(json!({
            "id": "web",
            "app": "web",
            "domain": "web_untrusted",
            "title": format!("Isolated Web #2 ({})", inp.untrusted_origin),
            "role": "browser",
            "visible": true,
            "focused": plan.focused == "web_untrusted",
            "bounds": r,
            "physical_bounds": phys_rect(r, scale),
        }));
    }

    // 5. Protected Ceremony / SAS Lockdown Modal
    if let Some(ref cr) = plan.layout.ceremony {
        windows.push(json!({
            "id": "ceremony",
            "app": "trusted_chrome",
            "domain": "trusted_chrome",
            "title": if plan.outbox.status == "staged" { "Approval Ceremony" } else { "Secure Vault (SAS)" },
            "role": "dialog",
            "visible": true,
            "focused": true,
            "bounds": cr,
            "physical_bounds": phys_rect(cr, scale),
        }));
        let by = cr.y + cr.h.saturating_sub(60);
        visible_elements.push(json!({
            "id": "ceremony-primary",
            "app": "trusted_chrome",
            "label": if plan.outbox.status == "staged" { "Approve Hash" } else { "Seal Key" },
            "role": "button",
            "bounds": { "x": cr.x + 18, "y": by, "w": 244, "h": 40 },
        }));
        visible_elements.push(json!({
            "id": "ceremony-secondary",
            "app": "trusted_chrome",
            "label": if plan.outbox.status == "staged" { "Deny Effect" } else { "Close SAS" },
            "role": "button",
            "bounds": { "x": cr.x + 276, "y": by, "w": 266, "h": 40 },
        }));
    }

    let apps = if locked {
        json!({
            "redacted": true,
            "reason": "capture_locked_during_sas_or_ceremony",
            "trusted_chrome": {
                "sas": plan.sas,
                "outbox_status": plan.outbox.status,
                "outbox_target": plan.outbox.target,
                "outbox_summary": plan.outbox.summary,
            }
        })
    } else {
        json!({
            "figments": {
                "visible": true,
                "focused": plan.focused == "figments",
                "rev": inp.board.rev,
                "camera": inp.board.camera,
                "cards": inp.board.cards,
                "edges": inp.board.edges,
                "ink_strokes": inp.ink_count,
                "ui": inp.figments_ui,
            },
            "shell": {
                "visible": plan.layout.vm_dev.is_some(),
                "focused": plan.focused == "vm_dev",
                "vm_ready": inp.vm_ready,
                "input": inp.shell.input,
                "last_command": inp.shell.last_command,
                "last_ok": inp.shell.last_ok,
                "last_value": inp.shell.last_value,
                "defs": inp.shell.defs,
                "history_len": inp.shell.history.len(),
                "recent_output": inp.shell.output_lines.iter().rev().take(20).rev().cloned().collect::<Vec<_>>(),
            },
            "activity": {
                "visible": plan.layout.activity.is_some(),
                "focused": plan.focused == "activity",
                "sort_by": inp.activity.sort_by,
                "selected_idx": inp.activity.selected_idx,
                "uptime_sec": inp.activity.uptime_sec,
                "memory": inp.activity.memory,
                "num_cpus": inp.activity.num_cpus,
                "avg_cpu_pct": inp.activity.avg_cpu_pct,
                "cpus": inp.activity.cpus,
                "processes": inp.activity.processes,
                "live_view": inp.app_registry.activity_view,
                "storage_total_bytes": inp.activity.storage_total_bytes,
            },
            "web": {
                "visible": plan.layout.web.is_some(),
                "focused": plan.focused == "web_untrusted",
                "origin": inp.untrusted_origin,
            },
            "settings": inp.settings_json,
            "live_apps": inp.app_registry.summary_list(),
            "system": {
                "room": plan.room,
                "theme": inp.active_theme,
                "powerbox_toast": inp.powerbox_toast,
                "powerbox_transfers": inp.plan.powerbox_transfers,
                "bank_spent_micros": plan.bank_spent_micros,
                "bank_limit_micros": plan.bank_limit_micros,
                "approved": plan.approved,
                "denied": plan.denied,
                "audit_count": inp.audit_log.len(),
                "recent_audit": inp.audit_log.iter().rev().take(10).rev().cloned().collect::<Vec<_>>(),
            }
        })
    };

    json!({
        "schema": "hirsel.os.tree/v1",
        "display": {
            "phys_w": inp.phys_w,
            "phys_h": inp.phys_h,
            "logical_w": plan.w,
            "logical_h": plan.h,
            "ui_scale_pct": inp.ui_scale_pct,
            "room": plan.room,
            "theme": inp.active_theme,
        },
        "security": {
            "focused": plan.focused,
            "sas": plan.sas,
            "capture_allowed": plan.capture_allowed,
            "agent_conn": plan.agent_conn,
            "ceremony_active": plan.outbox.status == "staged",
            "redacted": locked,
        },
        "top_bar": {
            "bounds": { "x": 0, "y": 0, "w": plan.w, "h": 44 },
            "pills": top_bar_pills_json,
        },
        "windows": windows,
        "visible_elements": visible_elements,
        "apps": apps,
    })
}

/// Encode the live OS tree into the `os@1` stdin format consumed by Hirsel scripts running in `bin/hirsel-eval`:
/// fast `key=value\n` typed header lines followed by `---JSON---\n<tree json>`.
pub fn encode_hirsel_stdin(inp: &TreeInputs<'_>, tree: &Value) -> String {
    let plan = inp.plan;
    let locked = !plan.capture_allowed;
    let win_count = tree["windows"].as_array().map(|a| a.len()).unwrap_or(0);
    let el_count = tree["visible_elements"].as_array().map(|a| a.len()).unwrap_or(0);
    let top_proc = inp.activity.processes.first();
    let top_proc_name = top_proc.map(|p| p.name.as_str()).unwrap_or("none");
    let top_proc_rss = top_proc.map(|p| p.rss_mb.round() as u64).unwrap_or(0);

    let mut out = String::with_capacity(4096);
    out.push_str(&format!("focused={}\n", plan.focused));
    out.push_str(&format!("theme={}\n", inp.active_theme));
    out.push_str(&format!("room={}\n", plan.room));
    out.push_str(&format!("arch={}\n", inp.activity.arch));
    out.push_str(&format!("ui_scale_pct={}\n", inp.ui_scale_pct));
    out.push_str(&format!("logical_w={}\n", plan.w));
    out.push_str(&format!("logical_h={}\n", plan.h));
    out.push_str(&format!("uptime_sec={}\n", inp.activity.uptime_sec));
    out.push_str(&format!("capture_allowed={}\n", plan.capture_allowed));
    out.push_str(&format!("sas_active={}\n", plan.sas));
    out.push_str(&format!("window_count={win_count}\n"));
    out.push_str(&format!("element_count={el_count}\n"));
    if !locked {
        out.push_str(&format!("mem_used_mb={}\n", inp.activity.memory.used_mb.round() as u64));
        out.push_str(&format!("mem_total_mb={}\n", inp.activity.memory.total_mb.round() as u64));
        out.push_str(&format!("mem_used_pct={}\n", inp.activity.memory.used_pct.round() as u64));
        out.push_str(&format!("num_cpus={}\n", inp.activity.num_cpus));
        out.push_str(&format!("avg_cpu_pct={}\n", inp.activity.avg_cpu_pct.round() as u64));
        out.push_str(&format!("proc_count={}\n", inp.activity.processes.len()));
        out.push_str(&format!("top_proc_name={top_proc_name}\n"));
        out.push_str(&format!("top_proc_rss_mb={top_proc_rss}\n"));
        out.push_str(&format!("card_count={}\n", inp.board.cards.len()));
        out.push_str(&format!("shell_defs_count={}\n", inp.shell.defs.len()));
    }
    out.push_str("---JSON---\n");
    out.push_str(&tree.to_string());
    out
}
