// Structured Hirsel System Shell (`hirsel-sh`).
//
// Nushell-inspired structured command language with:
//   - Typed JSON tables & records + real pipeline stages:
//       `| where <field> <op> <val>`
//       `| sort <field> [asc|desc]`
//       `| select <f1,f2,...>`
//       `| first <n>` / `| last <n>`
//       `| count` / `| get <field>` / `| json`
//   - User-defined commands & aliases persisted on Fxfs (`/data/shell.json`):
//       `def <name> = <pipeline>`
//       `undef <name>`
//       `defs`
//   - Deep OS builtins (`ps`, `mem`, `cpu`, `fs`, `net`, `rooms`, `scale`,
//     `board`, `tree`, `apps`, `tell ...`, `js ...`, `agent ...`, `egress`, `audit`, `vm ...`)

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Write as _;

const SHELL_CONFIG_PATH: &str = "/data/shell.json";
const MAX_OUTPUT_LINES: usize = 160;
const MAX_HISTORY: usize = 100;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedShellConfig {
    defs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellExecResponse {
    pub ok: bool,
    pub command: String,
    pub expanded: String,
    pub value: Value,
    pub table: String,
    pub defs_count: usize,
}

#[derive(Clone, Debug)]
pub enum ShellBuiltin {
    Help,
    Clear,
    Defs,
    Def { name: String, body: String },
    Undef { name: String },
    History,
    Ps,
    Mem,
    Cpu,
    FsLs { path: String },
    FsCat { path: String },
    FsWrite { path: String, content: String },
    FsRm { path: String },
    Net,
    Rooms { set: Option<String> },
    Scale { pct: Option<u32> },
    Board,
    Tree,
    Apps,
    Audit,
    Egress,
    System,
    Hirsel { code: String },
    AppRun { name: String },
    AppSource { name: String },
    AppRollback { name: String },
    Js { code: String },
    Agent { goal: String },
    Vm { cmd: String },
    Echo { text: String },
    Unknown { name: String },
}

pub struct ParsedShellLine {
    pub raw: String,
    pub expanded: String,
    pub builtin: ShellBuiltin,
    pub pipeline: Vec<String>,
}

pub struct ShellState {
    pub defs: BTreeMap<String, String>,
    pub history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub input: String,
    pub output_lines: Vec<String>,
    pub last_command: String,
    pub last_value: Value,
    pub last_table: String,
    pub last_ok: bool,
}

impl ShellState {
    pub fn load() -> Self {
        let defs = match std::fs::read_to_string(SHELL_CONFIG_PATH) {
            Ok(raw) => serde_json::from_str::<PersistedShellConfig>(&raw)
                .map(|c| c.defs)
                .unwrap_or_default(),
            Err(_) => {
                let mut d = BTreeMap::new();
                d.insert("top".to_string(), "ps | sort rss_mb desc | first 6".to_string());
                d.insert("heavy".to_string(), "ps | where rss_mb > 0 | sort rss_mb desc".to_string());
                d
            }
        };
        let mut st = Self {
            defs,
            history: Vec::new(),
            history_cursor: None,
            input: String::new(),
            output_lines: Vec::new(),
            last_command: String::new(),
            last_value: json!(null),
            last_table: String::new(),
            last_ok: true,
        };
        st.push_output("hirsel-sh 0.1 · structured OS shell (type `help`, `ps`, `mem`, `tree`, `def`)");
        st
    }

    pub fn persist(&self) {
        let cfg = PersistedShellConfig { defs: self.defs.clone() };
        if let Ok(json) = serde_json::to_string_pretty(&cfg) {
            if let Ok(mut f) = std::fs::File::create(SHELL_CONFIG_PATH) {
                let _ = f.write_all(json.as_bytes()).and_then(|_| f.sync_all());
            }
        }
    }

    pub fn push_output(&mut self, text: &str) {
        for line in text.lines() {
            self.output_lines.push(line.to_string());
        }
        if self.output_lines.len() > MAX_OUTPUT_LINES {
            let drain = self.output_lines.len() - MAX_OUTPUT_LINES;
            self.output_lines.drain(..drain);
        }
    }

    pub fn record_history(&mut self, cmd: &str) {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().map(|s| s.as_str()) != Some(trimmed) {
            self.history.push(trimmed.to_string());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.history_cursor = None;
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next);
        self.input = self.history[next].clone();
    }

    pub fn history_down(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_cursor {
            None => {}
            Some(i) if i + 1 < self.history.len() => {
                self.history_cursor = Some(i + 1);
                self.input = self.history[i + 1].clone();
            }
            Some(_) => {
                self.history_cursor = None;
                self.input.clear();
            }
        }
    }

    pub fn clear_output(&mut self) {
        self.output_lines.clear();
    }
}

pub fn split_pipeline(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in input.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' && !in_single {
            cur.push(ch);
            escape = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            cur.push(ch);
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            cur.push(ch);
            continue;
        }
        if ch == '|' && !in_single && !in_double {
            let trimmed = cur.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    let trimmed = cur.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts
}

pub fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let mut had_token = false;

    for ch in input.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            had_token = true;
            continue;
        }
        if ch == '\\' && !in_single {
            escape = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            had_token = true;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            had_token = true;
            continue;
        }
        if ch.is_whitespace() && !in_single && !in_double {
            if had_token || !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
                had_token = false;
            }
        } else {
            cur.push(ch);
            had_token = true;
        }
    }
    if had_token || !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

