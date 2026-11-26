//! Tier 1 Zircon Job + ELF/ldsvc process runner for Hirsel-OS.
//!
//! Parses `fuchsia.boot.Userboot` handles directly over Zircon channels,
//! reads BOOTFS entries, spawns compiled Hirsel binaries inside a strictly
//! sandboxed child `zx::Job`, serves `fuchsia.ldsvc.Loader` for `ld.so.1`,
//! and captures child stdout via a Zircon stream socket.

#![cfg(target_os = "fuchsia")]

use std::collections::HashMap;
use std::mem::size_of;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;
use zx_sys::*;

/// Per-job capability and resource grants for Tier 1 Zircon Job launches.
#[derive(Default)]
pub struct Tier1LaunchOpts {
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub timeout_ms: u64,
    pub dir_backend: Option<Arc<Mutex<crate::caps::DirBackend>>>,
    pub grant_net: bool,
}

const PAGE_SIZE: usize = 4096;

// FIDL ordinals extracted from Fuchsia's generated IR (`fuchsia.boot/Userboot` & `fuchsia.ldsvc/Loader`).
const ORD_USERBOOT_POST_BOOTFS_VMO: u64 = 0x3ba7_5f7f_dc77_fff3;
const ORD_USERBOOT_POST_BOOTFS_FILES: u64 = 0x296d_4420_db7c_c694;
const ORD_USERBOOT_POST_BOOT_HANDLES: u64 = 0x6102_b541_75b3_f1eb;

const ORD_LDSVC_DONE: u64 = 0x63ba_6b76_d367_1001;
const ORD_LDSVC_LOAD_OBJECT: u64 = 0x48c5_a151_d6df_2853;
const ORD_LDSVC_CONFIG: u64 = 0x6a8a_1a14_6463_2841;
const ORD_LDSVC_CLONE: u64 = 0x57e6_43a9_ab6e_4c29;

const ZBI_BOOTFS_MAGIC: u32 = 0xa56d_3ff9;

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

fn check(status: zx_status_t, ctx: &str) -> Result<(), String> {
    if status == ZX_OK {
        Ok(())
    } else {
        Err(format!("{ctx}: zx_status={status}"))
    }
}

fn dup(h: zx_handle_t) -> Result<zx_handle_t, String> {
    let mut out = ZX_HANDLE_INVALID;
    check(
        unsafe { zx_handle_duplicate(h, ZX_RIGHT_SAME_RIGHTS, &mut out) },
        "zx_handle_duplicate",
    )?;
    Ok(out)
}

