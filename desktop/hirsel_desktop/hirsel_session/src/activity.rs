// Real Zircon Kernel & Process Activity Manager Engine (`activity.rs`).
//
// Reads genuine kernel memory & per-CPU telemetry from `fuchsia.kernel.Stats`
// and genuine per-process KOID, thread count, private/shared RSS, and CPU runtime
// from Zircon `zx::Process` handles owned by `hirsel_shell` (`process_self()` plus
// every spawned Place & MicroVM child process).

use fidl_fuchsia_kernel as fkernel;
use fuchsia_component::client::connect_to_protocol;
use log::warn;
use serde::{Deserialize, Serialize};
use zx::Task as _;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub used_pct: f32,
    pub vmo_bytes: u64,
    pub wired_bytes: u64,
    pub heap_bytes: u64,
    pub mmu_bytes: u64,
    pub ipc_bytes: u64,
    pub total_mb: f64,
    pub used_mb: f64,
    pub free_mb: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CpuCoreSample {
    pub cpu: u32,
    pub busy_pct: f32,
    pub idle_ms: u64,
    pub context_switches: u64,
    pub syscalls: u64,
    pub page_faults: u64,
    pub ints: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub koid: u64,
    pub name: String,
    pub role: String,
    pub tier: String,
    pub status: String,
    pub restartable: bool,
    pub threads: usize,
    pub private_bytes: u64,
    pub shared_bytes: u64,
    pub scaled_shared_bytes: u64,
    pub rss_bytes: u64,
    pub rss_mb: f64,
    pub cpu_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageEntry {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    pub arch: String,
    pub uptime_sec: u64,
    pub kernel_stats_available: bool,
    pub memory: MemorySnapshot,
    pub num_cpus: u64,
    pub avg_cpu_pct: f32,
    pub cpus: Vec<CpuCoreSample>,
    pub processes: Vec<ProcessEntry>,
    pub storage_files: Vec<StorageEntry>,
    pub storage_total_bytes: u64,
    pub sort_by: String,
    pub selected_idx: usize,
}

pub struct ProcessHandleRef<'a> {
    pub name: &'a str,
    pub role: &'a str,
    pub tier: &'a str,
    pub restartable: bool,
    pub process: &'a zx::Process,
}

pub struct ActivityMonitor {
    stats_proxy: Option<fkernel::StatsProxy>,
    prev_wall_ns: i64,
    prev_idle_ns: Vec<i64>,
    pub sort_by: String,
    pub selected_idx: usize,
    pub latest: ActivitySnapshot,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn bytes_to_mb(b: u64) -> f64 {
    round2(b as f64 / (1024.0 * 1024.0))
}

impl ActivityMonitor {
    pub fn new() -> Self {
        let stats_proxy = match connect_to_protocol::<fkernel::StatsMarker>() {
            Ok(p) => Some(p),
            Err(e) => {
                warn!("fuchsia.kernel.Stats unavailable: {e:?}");
                None
            }
        };
        let arch = std::env::consts::ARCH.to_string();
        Self {
            stats_proxy,
            prev_wall_ns: 0,
            prev_idle_ns: Vec::new(),
            sort_by: "rss".to_string(),
            selected_idx: 0,
            latest: ActivitySnapshot {
                arch,
                sort_by: "rss".to_string(),
                ..Default::default()
            },
        }
    }

    pub fn cycle_sort(&mut self) {
        self.sort_by = match self.sort_by.as_str() {
            "rss" => "cpu".to_string(),
            "cpu" => "name".to_string(),
            _ => "rss".to_string(),
        };
        self.apply_sort();
    }

    pub fn set_sort(&mut self, sort_by: &str) -> bool {
        let norm = match sort_by.to_ascii_lowercase().as_str() {
            "rss" | "mem" | "memory" | "rss_mb" => "rss",
            "cpu" | "cpu_ms" => "cpu",
            "name" | "proc" => "name",
            _ => return false,
        };
        self.sort_by = norm.to_string();
        self.apply_sort();
        true
    }

    pub fn select_delta(&mut self, delta: isize) {
        let len = self.latest.processes.len();
        if len == 0 {
            self.selected_idx = 0;
            return;
        }
        let cur = self.selected_idx.min(len - 1) as isize;
        let next = (cur + delta).clamp(0, (len - 1) as isize) as usize;
        self.selected_idx = next;
        self.latest.selected_idx = next;
    }

    pub fn select_index(&mut self, idx: usize) {
        let len = self.latest.processes.len();
        if len == 0 {
            self.selected_idx = 0;
        } else {
            self.selected_idx = idx.min(len - 1);
        }
        self.latest.selected_idx = self.selected_idx;
    }