pub fn expand_defs(raw: &str, defs: &BTreeMap<String, String>) -> String {
    let mut current = raw.trim().to_string();
    for _ in 0..8 {
        let trimmed = current.trim();
        // Never expand inside `def`, `undef`, `hirsel`, `eval`, `js`, `vm`
        if trimmed.starts_with("def ")
            || trimmed.starts_with("undef ")
            || trimmed.starts_with("hirsel ")
            || trimmed.starts_with("eval ")
            || trimmed.starts_with("js ")
            || trimmed.starts_with("vm ")
        {
            break;
        }
        let stages = split_pipeline(trimmed);
        let Some(first_stage) = stages.first() else {
            break;
        };
        let toks = tokenize(first_stage);
        let Some(head) = toks.first() else {
            break;
        };
        let Some(body) = defs.get(head) else {
            break;
        };
        // Replace first token with definition body, preserving extra args and trailing pipeline stages.
        let rest_args = first_stage
            .strip_prefix(head.as_str())
            .map(|s| s.trim())
            .unwrap_or("");
        let mut rebuilt = if rest_args.is_empty() {
            body.clone()
        } else {
            format!("{body} {rest_args}")
        };
        if stages.len() > 1 {
            rebuilt.push_str(" | ");
            rebuilt.push_str(&stages[1..].join(" | "));
        }
        if rebuilt == current {
            break;
        }
        current = rebuilt;
    }
    current
}

