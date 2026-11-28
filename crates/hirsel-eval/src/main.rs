// hirsel-eval: OS-resident Hirsel Compiler, Type-Checker, and Bytecode/IR Evaluator.
//
// Runs inside an isolated zero-ambient Zircon Job spawned by `hirsel_shell`.
// Preloads Hirsel's `core` + Hirsel-OS's `os` library ONCE at startup, caches their
// checked declarations & lowered CoreIR bodies, and serves line-delimited JSON
// `check` / `eval` requests over Stdin/Stdout with exact memory limits, step bounds,
// and structured diagnostics.

use hirsel_core_eval::Outcome;
use hirsel_core_front::{CoreCheck, Emitter, GenericTemplate, ModuleInput, Program};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_ir::{FuncId, Identity, Module};
use hirsel_core_layout::Target;
use hirsel_core_materialize::{Published, Resolver, Unit};
use hirsel_core_resolve::{
    Corpus, CorpusManifest, CorpusSource, ModulePath, ModuleSource, NameIndex, PackageManifest,
    Resolution, ResolutionScratch, SourceId,
};
use hirsel_core_syntax::SyntaxFile;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

include!(concat!(env!("OUT_DIR"), "/embedded_libs.rs"));

#[derive(Deserialize)]
struct EvalRequest {
    #[serde(default = "default_op")]
    op: String,
    source: String,
    #[serde(default)]
    stdin: String,
    #[serde(default = "default_limit")]
    limit_bytes: u64,
}

fn default_op() -> String {
    "eval".to_string()
}

fn default_limit() -> u64 {
    16 * 1024 * 1024
}

#[derive(Serialize)]
struct EvalResponse {
    ok: bool,
    diagnostics: Vec<String>,
    stdout: String,
    stderr: String,
    logs: Vec<String>,
    ops: Vec<Value>,
    used_bytes: u64,
    compile_ms: u64,
    run_ms: u64,
}

struct LoweredDef {
    module: Module,
    function: FuncId,
    template: Option<GenericTemplate>,
}

struct CachedBase {
    manifest: CorpusManifest,
    sources: Vec<CorpusSource>,
    base_defs: HashMap<DefPath, LoweredDef>,
    core_modules: usize,
    os_modules: usize,
}

fn current_target() -> Target {
    #[cfg(target_arch = "aarch64")]
    {
        Target::Aarch64Linux
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        Target::X86_64Linux
    }
}