    pub fn selected_process(&self) -> Option<&ProcessEntry> {
        self.latest.processes.get(self.selected_idx)
    }

    fn apply_sort(&mut self) {
        let key = self.sort_by.as_str();
        self.latest.sort_by = self.sort_by.clone();
        self.latest.processes.sort_by(|a, b| match key {
            "cpu" => b
                .cpu_ms
                .cmp(&a.cpu_ms)
                .then_with(|| b.rss_bytes.cmp(&a.rss_bytes))
                .then_with(|| a.name.cmp(&b.name)),
            "name" => a.name.cmp(&b.name),
            _ => b
                .rss_bytes
                .cmp(&a.rss_bytes)
                .then_with(|| b.cpu_ms.cmp(&a.cpu_ms))
                .then_with(|| a.name.cmp(&b.name)),
        });
        if !self.latest.processes.is_empty() {
            self.selected_idx = self.selected_idx.min(self.latest.processes.len() - 1);
        } else {
            self.selected_idx = 0;
        }
        self.latest.selected_idx = self.selected_idx;
    }

    pub fn sample_process(spec: &ProcessHandleRef<'_>) -> ProcessEntry {
        let koid = spec.process.koid().map(|k| k.raw_koid()).unwrap_or(0);
        let threads = spec.process.threads().map(|v| v.len()).unwrap_or(0);
        let exited = spec
            .process
            .info()
            .map(|i| i.flags.contains(zx::ProcessInfoFlags::EXITED))
            .unwrap_or(false);
        let (private_bytes, shared_bytes, scaled_shared_bytes) = match spec.process.task_stats() {
            Ok(st) => (
                st.mem_private_bytes as u64,
                st.mem_shared_bytes as u64,
                st.mem_scaled_shared_bytes as u64,
            ),
            Err(_) => (0, 0, 0),
        };
        let rss_bytes = private_bytes.saturating_add(scaled_shared_bytes);
        let cpu_ms = spec
            .process
            .get_runtime_info()
            .map(|r| (r.cpu_time.max(0) as u64) / 1_000_000)
            .unwrap_or(0);

        ProcessEntry {
            koid,
            name: spec.name.to_string(),
            role: spec.role.to_string(),
            tier: spec.tier.to_string(),
            status: if exited { "exited".to_string() } else { "running".to_string() },
            restartable: spec.restartable,
            threads,
            private_bytes,
            shared_bytes,
            scaled_shared_bytes,
            rss_bytes,
            rss_mb: bytes_to_mb(rss_bytes),
            cpu_ms,
        }
    }