pub fn parse_line(raw: &str, defs: &BTreeMap<String, String>) -> ParsedShellLine {
    let trimmed = raw.trim();

    // Handle `def <name> = <pipeline>` BEFORE pipeline splitting so `|` belongs to the definition!
    if let Some(rest) = trimmed.strip_prefix("def ") {
        if let Some((lhs, rhs)) = rest.split_once('=') {
            let name = lhs.trim().to_string();
            let body = rhs.trim().to_string();
            return ParsedShellLine {
                raw: trimmed.to_string(),
                expanded: trimmed.to_string(),
                builtin: ShellBuiltin::Def { name, body },
                pipeline: Vec::new(),
            };
        }
    }

    // Handle `hirsel ...`, `eval ...`, `js ...`, `vm ...` directly so pipes/operators inside them aren't stolen
    if let Some(code) = trimmed.strip_prefix("hirsel ").or_else(|| trimmed.strip_prefix("eval ")) {
        return ParsedShellLine {
            raw: trimmed.to_string(),
            expanded: trimmed.to_string(),
            builtin: ShellBuiltin::Hirsel { code: code.trim().to_string() },
            pipeline: Vec::new(),
        };
    }
    if let Some(code) = trimmed.strip_prefix("js ") {
        return ParsedShellLine {
            raw: trimmed.to_string(),
            expanded: trimmed.to_string(),
            builtin: ShellBuiltin::Js { code: code.trim().to_string() },
            pipeline: Vec::new(),
        };
    }
    if let Some(cmd) = trimmed.strip_prefix("vm ") {
        return ParsedShellLine {
            raw: trimmed.to_string(),
            expanded: trimmed.to_string(),
            builtin: ShellBuiltin::Vm { cmd: cmd.trim().to_string() },
            pipeline: Vec::new(),
        };
    }

    let expanded = expand_defs(trimmed, defs);
    let stages = split_pipeline(&expanded);
    let first = stages.first().cloned().unwrap_or_default();
    let pipeline = if stages.len() > 1 { stages[1..].to_vec() } else { Vec::new() };
    let toks = tokenize(&first);
    let cmd = toks.first().map(|s| s.as_str()).unwrap_or("");

    let builtin = match cmd {
        "" | "help" | "?" => ShellBuiltin::Help,
        "clear" | "cls" => ShellBuiltin::Clear,
        "defs" | "aliases" => ShellBuiltin::Defs,
        "undef" => ShellBuiltin::Undef {
            name: toks.get(1).cloned().unwrap_or_default(),
        },
        "history" => ShellBuiltin::History,
        "ps" | "procs" | "tasks" => ShellBuiltin::Ps,
        "mem" | "memory" | "free" => ShellBuiltin::Mem,
        "cpu" | "cpus" => ShellBuiltin::Cpu,
        "fs" => {
            let sub = toks.get(1).map(|s| s.as_str()).unwrap_or("ls");
            match sub {
                "cat" | "read" => ShellBuiltin::FsCat {
                    path: toks.get(2).cloned().unwrap_or_else(|| "/data".to_string()),
                },
                "write" => {
                    let path = toks.get(2).cloned().unwrap_or_default();
                    let content = if toks.len() > 3 {
                        toks[3..].join(" ")
                    } else {
                        String::new()
                    };
                    ShellBuiltin::FsWrite { path, content }
                }
                "rm" | "del" => ShellBuiltin::FsRm {
                    path: toks.get(2).cloned().unwrap_or_default(),
                },
                _ => ShellBuiltin::FsLs {
                    path: toks.get(2).cloned().unwrap_or_else(|| "/data".to_string()),
                },
            }
        }
        "ls" => ShellBuiltin::FsLs {
            path: toks.get(1).cloned().unwrap_or_else(|| "/data".to_string()),
        },
        "cat" => ShellBuiltin::FsCat {
            path: toks.get(1).cloned().unwrap_or_default(),
        },
        "net" | "network" => ShellBuiltin::Net,
        "rooms" | "room" | "theme" => ShellBuiltin::Rooms {
            set: toks.get(1).cloned(),
        },
        "scale" => ShellBuiltin::Scale {
            pct: toks.get(1).and_then(|s| s.trim_end_matches('%').parse::<u32>().ok()),
        },
        "board" | "cards" => ShellBuiltin::Board,
        "tree" => ShellBuiltin::Tree,
        "apps" | "windows" => ShellBuiltin::Apps,
        "run" => ShellBuiltin::AppRun {
            name: toks.get(1).cloned().unwrap_or_default(),
        },
        "source" | "src" => ShellBuiltin::AppSource {
            name: toks.get(1).cloned().unwrap_or_else(|| "activity".to_string()),
        },
        "rollback" => ShellBuiltin::AppRollback {
            name: toks.get(1).cloned().unwrap_or_default(),
        },
        "audit" => ShellBuiltin::Audit,
        "egress" => ShellBuiltin::Egress,
        "sys" | "system" | "uname" => ShellBuiltin::System,
        "agent" => ShellBuiltin::Agent {
            goal: if toks.len() > 1 {
                toks[1..].join(" ")
            } else {
                "Audit Hirsel-OS workstation".to_string()
            },
        },
        "echo" => ShellBuiltin::Echo {
            text: if toks.len() > 1 { toks[1..].join(" ") } else { String::new() },
        },
        other => ShellBuiltin::Unknown { name: other.to_string() },
    };

    ParsedShellLine {
        raw: trimmed.to_string(),
        expanded,
        builtin,
        pipeline,
    }
}