fn pkg_id(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

fn load_base(os_file: &Path) -> Result<CachedBase, String> {
    let core_pkg = pkg_id("core");
    let os_pkg = pkg_id("os");

    let mut core_sources_meta = Vec::with_capacity(EMBEDDED_CORE.len());
    let mut corpus_sources = Vec::with_capacity(EMBEDDED_CORE.len() + 1);

    for (stem, text) in EMBEDDED_CORE {
        let msrc = ModuleSource {
            foreign: None,
            captured: None,
            file: PathBuf::from(format!("core/{stem}.hirsel")),
            modules: vec![(*stem).to_string()],
            signatures: true,
        };
        let mode = msrc.mode();
        corpus_sources.push(CorpusSource {
            module: ModulePath {
                package: core_pkg.clone(),
                modules: vec![(*stem).to_string()],
            },
            file: msrc.file.clone(),
            text: (*text).to_string(),
            mode,
        });
        core_sources_meta.push(msrc);
    }

    let os_text = std::fs::read_to_string(os_file).unwrap_or_else(|_| EMBEDDED_OS_HIRSEL.to_string());
    let os_msrc = ModuleSource {
        foreign: None,
        captured: None,
        file: os_file.to_path_buf(),
        modules: vec!["os".to_string()],
        signatures: false,
    };
    let os_mode = os_msrc.mode();
    corpus_sources.push(CorpusSource {
        module: ModulePath {
            package: os_pkg.clone(),
            modules: vec!["os".to_string()],
        },
        file: os_file.to_path_buf(),
        text: os_text,
        mode: os_mode,
    });

    let core_modules = core_sources_meta.len();
    let os_modules = 1usize;

    let mut manifest = CorpusManifest {
        core: core_pkg.clone(),
        packages: vec![
            PackageManifest {
                namespace: None,
                package: core_pkg.clone(),
                dependencies: Vec::new(),
                sources: core_sources_meta,
                toolchain: true,
            },
            PackageManifest {
                namespace: None,
                package: os_pkg,
                dependencies: vec![core_pkg],
                sources: vec![os_msrc],
                toolchain: false,
            },
        ],
    };
    manifest.normalise(None);
    // Keep corpus_sources in exact normalised manifest order!
    let mut ordered_sources = Vec::with_capacity(corpus_sources.len());
    for pkg in &manifest.packages {
        for s in &pkg.sources {
            let mpath = ModulePath {
                package: pkg.package.clone(),
                modules: s.modules.clone(),
            };
            if let Some(cs) = corpus_sources.iter().find(|c| c.module == mpath) {
                ordered_sources.push(cs.clone());
            }
        }
    }
    let corpus = Corpus {
        manifest,
        sources: ordered_sources,
    };
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);

    let syntax_files: Vec<SyntaxFile> = parsed.files.into_iter().map(|p| p.syntax).collect();
    let modules: Vec<ModuleInput> = corpus
        .sources
        .iter()
        .zip(syntax_files.iter())
        .enumerate()
        .map(|(sid, (s, syn))| ModuleInput {
            source: SourceId(sid as u32),
            id: index.module(&s.module).unwrap(),
            syntax: syn.clone(),
        })
        .collect();

    let mut scratch = ResolutionScratch::default();
    let resolutions: Vec<Vec<Arc<Resolution>>> = modules
        .iter()
        .map(|m| {
            m.syntax
                .items()
                .enumerate()
                .map(|(i, item)| Arc::new(scratch.resolve(&index, m.id, m.source, i as u32, item)))
                .collect()
        })
        .collect();

    let program = Program::declare_resolved(&index, &modules, resolutions.clone());
    if !program.refusals.is_empty() {
        let errs: Vec<String> = program
            .refusals
            .iter()
            .take(8)
            .map(|r| format!("{}: {}", r.rule, r.message))
            .collect();
        return Err(format!("base library declaration refusals: {}", errs.join("; ")));
    }

    let mut base_defs = HashMap::new();
    for f in 0..program.fns.len() {
        let path = program.fns[f].path.clone();
        if program.fns[f].body.is_none() {
            continue;
        }
        let mut emitter = Emitter::new();
        let check: CoreCheck = program.check_lower(f, &mut emitter);
        emitter.seal();
        if !check.refusals.is_empty() && path.0.package.name == "os" {
            let errs: Vec<String> = check
                .refusals
                .iter()
                .map(|r| format!("{}: [{}] {}", path, r.rule, r.message))
                .collect();
            return Err(format!("os library body refusal: {}", errs.join("; ")));
        }
        if check.refusals.is_empty() && check.gaps.is_empty() {
            if let Some(func_id) = check.function {
                base_defs.insert(
                    path,
                    LoweredDef {
                        module: emitter.module,
                        function: func_id,
                        template: check.template,
                    },
                );
            }
        }
    }

    Ok(CachedBase {
        manifest: corpus.manifest,
        sources: corpus.sources,
        base_defs,
        core_modules,
        os_modules,
    })
}

fn wrap_if_script(raw: &str) -> (String, usize) {
    // If the user wrote a complete Hirsel module with `fn main(`, run it verbatim.
    let has_main = raw.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("pub fn main(") || t.starts_with("fn main(")
    });
    if has_main {
        return (raw.to_string(), 0);
    }
    let mut wrapped = String::with_capacity(raw.len() + 140);
    wrapped.push_str("pub fn main(input: Stdin, out: Stdout) waits {\n");
    wrapped.push_str("    let mut ctx = os.open(input, out)\n");
    for line in raw.lines() {
        wrapped.push_str("    ");
        wrapped.push_str(line);
        wrapped.push('\n');
    }
    wrapped.push_str("}\n");
    (wrapped, 2)
}

fn format_refusal(
    r: &hirsel_core_resolve::Refusal,
    index: &NameIndex,
    sources: &[CorpusSource],
    syntax_files: &[SyntaxFile],
    line_offset: usize,
) -> String {
    let mpath = index.module_path(r.primary.module);
    if let Some(pos) = sources.iter().position(|s| s.module == mpath) {
        let syn = &syntax_files[pos];
        if let Some(item) = syn.items.get(r.primary.item as usize) {
            let offset = (item.file_offset + r.primary.span.start) as usize;
            let text = &sources[pos].text;
            let clamped = offset.min(text.len());
            let before = &text[..clamped];
            let raw_line = before.matches('\n').count() + 1;
            let line = raw_line.saturating_sub(line_offset).max(1);
            let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            return format!("{}:{} [{}] {}", line, col, r.rule, r.message);
        }
    }
    format!("[{}] {}", r.rule, r.message)
}

