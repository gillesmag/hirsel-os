//! `hirsel-init`: Talos-style root process for Hirsel-OS on Zircon.
//!
//! Booted directly by Zircon's `userboot` via `userboot.next=bin/hirsel-init`,
//! bypassing Fuchsia's `component_manager` entirely.

#[cfg(target_os = "fuchsia")]
mod api;
#[cfg(target_os = "fuchsia")]
mod caps;
#[cfg(target_os = "fuchsia")]
mod runner;
#[cfg(target_os = "fuchsia")]
mod vm_manager;

#[cfg(target_os = "fuchsia")]
mod fuchsia_main {
    use crate::runner::BootContext;
    use std::sync::Arc;
    use zx_sys::*;

    pub fn run() -> ! {
        let mono_ns = unsafe { zx_clock_get_monotonic() };
        let boot_ns = unsafe { zx_clock_get_boot() };
        let mut rng = [0u8; 8];
        unsafe { zx_cprng_draw(rng.as_mut_ptr(), rng.len()) };

        println!("============================================================");
        println!("[hirsel-os] Zircon root process `bin/hirsel-init` is ALIVE!");
        println!("[hirsel-os] monotonic_ns = {mono_ns}");
        println!("[hirsel-os] boot_ns      = {boot_ns}");
        println!("[hirsel-os] cprng_sample = {:016x}", u64::from_le_bytes(rng));
        println!("[hirsel-os] No shell, no SSH, no component_manager.");
        println!("============================================================");

        match BootContext::take_from_userboot() {
            Ok(ctx) => {
                let mut t1_runs: u64 = 0;
                let mut t2_runs: u64 = 0;
                println!(
                    "[hirsel-os] BootContext ready: {} BOOTFS entries, vmex={}, hypervisor={}, iommu={}, mmio={}, ioport={}",
                    ctx.bootfs_index.len(),
                    ctx.vmex_resource != ZX_HANDLE_INVALID,
                    ctx.hypervisor_resource != ZX_HANDLE_INVALID,
                    ctx.iommu_resource != ZX_HANDLE_INVALID,
                    ctx.mmio_resource != ZX_HANDLE_INVALID,
                    ctx.ioport_resource != ZX_HANDLE_INVALID,
                );
                const DEFAULT_WORKLOAD: &str = "bin/hirsel-hello";
                if ctx.bootfs_index.contains_key(DEFAULT_WORKLOAD) {
                    println!(
                        "[hirsel-os] Spawning Tier 1 Zircon Job for `{DEFAULT_WORKLOAD}`..."
                    );
                    let t0 = unsafe { zx_clock_get_monotonic() };
                    match ctx.run_tier1_job(DEFAULT_WORKLOAD) {
                        Ok((retcode, stdout)) => {
                            t1_runs += 1;
                            let elapsed_us = (unsafe { zx_clock_get_monotonic() } - t0) / 1000;
                            for line in String::from_utf8_lossy(&stdout).lines() {
                                println!("[hirsel-job] {line}");
                            }
                            println!(
                                "[hirsel-os] Tier 1 Job `{DEFAULT_WORKLOAD}` exited with code={retcode} in {elapsed_us} us"
                            );
                        }
                        Err(e) => {
                            println!("[hirsel-os] Tier 1 Job failed: {e}");
                        }
                    }
                }

                const DEFAULT_GUEST: &str = "bin/hirsel-guest.elf";
                if ctx.hypervisor_resource != ZX_HANDLE_INVALID
                    && ctx.bootfs_index.contains_key(DEFAULT_GUEST)
                {
                    println!(
                        "[hirsel-os] Spawning Tier 2 Hardware MicroVM for `{DEFAULT_GUEST}`..."
                    );
                    match ctx.read_bootfs_bytes(DEFAULT_GUEST).and_then(|elf| {
                        hirsel_vmm::run_elf_microvm(
                            ctx.hypervisor_resource,
                            ctx.vmex_resource,
                            &elf,
                            hirsel_vmm::MicroVmConfig::default(),
                        )
                    }) {
                        Ok(report) => {
                            t2_runs += 1;
                            for line in String::from_utf8_lossy(&report.stdout).lines() {
                                println!("[hirsel-vm] {line}");
                            }
                            println!(
                                "[hirsel-os] Tier 2 MicroVM `{DEFAULT_GUEST}` exited with code={} in {} us (io_exits={}, mmio_exits={}, bells={})",
                                report.exit_code,
                                report.elapsed_us,
                                report.io_exits,
                                report.mmio_exits,
                                report.bell_count,
                            );
                        }
                        Err(e) => {
                            println!("[hirsel-os] Tier 2 MicroVM failed: {e}");
                        }
                    }

                    // Tier 2 NPT hardware isolation self-test:
                    // A hostile guest attempts to read unmapped GPA 0x2000_0000 (beyond its 2 MiB RAM).
                    // `mov eax, 0x20000000; mov rax, [rax]; hlt`
                    const HOSTILE_GUEST: &[u8] = &[
                        0xb8, 0x00, 0x00, 0x00, 0x20, // mov eax, 0x20000000
                        0x48, 0x8b, 0x00,             // mov rax, [rax]
                        0xf4,                         // hlt
                    ];
                    let isolation_cfg = hirsel_vmm::MicroVmConfig {
                        ram_bytes: 2 * 1024 * 1024,
                        max_vm_exits: 100,
                    };
                    match hirsel_vmm::run_raw_microvm(
                        ctx.hypervisor_resource,
                        ctx.vmex_resource,
                        0x0010_0000,
                        HOSTILE_GUEST,
                        isolation_cfg,
                    ) {
                        Err(fault) if fault.contains("zx_vcpu_enter") => {
                            println!(
                                "[hirsel-os] Tier 2 NPT isolation check PASSED (hostile guest trapped: {fault})"
                            );
                        }
                        Err(err) => {
                            println!(
                                "[hirsel-os] Tier 2 NPT isolation check FAILED during setup: {err}"
                            );
                        }
                        Ok(r) => {
                            println!(
                                "[hirsel-os] Tier 2 NPT isolation check UNEXPECTEDLY succeeded: code={}",
                                r.exit_code
                            );
                        }
                    }
                }

                // Enter the Talos-style remote control-plane API (virtio-net TCP + virtio-blk + COM2).
                crate::api::serve_control_plane(Arc::new(ctx), t1_runs, t2_runs);
            }
            Err(e) => {
                println!("[hirsel-os] BootContext init failed: {e}");
            }
        }

        // Stay alive cleanly as PID 1 / root process.
        loop {
            let next = unsafe { zx_clock_get_monotonic() }.saturating_add(5_000_000_000);
            unsafe { zx_nanosleep(next) };
        }
    }
}

fn main() {
    #[cfg(target_os = "fuchsia")]
    {
        fuchsia_main::run();
    }
    #[cfg(not(target_os = "fuchsia"))]
    {
        println!(
            "hirsel-init: host stub build (compile for x86_64-unknown-fuchsia to boot on Zircon)"
        );
    }
}
