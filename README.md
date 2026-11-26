# Hirsel-OS

A minimal, Talos-style, immutable, API-driven capability OS & hypervisor built directly on the **Zircon** microkernel — with no shell, no SSH, and no Fuchsia `component_manager`.

Zircon's `userboot` launches `bin/hirsel-init` directly (`userboot.next=bin/hirsel-init`), which brings up Ring-3 `virtio-blk` and `virtio-net` (`smoltcp` TCP/IP) drivers, a mutually authenticated & ChaCha20-Poly1305 AEAD-encrypted control plane, and two hardware-enforced workload execution tiers:

- **Tier 1 — Native Hirsel Processes in Hardened Zircon Jobs**: Compiles Hirsel programs natively for `x86_64-unknown-fuchsia`, streams them content-addressed by SHA-256, seals them into immutable executable VMOs (`vmex`), and executes them in ~2–3 ms inside policy-locked `zx::Job`s (`ZX_POL_VMAR_WX`, `ZX_POL_AMBIENT_MARK_VMO_EXEC`, and `ZX_POL_NEW_PROCESS` all `DENY`). Supports per-job `Args`, attenuated `Env`, wall-clock kill timeouts (`zx_task_kill`), and a Zircon-channel `Dir` capability backed by persistent `virtio-blk` storage (`HRSLFS01`).
- **Tier 2 — Hardware-Isolated AMD SVM / Intel VMX MicroVMs**: Uses Zircon's kernel hypervisor (`zx_guest_create`, `zx_vcpu_create`, `zx_vcpu_enter`, `zx_guest_set_trap`) to run either freestanding 64-bit Ring-0 ELF guests or **unmodified Linux x86-64 static ELF binaries** in Ring 3 inside `bin/hirsel-linux-shim.elf` with zero-exit `SYSCALL`/`SYSRET` via AMD SVM MSRPM passthrough, Linux `boot_params`/E820 setup, and ~5.5 ms cold-to-exit runtime.

---

## Architecture

```text
+-----------------------------------------------------------------------------+
|  Operator Host (`hirsel-os-ctl`)                                            |
|    Mutual HMAC-SHA256 Auth -> HKDF-SHA256 -> RFC 8439 ChaCha20-Poly1305     |
+------------------------------------+----------------------------------------+
                                     | TCP :4444 (virtio-net) or COM2 0x2F8
+------------------------------------v----------------------------------------+
|  Hirsel-OS on Zircon (`out/hirsel-os.zbi`, ~4.5 MiB)                        |
|                                                                             |
|  bin/hirsel-init (PID 1 / Root Process, no shell / no component_manager)    |
|   ├── Control Plane API  : Mutual Auth + AEAD RPC, Content-Addressed Cache  |
|   ├── hirsel-netd        : Ring-3 Virtio 1.0 PCI Net + smoltcp TCP/IP       |
|   ├── hirsel-stored      : Ring-3 Virtio 1.0 PCI Blk                        |
|   │     ├── HRSLBLK1     : Persistent SHA-256 ELF Image Store (128 slots)   |
|   │     ├── HRSLFS01     : Persistent Tier 1 Dir Capability File Store      |
|   │     └── HRSLAB01     : Immutable A/B ZBI Upgrade Staging & Metadata     |
|   ├── Tier 1 Runner      : Policy-locked zx::Job + ldsvc + Dir/Net Channels |
|   └── Tier 2 VMM         : zx_guest/zx_vcpu MicroVMs + Linux Ring-3 Shim    |
|                                                                             |
|  Zircon Microkernel (x86_64) + AMD SVM & Nested Page Tables (NPT) Backend   |
+-----------------------------------------------------------------------------+
```

---

## Workspace Crates

| Crate | Purpose |
|---|---|
| [`crates/hirsel-init`](crates/hirsel-init) | Root Zircon process (`userboot.next=bin/hirsel-init`): BOOTFS loader, `fuchsia.ldsvc.Loader`, Tier 1 job runner, Tier 1 `Dir` Zircon-channel server (`caps.rs`), encrypted multi-transport control plane (`api.rs`). |
| [`crates/hirsel-vmm`](crates/hirsel-vmm) | Tier 2 hardware microVM VMM on `zx_guest_*` / `zx_vcpu_*`, 4-level identity page tables, COM1 UART, paravirt clock/CSPRNG/exit ports, NPT MMIO/doorbell traps, Linux x86-64 `boot_params` / `bzImage` loader (`linux_boot.rs`), and Ring-3 Linux static ELF runner. |
| [`crates/hirsel-virtio`](crates/hirsel-virtio) | Ring-3 Virtio 1.0 Modern PCI drivers (`virtio-blk`, `virtio-net`), Zircon IOMMU/BTI contiguous DMA, GPR inline-asm MMIO, `HRSLBLK1` image store, `HRSLFS01` persistent file store, and `HRSLAB01` A/B ZBI upgrade slots. |
| [`crates/hirsel-proto`](crates/hirsel-proto) | Zero-dependency binary RPC protocol (`HRSL`), IEEE CRC32, SHA-256, RFC 2104 HMAC-SHA256, HKDF-SHA256, and RFC 8439 ChaCha20-Poly1305 AEAD (`crypto.rs`). |
| [`crates/hirsel-os-ctl`](crates/hirsel-os-ctl) | Operator CLI: `ping`, `info`, `images`, `run`, `run-bootfs`, `upgrade`, `upgrade-status`. Compiles Hirsel packages, deduplicates by SHA-256, and streams over authenticated AEAD sessions. |
| [`crates/zx-sys`](crates/zx-sys) | `#![no_std]` Zircon vDSO syscall & `processargs` ABI bindings. |
| [`crates/zbi`](crates/zbi) | Pure-Rust Zircon Boot Image (ZBI) container parser and inspector. |