fn compile_and_maybe_run(base: &CachedBase, req: &EvalRequest) -> EvalResponse {
    let t0 = Instant::now();
    let (source_text, line_offset) = wrap_if_script(&req.source);

    let script_pkg = pkg_id("script");
    let script_src = ModuleSource {
        foreign: None,
        captured: None,
        file: PathBuf::from("<script.hirsel>"),
        modules: vec!["main".into()],
        signatures: false,
    };
    let script_mode = script_src.mode();
    let mut manifest = base.manifest.clone();
    manifest.packages.push(PackageManifest {
        namespace: None,
        package: script_pkg.clone(),
        dependencies: vec![pkg_id("core"), pkg_id("os")],
        sources: vec![script_src],
        toolchain: false,
    });

    let mut sources = base.sources.clone();
    sources.push(CorpusSource {
        module: ModulePath {
            package: script_pkg.clone(),
            modules: vec!["main".into()],
        },
        file: PathBuf::from("<script.hirsel>"),
        text: source_text.clone(),
        mode: script_mode,
    });

    let corpus = Corpus { manifest, sources };
    let parsed = corpus.parse();
    let index = corpus.index(&parsed);

    // Check syntax diagnostics on the script file first
    let mut diagnostics = Vec::new();
    if let Some(script_out) = parsed.files.last() {
        for err in &script_out.diagnostics {
            let off = (err.primary.start as usize).min(source_text.len());
            let before = &source_text[..off];
            let line = (before.matches('\n').count() + 1).saturating_sub(line_offset).max(1);
            let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            diagnostics.push(format!("{line}:{col} [syntax] {}", err.message));
        }
    }
    if !diagnostics.is_empty() {
        return EvalResponse {
            ok: false,
            diagnostics,
            stdout: String::new(),
            stderr: String::new(),
            logs: Vec::new(),
            ops: Vec::new(),
            used_bytes: 0,
            compile_ms: t0.elapsed().as_millis() as u64,
            run_ms: 0,
        };
    }

    let syntax_files: Vec<SyntaxFile> = parsed.files.into_iter().map(|p| p.syntax).collect();
    let modules: Vec<ModuleInput> = corpus
        .sources
        .iter()
        .zip(syntax_files.iter())
        .enumerate()
        .map(|(sid, (s, syn))| ModuleInput {
            source: SourceId(sid as u32),
            id: index.module(&s.module).unwrap(),
            syntax: syn.clone(),
        })
        .collect();

    let mut scratch = ResolutionScratch::default();
    let resolutions: Vec<Vec<Arc<Resolution>>> = modules
        .iter()
        .map(|m| {
            m.syntax
                .items()
                .enumerate()
                .map(|(i, item)| Arc::new(scratch.resolve(&index, m.id, m.source, i as u32, item)))
                .collect()
        })
        .collect();

    let entry_path = DefPath::from_full("script@1/main/main").unwrap();
    let program = Program::declare_resolved(&index, &modules, resolutions);

    for r in &program.refusals {
        if index.module_path(r.primary.module).package == script_pkg {
            diagnostics.push(format_refusal(r, &index, &corpus.sources, &syntax_files, line_offset));
        }
    }
    for g in &program.gaps {
        if index.module_path(g.site.module).package == script_pkg {
            diagnostics.push(format!("[unsupported] {}", g.what));
        }
    }

    // Lower ONLY script functions (all 207 core + os functions are already cached in `base.base_defs`!)
    let mut script_defs: HashMap<DefPath, LoweredDef> = HashMap::new();
    for f in 0..program.fns.len() {
        if program.fns[f].path.0.package != script_pkg {
            continue;
        }
        let path = program.fns[f].path.clone();
        if program.fns[f].body.is_none() {
            continue;
        }
        let mut emitter = Emitter::new();
        let check = program.check_lower(f, &mut emitter);
        emitter.seal();
        for r in &check.refusals {
            diagnostics.push(format_refusal(r, &index, &corpus.sources, &syntax_files, line_offset));
        }
        for g in &check.gaps {
            diagnostics.push(format!("[unsupported] {}", g.what));
        }
        if let Some(func_id) = check.function {
            script_defs.insert(
                path,
                LoweredDef {
                    module: emitter.module,
                    function: func_id,
                    template: check.template,
                },
            );
        }
    }

    if !diagnostics.is_empty() {
        return EvalResponse {
            ok: false,
            diagnostics,
            stdout: String::new(),
            stderr: String::new(),
            logs: Vec::new(),
            ops: Vec::new(),
            used_bytes: 0,
            compile_ms: t0.elapsed().as_millis() as u64,
            run_ms: 0,
        };
    }

    if !script_defs.contains_key(&entry_path) {
        return EvalResponse {
            ok: false,
            diagnostics: vec!["missing entry function `main`".to_string()],
            stdout: String::new(),
            stderr: String::new(),
            logs: Vec::new(),
            ops: Vec::new(),
            used_bytes: 0,
            compile_ms: t0.elapsed().as_millis() as u64,
            run_ms: 0,
        };
    }

    // If op == "check", type-check + ownership-check succeeded!
    if req.op == "check" {
        return EvalResponse {
            ok: true,
            diagnostics: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            logs: vec!["check OK".to_string()],
            ops: Vec::new(),
            used_bytes: 0,
            compile_ms: t0.elapsed().as_millis() as u64,
            run_ms: 0,
        };
    }

    // Materialize reachable closure starting from `script@1/main/main`
    let root = Identity::plain(entry_path);
    let target = current_target();
    let mut resolver = Resolver::default();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    let mut materialized_list: Vec<(Identity, Option<Module>, FuncId)> = Vec::new();
    let mut order_ids: Vec<Identity> = Vec::new();

    queue.push_back(root.clone());
    seen.insert(root.clone());

    while let Some(ident) = queue.pop_front() {
        let args = match resolver.arguments(&ident) {
            Ok(a) => a,
            Err(h) => {
                return EvalResponse {
                    ok: false,
                    diagnostics: vec![format!("missing type closure {h:?} for {}", ident.definition)],
                    stdout: String::new(),
                    stderr: String::new(),
                    logs: Vec::new(),
                    ops: Vec::new(),
                    used_bytes: 0,
                    compile_ms: t0.elapsed().as_millis() as u64,
                    run_ms: 0,
                };
            }
        };

        let mat = if hirsel_core_front::is_glue(&ident.definition) {
            hirsel_core_materialize::materialize_glue(&ident, &args, target)
        } else {
            let def = script_defs
                .get(&ident.definition)
                .or_else(|| base.base_defs.get(&ident.definition));
            let Some(def) = def else {
                return EvalResponse {
                    ok: false,
                    diagnostics: vec![format!("missing lowered body for {}", ident.definition)],
                    stdout: String::new(),
                    stderr: String::new(),
                    logs: Vec::new(),
                    ops: Vec::new(),
                    used_bytes: 0,
                    compile_ms: t0.elapsed().as_millis() as u64,
                    run_ms: 0,
                };
            };
            let published = Published {
                module: &def.module,
                function: def.function,
                template: def.template.as_ref(),
            };
            hirsel_core_materialize::materialize(&published, &ident, &args, target)
        };

        let mat = match mat {
            Ok(m) => m,
            Err(e) => {
                return EvalResponse {
                    ok: false,
                    diagnostics: vec![format!("materialize {}: {e}", ident.definition)],
                    stdout: String::new(),
                    stderr: String::new(),
                    logs: Vec::new(),
                    ops: Vec::new(),
                    used_bytes: 0,
                    compile_ms: t0.elapsed().as_millis() as u64,
                    run_ms: 0,
                };
            }
        };

        resolver.add(&mat.children);
        for child in &mat.children {
            if seen.insert(child.identity.clone()) {
                queue.push_back(child.identity.clone());
            }
        }
        order_ids.push(ident.clone());
        materialized_list.push((ident, mat.module, mat.function));
    }

    let units: Vec<Unit<'_>> = materialized_list
        .iter()
        .map(|(ident, mod_opt, func_id)| {
            let module: &Module = match mod_opt {
                Some(m) => m,
                None => script_defs
                    .get(&ident.definition)
                    .or_else(|| base.base_defs.get(&ident.definition))
                    .map(|d| &d.module)
                    .expect("non-generic module"),
            };
            Unit {
                identity: ident,
                module,
                function: *func_id,
            }
        })
        .collect();

    let (joined, entry_fn) = match hirsel_core_materialize::join(&units, &root) {
        Ok(res) => res,
        Err(e) => {
            return EvalResponse {
                ok: false,
                diagnostics: vec![format!("join IR failed: {e}")],
                stdout: String::new(),
                stderr: String::new(),
                logs: Vec::new(),
                ops: Vec::new(),
                used_bytes: 0,
                compile_ms: t0.elapsed().as_millis() as u64,
                run_ms: 0,
            };
        }
    };

    let compile_ms = t0.elapsed().as_millis() as u64;
    let t_run = Instant::now();

    // Match exact parameter count of `entry_fn` (usually 2 for `(input: Stdin, out: Stdout)`, or 0)
    let sig_id = joined.functions[entry_fn.index()].signature;
    let param_count = joined.signatures[sig_id.index()].params.len();
    let call_args = vec![0u64; param_count];

    let report = hirsel_core_eval::run_stdio(
        &joined,
        entry_fn,
        &call_args,
        req.limit_bytes.clamp(65_536, 64 * 1024 * 1024),
        req.stdin.as_bytes(),
        None,
    );
    let run_ms = t_run.elapsed().as_millis() as u64;

    let stdout_str = String::from_utf8_lossy(&report.stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&report.stderr).to_string();

    let mut logs = Vec::new();
    let mut ops = Vec::new();
    for line in stdout_str.lines() {
        if let Some(msg) = line.strip_prefix("@HIRSEL_LOG ") {
            logs.push(msg.to_string());
        } else if let Some(op_json) = line.strip_prefix("@HIRSEL_OP ") {
            if let Ok(v) = serde_json::from_str::<Value>(op_json) {
                ops.push(v);
            }
        } else if !line.trim().is_empty() {
            logs.push(line.to_string());
        }
    }

    match report.outcome {
        Outcome::Returned(_) => EvalResponse {
            ok: true,
            diagnostics: Vec::new(),
            stdout: stdout_str,
            stderr: stderr_str,
            logs,
            ops,
            used_bytes: report.used,
            compile_ms,
            run_ms,
        },
        Outcome::Failed(causes) => EvalResponse {
            ok: false,
            diagnostics: causes,
            stdout: stdout_str,
            stderr: stderr_str,
            logs,
            ops: Vec::new(), // Transactional: discard ops on failure!
            used_bytes: report.used,
            compile_ms,
            run_ms,
        },
        Outcome::Stopped(reason) => EvalResponse {
            ok: false,
            diagnostics: vec![format!("execution stopped: {reason}")],
            stdout: stdout_str,
            stderr: stderr_str,
            logs,
            ops: Vec::new(), // Transactional: discard ops on stop!
            used_bytes: report.used,
            compile_ms,
            run_ms,
        },
    }
}