fn object_name(h: zx_handle_t) -> String {
    let mut buf = [0u8; 32];
    let st = unsafe { zx_object_get_property(h, ZX_PROP_NAME, buf.as_mut_ptr(), buf.len()) };
    if st != ZX_OK {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// System capabilities handed off by Zircon `userboot` to `bin/hirsel-init`.
pub struct BootContext {
    pub root_job: zx_handle_t,
    pub vdso_vmo: zx_handle_t,
    pub vmex_resource: zx_handle_t,
    pub hypervisor_resource: zx_handle_t,
    pub iommu_resource: zx_handle_t,
    pub mmio_resource: zx_handle_t,
    pub ioport_resource: zx_handle_t,
    pub bootfs_vmo: zx_handle_t,
    /// Executable VMOs transferred out of `bootfs_vmo` by `userboot` (indexed by bootfs offset).
    pub bootfs_files: HashMap<u32, zx_handle_t>,
    /// Directory of `path -> (data_off, data_len)` parsed from `bootfs_vmo`.
    pub bootfs_index: HashMap<String, (u32, u32)>,
}

impl BootContext {
    pub fn take_from_userboot() -> Result<Self, String> {
        let root_job = unsafe { zx_job_default() };
        if root_job == ZX_HANDLE_INVALID {
            return Err("zx_job_default returned INVALID".into());
        }
        let vdso_vmo = unsafe { zx_take_startup_handle(pa_hnd(PA_VMO_VDSO, 0)) };
        if vdso_vmo == ZX_HANDLE_INVALID {
            return Err("PA_VMO_VDSO startup handle missing".into());
        }
        let userboot_ch = unsafe { zx_take_startup_handle(pa_hnd(PA_USER0, 0)) };
        if userboot_ch == ZX_HANDLE_INVALID {
            return Err("PA_USER0 (fuchsia.boot.Userboot) channel missing".into());
        }
        // Release the `fuchsia.ldsvc.Loader` channel held by libc so `userboot` finishes
        // serving our dynamic linker and posts `PostBootfsVmo` + `PostBootfsFiles`.
        let old_loader = unsafe { dl_set_loader_service(ZX_HANDLE_INVALID) };
        if old_loader != ZX_HANDLE_INVALID {
            unsafe { zx_handle_close(old_loader) };
        }

        let mut bootfs_vmo = ZX_HANDLE_INVALID;
        let mut system_resource = ZX_HANDLE_INVALID;
        let mut mmio_resource = ZX_HANDLE_INVALID;
        let mut ioport_resource = ZX_HANDLE_INVALID;
        let mut bootfs_files: HashMap<u32, zx_handle_t> = HashMap::new();

        // Drain pipelined messages from `userboot` until it closes its client end.
        let mut bytes = vec![0u8; 65536];
        let mut handles = vec![0u32; 64];
        loop {
            let mut obs: zx_signals_t = 0;
            let st = unsafe {
                zx_object_wait_one(
                    userboot_ch,
                    ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED,
                    ZX_TIME_INFINITE,
                    &mut obs,
                )
            };
            if st != ZX_OK {
                break;
            }
            if (obs & ZX_CHANNEL_READABLE) == 0 {
                break;
            }
            let mut nbytes = 0u32;
            let mut nhandles = 0u32;
            let rd = unsafe {
                zx_channel_read(
                    userboot_ch,
                    0,
                    bytes.as_mut_ptr(),
                    handles.as_mut_ptr(),
                    bytes.len() as u32,
                    handles.len() as u32,
                    &mut nbytes,
                    &mut nhandles,
                )
            };
            if rd != ZX_OK {
                break;
            }
            if nbytes < 16 {
                continue;
            }
            let ordinal = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
            let got_handles = &handles[..nhandles as usize];

            match ordinal {
                ORD_USERBOOT_POST_BOOTFS_VMO => {
                    if let Some(&h) = got_handles.first() {
                        bootfs_vmo = h;
                    }
                }
                ORD_USERBOOT_POST_BOOT_HANDLES => {
                    for &h in got_handles {
                        match object_name(h).as_str() {
                            "system" => system_resource = h,
                            "mmio" => mmio_resource = h,
                            "io_port" | "ioport" => ioport_resource = h,
                            _ => unsafe {
                                zx_handle_close(h);
                            },
                        }
                    }
                }
                ORD_USERBOOT_POST_BOOTFS_FILES => {
                    // Header (16) + vector header (16: count u64, presence u64) + entries (8 bytes each: offset u32, present u32).
                    if nbytes >= 32 {
                        let count = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
                        for i in 0..count.min(got_handles.len()) {
                            let pos = 32 + i * 8;
                            if pos + 4 <= nbytes as usize {
                                let off =
                                    u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
                                bootfs_files.insert(off, got_handles[i]);
                            }
                        }
                    }
                }
                _ => {
                    for &h in got_handles {
                        unsafe { zx_handle_close(h) };
                    }
                }
            }
        }
        unsafe { zx_handle_close(userboot_ch) };

        if bootfs_vmo == ZX_HANDLE_INVALID {
            return Err("userboot did not deliver PostBootfsVmo".into());
        }
        if system_resource == ZX_HANDLE_INVALID {
            return Err("userboot did not deliver system resource".into());
        }

        let mut vmex_resource = ZX_HANDLE_INVALID;
        check(
            unsafe {
                zx_resource_create(
                    system_resource,
                    ZX_RSRC_KIND_SYSTEM,
                    ZX_RSRC_SYSTEM_VMEX_BASE,
                    1,
                    b"vmex".as_ptr(),
                    4,
                    &mut vmex_resource,
                )
            },
            "create vmex resource",
        )?;

        let mut hypervisor_resource = ZX_HANDLE_INVALID;
        let _ = unsafe {
            zx_resource_create(
                system_resource,
                ZX_RSRC_KIND_SYSTEM,
                ZX_RSRC_SYSTEM_HYPERVISOR_BASE,
                1,
                b"hypervisor".as_ptr(),
                10,
                &mut hypervisor_resource,
            )
        };
        let mut iommu_resource = ZX_HANDLE_INVALID;
        let _ = unsafe {
            zx_resource_create(
                system_resource,
                ZX_RSRC_KIND_SYSTEM,
                ZX_RSRC_SYSTEM_IOMMU_BASE,
                1,
                b"iommu".as_ptr(),
                5,
                &mut iommu_resource,
            )
        };
        unsafe { zx_handle_close(system_resource) };

        let bootfs_index = parse_bootfs_directory(bootfs_vmo)?;
        Ok(Self {
            root_job,
            vdso_vmo,
            vmex_resource,
            hypervisor_resource,
            iommu_resource,
            mmio_resource,
            ioport_resource,
            bootfs_vmo,
            bootfs_files,
            bootfs_index,
        })
    }

    pub fn read_bootfs_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let &(off, len) = self
            .bootfs_index
            .get(path)
            .ok_or_else(|| format!("BOOTFS entry not found: {path}"))?;
        let mut data = vec![0u8; len as usize];
        if let Some(&transferred) = self.bootfs_files.get(&off) {
            check(
                unsafe { zx_vmo_read(transferred, data.as_mut_ptr(), 0, data.len()) },
                "zx_vmo_read(bootfs_file)",
            )?;
        } else {
            check(
                unsafe { zx_vmo_read(self.bootfs_vmo, data.as_mut_ptr(), off as u64, data.len()) },
                "zx_vmo_read(bootfs_vmo)",
            )?;
        }
        Ok(data)
    }

    pub fn open_executable_vmo(&self, path: &str) -> Result<zx_handle_t, String> {
        let &(off, len) = self
            .bootfs_index
            .get(path)
            .ok_or_else(|| format!("BOOTFS entry not found: {path}"))?;
        if let Some(&transferred) = self.bootfs_files.get(&off) {
            return dup(transferred);
        }
        let mut child = ZX_HANDLE_INVALID;
        check(
            unsafe {
                zx_vmo_create_child(
                    self.bootfs_vmo,
                    ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_NO_WRITE,
                    off as u64,
                    len as u64,
                    &mut child,
                )
            },
            "zx_vmo_create_child(bootfs)",
        )?;
        let mut exec = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_replace_as_executable(child, self.vmex_resource, &mut exec) },
            "zx_vmo_replace_as_executable",
        )?;
        let _ = unsafe { zx_object_set_property(exec, ZX_PROP_NAME, path.as_ptr(), path.len()) };
        Ok(exec)
    }

    /// Seals raw ELF bytes into an immutable executable Zircon VMO backed by `vmex_resource`.
    pub fn seal_executable_vmo(&self, name: &str, bytes: &[u8]) -> Result<zx_handle_t, String> {
        let mut raw = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_create(bytes.len() as u64, 0, &mut raw) },
            "zx_vmo_create(sealed)",
        )?;
        let write_res = check(
            unsafe { zx_vmo_write(raw, bytes.as_ptr(), 0, bytes.len()) },
            "zx_vmo_write(sealed)",
        );
        if let Err(e) = write_res {
            unsafe { zx_handle_close(raw) };
            return Err(e);
        }
        let mut exec = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_replace_as_executable(raw, self.vmex_resource, &mut exec) },
            "zx_vmo_replace_as_executable(sealed)",
        )?;
        let _ = unsafe { zx_object_set_property(exec, ZX_PROP_NAME, name.as_ptr(), name.len()) };
        Ok(exec)
    }

    /// Runs a compiled Hirsel ELF from BOOTFS inside an isolated Tier 1 Zircon Job,
    /// returning its exit code and captured stdout bytes.
    pub fn run_tier1_job(&self, bootfs_path: &str) -> Result<(i64, Vec<u8>), String> {
        self.run_tier1_job_with_opts(bootfs_path, Tier1LaunchOpts::default())
    }

    pub fn run_tier1_job_with_opts(
        &self,
        bootfs_path: &str,
        opts: Tier1LaunchOpts,
    ) -> Result<(i64, Vec<u8>), String> {
        let prog_vmo = self.open_executable_vmo(bootfs_path)?;
        self.run_tier1_vmo(bootfs_path, prog_vmo, opts)
    }

    /// Runs a compiled Hirsel ELF from an existing executable VMO (taking ownership of a
    /// duplicate handle) inside an isolated Tier 1 Zircon Job.
    pub fn run_tier1_cached_vmo_with_opts(
        &self,
        name: &str,
        cached_vmo: zx_handle_t,
        opts: Tier1LaunchOpts,
    ) -> Result<(i64, Vec<u8>), String> {
        let prog_vmo = dup(cached_vmo)?;
        self.run_tier1_vmo(name, prog_vmo, opts)
    }

    fn run_tier1_vmo(
        &self,
        bootfs_path: &str,
        prog_vmo: zx_handle_t,
        opts: Tier1LaunchOpts,
    ) -> Result<(i64, Vec<u8>), String> {

        // 1. Create a sandboxed child Zircon Job with strict Tier 1 policies.
        let mut job = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_job_create(self.root_job, 0, &mut job) },
            "zx_job_create",
        )?;
        let job_name = format!("hirsel:{bootfs_path}");
        let _ = unsafe {
            zx_object_set_property(job, ZX_PROP_NAME, job_name.as_ptr(), job_name.len())
        };
        let policies = [
            zx_policy_basic_v2_t {
                condition: ZX_POL_VMAR_WX,
                action: ZX_POL_ACTION_DENY,
                flags: ZX_POL_OVERRIDE_DENY,
            },
            zx_policy_basic_v2_t {
                condition: ZX_POL_AMBIENT_MARK_VMO_EXEC,
                action: ZX_POL_ACTION_DENY,
                flags: ZX_POL_OVERRIDE_DENY,
            },
            zx_policy_basic_v2_t {
                condition: ZX_POL_NEW_PROCESS,
                action: ZX_POL_ACTION_DENY,
                flags: ZX_POL_OVERRIDE_DENY,
            },
        ];
        check(
            unsafe {
                zx_job_set_policy(
                    job,
                    ZX_JOB_POL_RELATIVE,
                    ZX_JOB_POL_BASIC_V2,
                    policies.as_ptr().cast(),
                    policies.len() as u32,
                )
            },
            "zx_job_set_policy",
        )?;

        // 2. Create child process + initial thread.
        let mut proc_h = ZX_HANDLE_INVALID;
        let mut root_vmar = ZX_HANDLE_INVALID;
        check(
            unsafe {
                zx_process_create(
                    job,
                    bootfs_path.as_ptr(),
                    bootfs_path.len(),
                    0,
                    &mut proc_h,
                    &mut root_vmar,
                )
            },
            "zx_process_create",
        )?;
        let mut thread_h = ZX_HANDLE_INVALID;
        check(
            unsafe {
                zx_thread_create(
                    proc_h,
                    b"initial-thread".as_ptr(),
                    14,
                    0,
                    &mut thread_h,
                )
            },
            "zx_thread_create",
        )?;

        // 3. Create bootstrap channel & stdout capture socket.
        let (mut to_child_client, mut to_child_server) = (ZX_HANDLE_INVALID, ZX_HANDLE_INVALID);
        check(
            unsafe { zx_channel_create(0, &mut to_child_client, &mut to_child_server) },
            "zx_channel_create(bootstrap)",
        )?;
        let (mut stdout_local, mut stdout_child) = (ZX_HANDLE_INVALID, ZX_HANDLE_INVALID);
        check(
            unsafe { zx_socket_create(ZX_SOCKET_STREAM, &mut stdout_local, &mut stdout_child) },
            "zx_socket_create(stdout)",
        )?;

        // 4. Inspect program ELF for PT_INTERP (`ld.so.1`) and map ELF + vDSO + stack.
        let interp = read_elf_interp(prog_vmo)?;
        let mut ldsvc_server = ZX_HANDLE_INVALID;
        let entry_point = if let Some(interp_name) = interp {
            let lib_path = if interp_name.starts_with("lib/") {
                interp_name
            } else {
                format!("lib/{}", interp_name.trim_start_matches('/'))
            };
            let interp_vmo = self.open_executable_vmo(&lib_path)?;
            let loaded_interp = load_elf(interp_vmo, root_vmar)?;
            unsafe { zx_handle_close(interp_vmo) };

            let mut ldsvc_client = ZX_HANDLE_INVALID;
            check(
                unsafe { zx_channel_create(0, &mut ldsvc_client, &mut ldsvc_server) },
                "zx_channel_create(ldsvc)",
            )?;

            // Bootstrap message #1: for `ld.so.1` dynamic linker.
            let msg1_handles = [
                (pa_hnd(PA_VMO_EXECUTABLE, 0), prog_vmo),
                (pa_hnd(PA_FD, FDIO_FLAG_USE_FOR_STDIO), dup(stdout_child)?),
                (pa_hnd(PA_PROC_SELF, 0), dup(proc_h)?),
                (pa_hnd(PA_VMAR_ROOT, 0), dup(root_vmar)?),
                (pa_hnd(PA_VMAR_LOADED, 0), loaded_interp.vmar),
                (pa_hnd(PA_THREAD_SELF, 0), dup(thread_h)?),
                (pa_hnd(PA_LDSVC_LOADER, 0), ldsvc_client),
            ];
            write_processargs(to_child_client, &[], &[], &msg1_handles)?;
            loaded_interp.entry
        } else {
            let loaded = load_elf(prog_vmo, root_vmar)?;
            unsafe { zx_handle_close(prog_vmo) };
            let init_h = [
                dup(stdout_child)?,
                dup(proc_h)?,
                dup(root_vmar)?,
                loaded.vmar,
                dup(thread_h)?,
            ];
            check(
                unsafe {
                    zx_channel_write(
                        to_child_client,
                        0,
                        ptr::null(),
                        0,
                        init_h.as_ptr(),
                        init_h.len() as u32,
                    )
                },
                "zx_channel_write(static init)",
            )?;
            loaded.entry
        };

        let loaded_vdso = load_elf(self.vdso_vmo, root_vmar)?;
        unsafe { zx_handle_close(loaded_vdso.vmar) };

        let stack_size: usize = 256 * 1024;
        let mut stack_vmo = ZX_HANDLE_INVALID;
        check(
            unsafe { zx_vmo_create(stack_size as u64, 0, &mut stack_vmo) },
            "zx_vmo_create(stack)",
        )?;
        let mut stack_base = 0usize;
        check(
            unsafe {
                zx_vmar_map(
                    root_vmar,
                    ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                    0,
                    stack_vmo,
                    0,
                    stack_size,
                    &mut stack_base,
                )
            },
            "zx_vmar_map(stack)",
        )?;
        // x86-64 ABI expects initial stack aligned to 16 bytes below stack top.
        let sp = (stack_base + stack_size) & !0xf;

        // Optional Tier 1 Zircon-channel capability grants (`Dir`, `Net`).
        let mut dir_server = ZX_HANDLE_INVALID;
        let mut net_server = ZX_HANDLE_INVALID;
        let mut msg2_handles = vec![
            (pa_hnd(PA_PROC_SELF, 0), dup(proc_h)?),
            (pa_hnd(PA_VMAR_ROOT, 0), dup(root_vmar)?),
            (pa_hnd(PA_THREAD_SELF, 0), dup(thread_h)?),
            (pa_hnd(PA_VMO_STACK, 0), stack_vmo),
            (pa_hnd(PA_FD, FDIO_FLAG_USE_FOR_STDIO), stdout_child),
        ];
        if opts.dir_backend.is_some() {
            let mut dir_client = ZX_HANDLE_INVALID;
            check(
                unsafe { zx_channel_create(0, &mut dir_client, &mut dir_server) },
                "zx_channel_create(dir)",
            )?;
            msg2_handles.push((pa_hnd(PA_USER0, 1), dir_client));
        }
        if opts.grant_net {
            let mut net_client = ZX_HANDLE_INVALID;
            check(
                unsafe { zx_channel_create(0, &mut net_client, &mut net_server) },
                "zx_channel_create(net)",
            )?;
            msg2_handles.push((pa_hnd(PA_USER0, 2), net_client));
        }

        let mut argv_refs: Vec<&str> = Vec::with_capacity(1 + opts.args.len());
        argv_refs.push(bootfs_path);
        for a in &opts.args {
            argv_refs.push(a.as_str());
        }
        let env_refs: Vec<&str> = opts.env.iter().map(|s| s.as_str()).collect();

        // Bootstrap message #2: for the child program (`libc` / `fdio` / Hirsel `main`).
        write_processargs(to_child_client, &argv_refs, &env_refs, &msg2_handles)?;
        unsafe { zx_handle_close(to_child_client) };

        // 5. Start the child process!
        check(
            unsafe {
                zx_process_start(
                    proc_h,
                    thread_h,
                    entry_point,
                    sp,
                    to_child_server,
                    loaded_vdso.base,
                )
            },
            "zx_process_start",
        )?;

        // 6. Serve `fuchsia.ldsvc.Loader` and optional `Dir` capability channel in background.
        let loader_thread = if ldsvc_server != ZX_HANDLE_INVALID {
            let bootfs_vmo = dup(self.bootfs_vmo)?;
            let vmex = dup(self.vmex_resource)?;
            let index = self.bootfs_index.clone();
            let mut files_dup = HashMap::with_capacity(self.bootfs_files.len());
            for (&off, &h) in &self.bootfs_files {
                files_dup.insert(off, dup(h)?);
            }
            Some(thread::spawn(move || {
                serve_ldsvc(ldsvc_server, bootfs_vmo, vmex, &index, &files_dup);
                unsafe {
                    zx_handle_close(ldsvc_server);
                    zx_handle_close(bootfs_vmo);
                    zx_handle_close(vmex);
                    for (_, h) in files_dup {
                        zx_handle_close(h);
                    }
                }
            }))
        } else {
            None
        };

        let dir_thread = if let (Some(backend), true) =
            (opts.dir_backend, dir_server != ZX_HANDLE_INVALID)
        {
            Some(thread::spawn(move || {
                crate::caps::serve_dir_channel(dir_server, backend);
            }))
        } else {
            None
        };

        let deadline = if opts.timeout_ms > 0 {
            unsafe { zx_clock_get_monotonic() }
                .saturating_add((opts.timeout_ms as i64).saturating_mul(1_000_000))
        } else {
            ZX_TIME_INFINITE
        };

        // 7. Capture stdout stream socket until peer closes or wall-clock timeout expires.
        let mut output = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut timed_out = false;
        loop {
            let mut obs: zx_signals_t = 0;
            let st = unsafe {
                zx_object_wait_one(
                    stdout_local,
                    ZX_SOCKET_READABLE | ZX_SOCKET_PEER_CLOSED,
                    deadline,
                    &mut obs,
                )
            };
            if st == ZX_ERR_TIMED_OUT {
                timed_out = true;
                unsafe { zx_task_kill(job) };
                break;
            }
            if st != ZX_OK {
                break;
            }
            if (obs & ZX_SOCKET_READABLE) != 0 {
                let mut actual = 0usize;
                let rd = unsafe {
                    zx_socket_read(stdout_local, 0, chunk.as_mut_ptr(), chunk.len(), &mut actual)
                };
                if rd == ZX_OK && actual > 0 {
                    output.extend_from_slice(&chunk[..actual]);
                } else if rd == ZX_ERR_PEER_CLOSED {
                    break;
                }
            } else if (obs & ZX_SOCKET_PEER_CLOSED) != 0 {
                break;
            }
        }

        // 8. Wait for process termination (enforcing timeout) and read its return code.
        let mut obs: zx_signals_t = 0;
        let wait_st = unsafe {
            zx_object_wait_one(
                proc_h,
                ZX_PROCESS_TERMINATED,
                if timed_out { ZX_TIME_INFINITE } else { deadline },
                &mut obs,
            )
        };
        if wait_st == ZX_ERR_TIMED_OUT {
            timed_out = true;
            unsafe { zx_task_kill(job) };
            let _ = unsafe {
                zx_object_wait_one(proc_h, ZX_PROCESS_TERMINATED, ZX_TIME_INFINITE, &mut obs)
            };
        }
        let mut info = zx_info_process_t::default();
        check(
            unsafe {
                zx_object_get_info(
                    proc_h,
                    ZX_INFO_PROCESS,
                    ptr::from_mut(&mut info).cast(),
                    size_of::<zx_info_process_t>(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            "zx_object_get_info(ZX_INFO_PROCESS)",
        )?;

        unsafe { zx_task_kill(job) };
        if let Some(t) = loader_thread {
            let _ = t.join();
        }
        if let Some(t) = dir_thread {
            let _ = t.join();
        }
        unsafe {
            if net_server != ZX_HANDLE_INVALID {
                zx_handle_close(net_server);
            }
            zx_handle_close(stdout_local);
            zx_handle_close(thread_h);
            zx_handle_close(root_vmar);
            zx_handle_close(proc_h);
            zx_handle_close(job);
        }
        let ret_code = if timed_out { -124 } else { info.return_code };
        Ok((ret_code, output))
    }
}

fn parse_bootfs_directory(vmo: zx_handle_t) -> Result<HashMap<String, (u32, u32)>, String> {
    let mut hdr = [0u8; 16];
    check(
        unsafe { zx_vmo_read(vmo, hdr.as_mut_ptr(), 0, hdr.len()) },
        "zx_vmo_read(bootfs header)",
    )?;
    let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    let dirsize = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    if magic != ZBI_BOOTFS_MAGIC {
        return Err(format!("bad BOOTFS magic {magic:#x}"));
    }
    let mut dir = vec![0u8; dirsize];
    check(
        unsafe { zx_vmo_read(vmo, dir.as_mut_ptr(), 16, dirsize) },
        "zx_vmo_read(bootfs dir)",
    )?;
    let mut map = HashMap::new();
    let mut pos = 0usize;
    while pos + 12 <= dir.len() {
        let name_len = u32::from_le_bytes(dir[pos..pos + 4].try_into().unwrap()) as usize;
        let data_len = u32::from_le_bytes(dir[pos + 4..pos + 8].try_into().unwrap());
        let data_off = u32::from_le_bytes(dir[pos + 8..pos + 12].try_into().unwrap());
        if name_len == 0 || pos + 12 + name_len > dir.len() {
            break;
        }
        let raw_name = &dir[pos + 12..pos + 12 + name_len];
        let trimmed = raw_name.strip_suffix(&[0]).unwrap_or(raw_name);
        if let Ok(s) = std::str::from_utf8(trimmed) {
            map.insert(s.to_owned(), (data_off, data_len));
        }
        pos += (12 + name_len + 3) & !3;
    }
    Ok(map)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

fn read_elf_headers(vmo: zx_handle_t) -> Result<(Elf64Ehdr, Vec<Elf64Phdr>), String> {
    let mut ehdr_bytes = [0u8; size_of::<Elf64Ehdr>()];
    check(
        unsafe { zx_vmo_read(vmo, ehdr_bytes.as_mut_ptr(), 0, ehdr_bytes.len()) },
        "zx_vmo_read(ehdr)",
    )?;
    if &ehdr_bytes[0..4] != b"\x7fELF" {
        return Err("not an ELF binary".into());
    }
    let ehdr: Elf64Ehdr = unsafe { ptr::read_unaligned(ehdr_bytes.as_ptr().cast()) };
    let ph_size = (ehdr.e_phnum as usize) * size_of::<Elf64Phdr>();
    let mut ph_bytes = vec![0u8; ph_size];
    check(
        unsafe { zx_vmo_read(vmo, ph_bytes.as_mut_ptr(), ehdr.e_phoff, ph_size) },
        "zx_vmo_read(phdrs)",
    )?;
    let mut phdrs = Vec::with_capacity(ehdr.e_phnum as usize);
    for chunk in ph_bytes.chunks_exact(size_of::<Elf64Phdr>()) {
        phdrs.push(unsafe { ptr::read_unaligned(chunk.as_ptr().cast()) });
    }
    Ok((ehdr, phdrs))
}

fn read_elf_interp(vmo: zx_handle_t) -> Result<Option<String>, String> {
    let (_, phdrs) = read_elf_headers(vmo)?;
    for ph in phdrs {
        if ph.p_type == PT_INTERP {
            let mut buf = vec![0u8; ph.p_filesz as usize];
            check(
                unsafe { zx_vmo_read(vmo, buf.as_mut_ptr(), ph.p_offset, buf.len()) },
                "zx_vmo_read(PT_INTERP)",
            )?;
            let trimmed = buf.strip_suffix(&[0]).unwrap_or(&buf);
            return Ok(Some(String::from_utf8_lossy(trimmed).into_owned()));
        }
    }
    Ok(None)
}

struct LoadedElf {
    vmar: zx_handle_t,
    base: usize,
    entry: usize,
}

fn page_down(x: usize) -> usize {
    x & !(PAGE_SIZE - 1)
}

fn page_up(x: usize) -> usize {
    (x + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn load_elf(vmo: zx_handle_t, root_vmar: zx_handle_t) -> Result<LoadedElf, String> {
    let (ehdr, phdrs) = read_elf_headers(vmo)?;
    let mut min_vaddr = usize::MAX;
    let mut max_vaddr = 0usize;
    for ph in &phdrs {
        if ph.p_type == PT_LOAD && ph.p_memsz > 0 {
            let lo = page_down(ph.p_vaddr as usize);
            let hi = page_up((ph.p_vaddr + ph.p_memsz) as usize);
            min_vaddr = min_vaddr.min(lo);
            max_vaddr = max_vaddr.max(hi);
        }
    }
    if min_vaddr >= max_vaddr {
        return Err("ELF has no PT_LOAD segments".into());
    }
    let span = max_vaddr - min_vaddr;
    let mut sub_vmar = ZX_HANDLE_INVALID;
    let mut base = 0usize;
    check(
        unsafe {
            zx_vmar_allocate(
                root_vmar,
                ZX_VM_CAN_MAP_READ
                    | ZX_VM_CAN_MAP_WRITE
                    | ZX_VM_CAN_MAP_EXECUTE
                    | ZX_VM_CAN_MAP_SPECIFIC,
                0,
                span,
                &mut sub_vmar,
                &mut base,
            )
        },
        "zx_vmar_allocate(elf)",
    )?;

    let mut vmo_size = 0u64;
    let _ = unsafe { zx_vmo_get_size(vmo, &mut vmo_size) };

    for ph in &phdrs {
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        let seg_vaddr = ph.p_vaddr as usize;
        let seg_start = page_down(seg_vaddr);
        let page_off = seg_vaddr - seg_start;
        let vmar_off = seg_start - min_vaddr;
        let file_off = page_down(ph.p_offset as usize) as u64;
        let filesz_total = page_off + (ph.p_filesz as usize);
        let memsz_total = page_up(page_off + (ph.p_memsz as usize));
        let map_filesz = page_up(filesz_total);

        let mut perms: zx_vm_option_t = ZX_VM_SPECIFIC;
        if (ph.p_flags & PF_R) != 0 {
            perms |= ZX_VM_PERM_READ;
        }
        if (ph.p_flags & PF_W) != 0 {
            perms |= ZX_VM_PERM_WRITE;
        }
        if (ph.p_flags & PF_X) != 0 {
            perms |= ZX_VM_PERM_EXECUTE;
        }

        if (ph.p_flags & PF_W) != 0 {
            // Writable segment: copy-on-write child VMO (size = memsz_total so BSS is auto-zeroed).
            let mut seg_vmo = ZX_HANDLE_INVALID;
            check(
                unsafe {
                    zx_vmo_create_child(
                        vmo,
                        ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE,
                        file_off,
                        memsz_total as u64,
                        &mut seg_vmo,
                    )
                },
                "zx_vmo_create_child(rw segment)",
            )?;
            // Zero any trailing file bytes past `filesz_total` in the last file-backed page.
            if filesz_total < map_filesz {
                let zero_len = map_filesz - filesz_total;
                let zeros = vec![0u8; zero_len];
                check(
                    unsafe {
                        zx_vmo_write(seg_vmo, zeros.as_ptr(), filesz_total as u64, zero_len)
                    },
                    "zx_vmo_write(bss tail)",
                )?;
            }
            // Zero any pages beyond `map_filesz` if parent VMO had bytes there.
            if map_filesz < memsz_total && (file_off + map_filesz as u64) < vmo_size {
                let extra = memsz_total - map_filesz;
                let zeros = vec![0u8; extra];
                check(
                    unsafe { zx_vmo_write(seg_vmo, zeros.as_ptr(), map_filesz as u64, extra) },
                    "zx_vmo_write(bss pages)",
                )?;
            }
            let mut mapped = 0usize;
            check(
                unsafe {
                    zx_vmar_map(sub_vmar, perms, vmar_off, seg_vmo, 0, memsz_total, &mut mapped)
                },
                "zx_vmar_map(rw segment)",
            )?;
            unsafe { zx_handle_close(seg_vmo) };
        } else {
            // Read-only / executable segment: map directly from executable VMO.
            let mut mapped = 0usize;
            check(
                unsafe {
                    zx_vmar_map(
                        sub_vmar,
                        perms,
                        vmar_off,
                        vmo,
                        file_off,
                        map_filesz,
                        &mut mapped,
                    )
                },
                "zx_vmar_map(rx segment)",
            )?;
        }
    }

    Ok(LoadedElf {
        vmar: sub_vmar,
        base,
        entry: base + (ehdr.e_entry as usize - min_vaddr),
    })
}

fn write_processargs(
    channel: zx_handle_t,
    args: &[&str],
    env: &[&str],
    handles: &[(u32, zx_handle_t)],
) -> Result<(), String> {
    let mut buf = Vec::new();
    buf.resize(size_of::<zx_proc_args_t>(), 0);

    let handle_info_off = buf.len() as u32;
    let mut raw_handles = Vec::with_capacity(handles.len());
    for &(info, h) in handles {
        buf.extend_from_slice(&info.to_le_bytes());
        raw_handles.push(h);
    }

    let args_off = buf.len() as u32;
    for a in args {
        buf.extend_from_slice(a.as_bytes());
        buf.push(0);
    }
    let environ_off = buf.len() as u32;
    for e in env {
        buf.extend_from_slice(e.as_bytes());
        buf.push(0);
    }

    let hdr = zx_proc_args_t {
        protocol: ZX_PROCARGS_PROTOCOL,
        version: ZX_PROCARGS_VERSION,
        handle_info_off,
        args_off,
        args_num: args.len() as u32,
        environ_off,
        environ_num: env.len() as u32,
        names_off: 0,
        names_num: 0,
    };
    let hdr_bytes = unsafe {
        core::slice::from_raw_parts(ptr::from_ref(&hdr).cast::<u8>(), size_of::<zx_proc_args_t>())
    };
    buf[..hdr_bytes.len()].copy_from_slice(hdr_bytes);

    check(
        unsafe {
            zx_channel_write(
                channel,
                0,
                buf.as_ptr(),
                buf.len() as u32,
                raw_handles.as_ptr(),
                raw_handles.len() as u32,
            )
        },
        "zx_channel_write(processargs)",
    )
}

fn serve_ldsvc(
    server: zx_handle_t,
    bootfs_vmo: zx_handle_t,
    vmex: zx_handle_t,
    index: &HashMap<String, (u32, u32)>,
    files: &HashMap<u32, zx_handle_t>,
) {
    let mut buf = [0u8; 4096];
    let mut handles = [0u32; 8];
    loop {
        let mut obs: zx_signals_t = 0;
        let st = unsafe {
            zx_object_wait_one(
                server,
                ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED,
                ZX_TIME_INFINITE,
                &mut obs,
            )
        };
        if st != ZX_OK || (obs & ZX_CHANNEL_READABLE) == 0 {
            break;
        }
        let mut nbytes = 0u32;
        let mut nhandles = 0u32;
        let rd = unsafe {
            zx_channel_read(
                server,
                0,
                buf.as_mut_ptr(),
                handles.as_mut_ptr(),
                buf.len() as u32,
                handles.len() as u32,
                &mut nbytes,
                &mut nhandles,
            )
        };
        if rd != ZX_OK || nbytes < 16 {
            break;
        }
        for &h in &handles[..nhandles as usize] {
            unsafe { zx_handle_close(h) };
        }

        let txid = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let ordinal = u64::from_le_bytes(buf[8..16].try_into().unwrap());

        match ordinal {
            ORD_LDSVC_DONE => break,
            ORD_LDSVC_LOAD_OBJECT => {
                // FIDL string payload: size u64 at [16..24], presence u64 at [24..32], bytes at [32..32+len].
                let name = if nbytes >= 32 {
                    let len = u64::from_le_bytes(buf[16..24].try_into().unwrap()) as usize;
                    let end = (32 + len).min(nbytes as usize);
                    String::from_utf8_lossy(&buf[32..end]).into_owned()
                } else {
                    String::new()
                };
                let key = format!("lib/{name}");
                let vmo_res = index.get(&key).and_then(|&(off, len)| {
                    if let Some(&h) = files.get(&off) {
                        dup(h).ok()
                    } else {
                        let mut child = ZX_HANDLE_INVALID;
                        let st1 = unsafe {
                            zx_vmo_create_child(
                                bootfs_vmo,
                                ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_NO_WRITE,
                                off as u64,
                                len as u64,
                                &mut child,
                            )
                        };
                        if st1 != ZX_OK {
                            return None;
                        }
                        let mut exec = ZX_HANDLE_INVALID;
                        let st2 =
                            unsafe { zx_vmo_replace_as_executable(child, vmex, &mut exec) };
                        if st2 == ZX_OK { Some(exec) } else { None }
                    }
                });

                let mut resp = [0u8; 24];
                resp[0..4].copy_from_slice(&txid.to_le_bytes());
                resp[4] = 0x02; // FIDL wire format v2 flag
                resp[7] = 0x01; // FIDL magic number
                resp[8..16].copy_from_slice(&ordinal.to_le_bytes());
                if let Some(vmo) = vmo_res {
                    resp[16..20].copy_from_slice(&ZX_OK.to_le_bytes());
                    resp[20..24].copy_from_slice(&u32::MAX.to_le_bytes()); // handle present
                    let out_h = [vmo];
                    let _ = unsafe {
                        zx_channel_write(
                            server,
                            0,
                            resp.as_ptr(),
                            resp.len() as u32,
                            out_h.as_ptr(),
                            1,
                        )
                    };
                } else {
                    resp[16..20].copy_from_slice(&ZX_ERR_NOT_FOUND.to_le_bytes());
                    resp[20..24].copy_from_slice(&0u32.to_le_bytes()); // handle absent
                    let _ = unsafe {
                        zx_channel_write(
                            server,
                            0,
                            resp.as_ptr(),
                            resp.len() as u32,
                            ptr::null(),
                            0,
                        )
                    };
                }
            }
            ORD_LDSVC_CONFIG | ORD_LDSVC_CLONE => {
                let rv = if ordinal == ORD_LDSVC_CONFIG {
                    ZX_OK
                } else {
                    ZX_ERR_NOT_SUPPORTED
                };
                let mut resp = [0u8; 24];
                resp[0..4].copy_from_slice(&txid.to_le_bytes());
                resp[4] = 0x02;
                resp[7] = 0x01;
                resp[8..16].copy_from_slice(&ordinal.to_le_bytes());
                resp[16..20].copy_from_slice(&rv.to_le_bytes());
                let _ = unsafe {
                    zx_channel_write(server, 0, resp.as_ptr(), resp.len() as u32, ptr::null(), 0)
                };
            }
            _ => break,
        }
    }
}