fn val_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn val_as_str(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn cmp_values(field_val: &Value, op: &str, rhs_str: &str) -> bool {
    let lhs_num = val_as_f64(field_val);
    let rhs_num = rhs_str.parse::<f64>().ok();
    match op {
        "==" | "=" => {
            if let (Some(a), Some(b)) = (lhs_num, rhs_num) {
                (a - b).abs() < 1e-9
            } else {
                val_as_str(field_val).eq_ignore_ascii_case(rhs_str)
            }
        }
        "!=" => {
            if let (Some(a), Some(b)) = (lhs_num, rhs_num) {
                (a - b).abs() >= 1e-9
            } else {
                !val_as_str(field_val).eq_ignore_ascii_case(rhs_str)
            }
        }
        ">" => match (lhs_num, rhs_num) {
            (Some(a), Some(b)) => a > b,
            _ => val_as_str(field_val).as_str() > rhs_str,
        },
        ">=" => match (lhs_num, rhs_num) {
            (Some(a), Some(b)) => a >= b,
            _ => val_as_str(field_val).as_str() >= rhs_str,
        },
        "<" => match (lhs_num, rhs_num) {
            (Some(a), Some(b)) => a < b,
            _ => val_as_str(field_val).as_str() < rhs_str,
        },
        "<=" => match (lhs_num, rhs_num) {
            (Some(a), Some(b)) => a <= b,
            _ => val_as_str(field_val).as_str() <= rhs_str,
        },
        "contains" | "~=" => val_as_str(field_val)
            .to_ascii_lowercase()
            .contains(&rhs_str.to_ascii_lowercase()),
        _ => false,
    }
}

pub fn apply_pipeline(mut val: Value, stages: &[String]) -> Result<(Value, bool), String> {
    let mut force_json = false;
    for stage in stages {
        let toks = tokenize(stage);
        let Some(op) = toks.first().map(|s| s.as_str()) else {
            continue;
        };
        match op {
            "where" => {
                if toks.len() < 4 {
                    return Err("usage: | where <field> <==|!=|>|>=|<|<=|contains> <value>".to_string());
                }
                let field = &toks[1];
                let cmp = toks[2].as_str();
                let rhs = toks[3..].join(" ");
                let items = match val {
                    Value::Array(arr) => arr,
                    other => vec![other],
                };
                let filtered: Vec<Value> = items
                    .into_iter()
                    .filter(|item| {
                        item.get(field)
                            .map(|fv| cmp_values(fv, cmp, &rhs))
                            .unwrap_or(false)
                    })
                    .collect();
                val = Value::Array(filtered);
            }
            "sort" | "sort-by" => {
                let Some(field) = toks.get(1) else {
                    return Err("usage: | sort <field> [asc|desc]".to_string());
                };
                let desc = toks
                    .get(2)
                    .map(|s| s.eq_ignore_ascii_case("desc") || s == "-r")
                    .unwrap_or(false);
                if let Value::Array(mut arr) = val {
                    arr.sort_by(|a, b| {
                        let fa = a.get(field);
                        let fb = b.get(field);
                        let ord = match (fa.and_then(val_as_f64), fb.and_then(val_as_f64)) {
                            (Some(na), Some(nb)) => {
                                na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
                            }
                            _ => {
                                let sa = fa.map(val_as_str).unwrap_or_default();
                                let sb = fb.map(val_as_str).unwrap_or_default();
                                sa.cmp(&sb)
                            }
                        };
                        if desc { ord.reverse() } else { ord }
                    });
                    val = Value::Array(arr);
                }
            }
            "select" => {
                if toks.len() < 2 {
                    return Err("usage: | select <col1,col2,...>".to_string());
                }
                let fields: Vec<String> = toks[1..]
                    .iter()
                    .flat_map(|t| t.split(','))
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let project = |item: Value| -> Value {
                    let mut out = Map::new();
                    for f in &fields {
                        out.insert(f.clone(), item.get(f).cloned().unwrap_or(Value::Null));
                    }
                    Value::Object(out)
                };
                val = match val {
                    Value::Array(arr) => Value::Array(arr.into_iter().map(project).collect()),
                    other => project(other),
                };
            }
            "first" | "head" => {
                let n = toks.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(5);
                if let Value::Array(arr) = val {
                    val = Value::Array(arr.into_iter().take(n).collect());
                }
            }
            "last" | "tail" => {
                let n = toks.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(5);
                if let Value::Array(arr) = val {
                    let start = arr.len().saturating_sub(n);
                    val = Value::Array(arr.into_iter().skip(start).collect());
                }
            }
            "count" => {
                let count = match &val {
                    Value::Array(a) => a.len(),
                    Value::Object(m) => m.len(),
                    Value::Null => 0,
                    _ => 1,
                };
                val = json!({ "count": count });
            }
            "get" => {
                let Some(field) = toks.get(1) else {
                    return Err("usage: | get <field>".to_string());
                };
                val = match val {
                    Value::Array(arr) => Value::Array(
                        arr.into_iter()
                            .map(|item| item.get(field).cloned().unwrap_or(Value::Null))
                            .collect(),
                    ),
                    other => other.get(field).cloned().unwrap_or(Value::Null),
                };
            }
            "json" => {
                force_json = true;
            }
            other => {
                return Err(format!("unknown pipeline stage `{other}` (try: where, sort, select, first, last, count, get, json)"));
            }
        }
    }
    Ok((val, force_json))
}

fn truncate_cell(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('~');
    out
}

pub fn format_table(val: &Value, force_json: bool) -> String {
    if force_json {
        return serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string());
    }
    match val {
        Value::Null => "(empty)".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let max_k = map.keys().map(|k| k.len()).max().unwrap_or(4).min(18);
            let mut lines = Vec::new();
            for (k, v) in map {
                let vstr = match v {
                    Value::Array(a) => format!("[{} items]", a.len()),
                    Value::Object(m) => format!("{{{} keys}}", m.len()),
                    _ => truncate_cell(&val_as_str(v), 42),
                };
                lines.push(format!("{:<width$}  {}", truncate_cell(k, max_k), vstr, width = max_k));
            }
            lines.join("\n")
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                return "(0 rows)".to_string();
            }
            let all_objs = arr.iter().all(|v| v.is_object());
            if !all_objs {
                return arr
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("{i:>2}  {}", truncate_cell(&val_as_str(v), 54)))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            // Collect up to 5 columns in stable order from first object.
            let mut cols: Vec<String> = Vec::new();
            if let Some(Value::Object(first)) = arr.first() {
                for k in first.keys() {
                    if cols.len() < 5 {
                        cols.push(k.clone());
                    }
                }
            }
            let mut widths: Vec<usize> = cols.iter().map(|c| c.len().min(16)).collect();
            for row in arr {
                for (ci, col) in cols.iter().enumerate() {
                    let cell = row.get(col).map(val_as_str).unwrap_or_default();
                    widths[ci] = widths[ci].max(cell.chars().count().min(18));
                }
            }
            let mut lines = Vec::with_capacity(arr.len() + 2);
            let hdr = cols
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<w$}", truncate_cell(c, widths[i]), w = widths[i]))
                .collect::<Vec<_>>()
                .join("  ");
            let sep = widths
                .iter()
                .map(|w| "-".repeat(*w))
                .collect::<Vec<_>>()
                .join("  ");
            lines.push(hdr);
            lines.push(sep);
            for row in arr {
                let r = cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let cell = row.get(c).map(val_as_str).unwrap_or_default();
                        format!("{:<w$}", truncate_cell(&cell, widths[i]), w = widths[i])
                    })
                    .collect::<Vec<_>>()
                    .join("  ");
                lines.push(r);
            }
            lines.join("\n")
        }
    }
}