fn resolve_dir(arg_flag: &str, default_pkg: &str, fallback_local: &str) -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == arg_flag {
            if let Some(p) = args.get(i + 1) {
                return PathBuf::from(p);
            }
        }
    }
    let pkg = PathBuf::from(default_pkg);
    if pkg.exists() {
        return pkg;
    }
    PathBuf::from(fallback_local)
}

fn main() {
    let os_file = resolve_dir(
        "--os",
        "/pkg/data/hirsel/os/os.hirsel",
        "/workspace/hirsel-os/hirsel-os-lib/os.hirsel",
    );

    let base = match load_base(&os_file) {
        Ok(b) => b,
        Err(e) => {
            let err = json!({ "ready": false, "error": e });
            println!("{err}");
            let _ = io::stdout().flush();
            std::process::exit(1);
        }
    };

    let banner = json!({
        "ready": true,
        "engine": "hirsel-core-eval",
        "arch": std::env::consts::ARCH,
        "core_modules": base.core_modules,
        "os_modules": base.os_modules,
        "cached_defs": base.base_defs.len(),
    });
    println!("{banner}");
    let _ = io::stdout().flush();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line_res in stdin.lock().lines() {
        let Ok(line) = line_res else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<EvalRequest>(trimmed) {
            Ok(req) => compile_and_maybe_run(&base, &req),
            Err(e) => EvalResponse {
                ok: false,
                diagnostics: vec![format!("invalid request JSON: {e}")],
                stdout: String::new(),
                stderr: String::new(),
                logs: Vec::new(),
                ops: Vec::new(),
                used_bytes: 0,
                compile_ms: 0,
                run_ms: 0,
            },
        };
        if let Ok(json_line) = serde_json::to_string(&resp) {
            let _ = writeln!(stdout, "{json_line}");
            let _ = stdout.flush();
        }
    }
}