    pub async fn refresh(
        &mut self,
        procs: &[ProcessHandleRef<'_>],
        egress_healthy: bool,
        figments_web_up: bool,
        untrusted_web_up: bool,
    ) -> ActivitySnapshot {
        let now_ns = zx::MonotonicInstant::get().into_nanos();
        let uptime_sec = (now_ns.max(0) as u64) / 1_000_000_000;

        let mut kernel_stats_available = false;
        let mut memory = MemorySnapshot::default();
        let mut num_cpus = 0u64;
        let mut cpus = Vec::<CpuCoreSample>::new();

        if let Some(ref proxy) = self.stats_proxy {
            if let Ok(m) = proxy.get_memory_stats().await {
                kernel_stats_available = true;
                let total = m.total_bytes.unwrap_or(0);
                let free = m.free_bytes.unwrap_or(0);
                let used = total.saturating_sub(free);
                let used_pct = if total > 0 {
                    ((used as f64 / total as f64) * 100.0) as f32
                } else {
                    0.0
                };
                memory = MemorySnapshot {
                    total_bytes: total,
                    free_bytes: free,
                    used_bytes: used,
                    used_pct: (used_pct * 10.0).round() / 10.0,
                    vmo_bytes: m.vmo_bytes.unwrap_or(0),
                    wired_bytes: m.wired_bytes.unwrap_or(0),
                    heap_bytes: m.total_heap_bytes.unwrap_or(0),
                    mmu_bytes: m.mmu_overhead_bytes.unwrap_or(0),
                    ipc_bytes: m.ipc_bytes.unwrap_or(0),
                    total_mb: bytes_to_mb(total),
                    used_mb: bytes_to_mb(used),
                    free_mb: bytes_to_mb(free),
                };
            }

            if let Ok(cs) = proxy.get_cpu_stats().await {
                kernel_stats_available = true;
                num_cpus = cs.actual_num_cpus;
                let per_cpu = cs.per_cpu_stats.unwrap_or_default();
                let wall_delta = if self.prev_wall_ns > 0 && now_ns > self.prev_wall_ns {
                    (now_ns - self.prev_wall_ns) as f64
                } else {
                    0.0
                };

                // If this is the very first sample, ask kernel for a quick 40ms load window.
                let fallback_loads: Vec<f32> = if wall_delta <= 0.0 {
                    proxy.get_cpu_load(40_000_000).await.unwrap_or_default()
                } else {
                    Vec::new()
                };

                let mut next_idle = Vec::with_capacity(per_cpu.len());
                for (idx, pc) in per_cpu.iter().enumerate() {
                    let idle_ns = pc.idle_time.unwrap_or(0);
                    next_idle.push(idle_ns);
                    let busy_pct = if wall_delta > 0.0 && idx < self.prev_idle_ns.len() {
                        let d_idle = (idle_ns - self.prev_idle_ns[idx]).max(0) as f64;
                        let pct = (1.0 - (d_idle / wall_delta)) * 100.0;
                        ((pct.clamp(0.0, 100.0) * 10.0).round() / 10.0) as f32
                    } else if idx < fallback_loads.len() {
                        ((fallback_loads[idx].clamp(0.0, 100.0) * 10.0).round() / 10.0) as f32
                    } else {
                        0.0
                    };
                    cpus.push(CpuCoreSample {
                        cpu: pc.cpu_number.unwrap_or(idx as u32),
                        busy_pct,
                        idle_ms: (idle_ns.max(0) as u64) / 1_000_000,
                        context_switches: pc.context_switches.unwrap_or(0),
                        syscalls: pc.syscalls.unwrap_or(0),
                        page_faults: pc.page_faults.unwrap_or(0),
                        ints: pc.ints.unwrap_or(0),
                    });
                }
                self.prev_wall_ns = now_ns;
                self.prev_idle_ns = next_idle;
            }
        }

        let avg_cpu_pct = if !cpus.is_empty() {
            let sum: f32 = cpus.iter().map(|c| c.busy_pct).sum();
            ((sum / cpus.len() as f32) * 10.0).round() / 10.0
        } else {
            0.0
        };

        let mut entries: Vec<ProcessEntry> = procs.iter().map(Self::sample_process).collect();

        // Include isolated sibling components managed via FIDL so the full desktop stack is visible.
        entries.push(ProcessEntry {
            koid: 0,
            name: "egress_gw.cm".to_string(),
            role: "Isolated TLS 1.3 Vault Gateway".to_string(),
            tier: "component".to_string(),
            status: if egress_healthy { "running".to_string() } else { "degraded".to_string() },
            restartable: false,
            threads: 0,
            private_bytes: 0,
            shared_bytes: 0,
            scaled_shared_bytes: 0,
            rss_bytes: 0,
            rss_mb: 0.0,
            cpu_ms: 0,
        });
        if figments_web_up {
            entries.push(ProcessEntry {
                koid: 0,
                name: "chromium:figments".to_string(),
                role: "Chromium 160 Spatial Canvas".to_string(),
                tier: "web_engine".to_string(),
                status: "running".to_string(),
                restartable: false,
                threads: 0,
                private_bytes: 0,
                shared_bytes: 0,
                scaled_shared_bytes: 0,
                rss_bytes: 0,
                rss_mb: 0.0,
                cpu_ms: 0,
            });
        }
        if untrusted_web_up {
            entries.push(ProcessEntry {
                koid: 0,
                name: "chromium:web_untrusted".to_string(),
                role: "Isolated Web #2 Context".to_string(),
                tier: "web_engine".to_string(),
                status: "running".to_string(),
                restartable: false,
                threads: 0,
                private_bytes: 0,
                shared_bytes: 0,
                scaled_shared_bytes: 0,
                rss_bytes: 0,
                rss_mb: 0.0,
                cpu_ms: 0,
            });
        }

        // Scan Fxfs /data usage
        let mut storage_files = Vec::new();
        let mut storage_total_bytes = 0u64;
        if let Ok(rd) = std::fs::read_dir("/data") {
            for ent in rd.flatten() {
                if let Ok(meta) = ent.metadata() {
                    if meta.is_file() {
                        let bytes = meta.len();
                        storage_total_bytes = storage_total_bytes.saturating_add(bytes);
                        storage_files.push(StorageEntry {
                            path: format!("/data/{}", ent.file_name().to_string_lossy()),
                            bytes,
                        });
                    }
                }
            }
        }
        storage_files.sort_by(|a, b| a.path.cmp(&b.path));

        self.latest = ActivitySnapshot {
            arch: std::env::consts::ARCH.to_string(),
            uptime_sec,
            kernel_stats_available,
            memory,
            num_cpus,
            avg_cpu_pct,
            cpus,
            processes: entries,
            storage_files,
            storage_total_bytes,
            sort_by: self.sort_by.clone(),
            selected_idx: self.selected_idx,
        };
        self.apply_sort();
        self.latest.clone()
    }
}