pub fn help_value() -> Value {
    json!([
        {"cmd": "ps | mem | cpu", "desc": "Real Zircon processes, kernel RAM, per-CPU load"},
        {"cmd": "fs ls|cat|write|rm <path>", "desc": "Inspect or edit Fxfs /data files"},
        {"cmd": "tree | apps | board", "desc": "Inspect live OS visual tree, live apps, Figments board"},
        {"cmd": "hirsel <code> | eval <code>", "desc": "Compile & run real Hirsel against typed os@1 library"},
        {"cmd": "source <app> | run <app> | rollback <app>", "desc": "Inspect, execute, or roll back live Hirsel app code"},
        {"cmd": "def <name> = <pipeline>", "desc": "Define persistent custom shell command (/data/shell.json)"},
        {"cmd": "undef <name> | defs", "desc": "Remove or list custom commands"},
        {"cmd": "rooms [name] | scale [pct]", "desc": "Inspect or switch OS room theme / UI scale"},
        {"cmd": "net | egress | audit | sys", "desc": "Network, FIDL egress_gw, audit log, OS info"},
        {"cmd": "js <code> | agent <goal>", "desc": "Eval in hirsel-js@1 or run hirsel-agent"},
        {"cmd": "vm <cmd>", "desc": "Run command inside x64 Linux 6.1.102 vm-dev"},
        {"cmd": "| where|sort|select|first|count|json", "desc": "Structured pipeline operators"}
    ])
}

/// Safe Fxfs path restriction for `fs` builtins (allows reading `/data` and `/pkg`, writing only `/data`).
pub fn sanitize_fs_path(path: &str, write: bool) -> Result<String, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok("/data".to_string());
    }
    if trimmed.contains("..") {
        return Err("path traversal `..` is not allowed".to_string());
    }
    if write {
        if !trimmed.starts_with("/data/") {
            return Err("writes are restricted to /data/<file>".to_string());
        }
    } else if !(trimmed == "/data"
        || trimmed.starts_with("/data/")
        || trimmed == "/pkg"
        || trimmed.starts_with("/pkg/")
        || trimmed == "/tmp"
        || trimmed.starts_with("/tmp/"))
    {
        return Err("reads are restricted to /data, /tmp, or /pkg".to_string());
    }
    Ok(trimmed.to_string())
}