## Examples & Guest Payloads

- [`examples/hello-fuchsia`](examples/hello-fuchsia): Native Hirsel program baked into BOOTFS (`bin/hirsel-hello`).
- [`examples/hello-remote`](examples/hello-remote): Hirsel program compiled on host, streamed over `virtio-net`, cached in `virtio-blk`, and run in Tier 1.
- [`examples/hello-caps`](examples/hello-caps): Exercises Tier 1 `Args`, attenuated `Env=HIRSEL_*`, and persistent Zircon-channel `Dir` across runs and cold reboots.
- [`examples/guest-microvm`](examples/guest-microvm): Freestanding 64-bit Ring-0 Tier 2 MicroVM guest verifying CPUID hypervisor leaf, NPT RAM, MMIO traps, async doorbells, clock, and CSPRNG.
- [`examples/linux-shim`](examples/linux-shim): 14 KiB Ring-0 -> Ring-3 Linux x86-64 compatibility shim (`bin/hirsel-linux-shim.elf`) setting up GDT/TSS/user page tables and zero-exit `SYSCALL`/`SYSRET`.
- [`examples/linux-workload`](examples/linux-workload): Unmodified Linux x86-64 static ELF (`hello-linux`) using raw Linux `syscall`s (`uname`, `clock_gettime`, `getrandom`, `brk`, `write`, `exit_group`) that runs identically natively on Linux and inside Hirsel-OS Tier 2 AMD SVM Ring 3.

## Upstreamable Patches

- [`patches/zircon-amd-svm.patch`](patches/zircon-amd-svm.patch): Full AMD-V (SVM) + Nested Page Tables (NPT) hypervisor backend for Zircon (`zircon/kernel/arch/x86/hypervisor/`), including MSRPM passthrough and x86-64 MMIO instruction-length decoding for nested KVM (`ktest x86_hypervisor_guest` passes 3/3).
- [`patches/hirsel-fuchsia-target.patch`](patches/hirsel-fuchsia-target.patch): `x86_64-unknown-fuchsia` compiler target, runtime host provider (`host/fuchsia/`), and Zircon-channel `Dir` capability client (`zircon_dir.rs`) for Hirsel.

---

## Quick Start

### 1. Run Host Unit Tests

```bash
just test
```

Verifies RFC 4231 HMAC-SHA256, RFC 8439 §2.8.2 ChaCha20-Poly1305 AEAD, encrypted session roundtrips, `HRSLBLK1`/`HRSLFS01`/`HRSLAB01` persistence across reopen, Linux `boot_params`/`bzImage` layout, identity page tables, and ZBI container framing.

### 2. Build & Run Complete End-to-End KVM Verification

After bootstrapping Zircon (`just bootstrap-zircon && just configure-zircon && just build-zircon`, applying `patches/zircon-amd-svm.patch` to `third-party/fuchsia`):

```bash
# Run Zircon AMD SVM kernel unit tests under KVM
just ktest x86_hypervisor_guest

# Run the complete 10-step end-to-end KVM suite (auth rejection, AEAD RPC,
# Tier 1 Args/Env/persistent Dir, Tier 2 unmodified Linux static ELF in Ring 3,
# A/B ZBI upgrade staging, and cold reboot persistence verification)
just test-all
```

### 3. Boot Interactively & Operate with `hirsel-os-ctl`

```bash
# Terminal 1: boot Zircon + Hirsel-OS under KVM (TCP :4444 + COM2 :4445 + 32 MiB disk)
just boot-os

# Terminal 2: operate the shell-less node via encrypted RPC
just ctl ping
just ctl info
just ctl images

# Compile, upload, and run a Tier 1 Hirsel program with capability grants
just ctl run --tier 1 \
  --arg alpha --arg zircon \
  --env HIRSEL_ROLE=worker-1 \
  --timeout-ms 5000 \
  --grant-dir \
  examples/hello-caps

# Run an unmodified Linux x86-64 static ELF in Ring 3 inside AMD SVM
just build-linux-workload
just ctl run --tier 2 --linux-elf out/hello-linux

# Stage an immutable A/B ZBI OS upgrade onto the inactive disk slot
just ctl upgrade out/hirsel-os.zbi
just ctl upgrade-status
```

---

## Verified Benchmark Snapshot (Nested KVM on AMD EPYC/Ryzen)

| Metric | Observed |
|---|---|
| `out/hirsel-os.zbi` (Zircon + `hirsel-init` + Tier 1 & Tier 2 guests + Linux shim) | **4.5 MiB** |
| Zircon `ktest x86_hypervisor_guest` (AMD SVM + NPT) | **3/3 pass (20.7 ms)** |
| Authenticated ChaCha20-Poly1305 AEAD `ping` RTT over `virtio-net` (`smoltcp`) | **~0.2–0.5 ms** |
| Tier 1 native Hirsel Job cold-to-exit (policy-locked `zx::Job` + `Dir` RPC) | **~2.9–3.4 ms** |
| Tier 2 unmodified Linux x86-64 static ELF cold-to-exit (AMD SVM Ring 3) | **~5.5 ms** |
| Cold reboot image hydration from `virtio-blk` (`HRSLBLK1`) | **~14 ms** |
| 4.5 MiB A/B ZBI upgrade streaming + SHA-256 verification + `virtio-blk` write | **1.26 s** |
