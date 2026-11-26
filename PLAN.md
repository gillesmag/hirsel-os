# Hirsel-OS: A Talos-Style Capability OS & Hypervisor on Zircon

A minimal, immutable, API-driven operating system built on the **Zircon** microkernel (Fuchsia's kernel, **without** full Fuchsia userspace / `component_manager`) designed specifically to run arbitrary Hirsel code and durable Hirsel `Place` / `Fleet` workloads.

---

## 1. Core Thesis: Why Zircon + Hirsel Fit Together

Hirsel ([/workspace/hirsel](/workspace/hirsel)) is already designed around explicit, unforgeable capabilities, zero ambient authority, bounded memory/steps, and a tiny OS-porting seam. Zircon is the rare production microkernel whose primitives map **1:1** onto Hirsel's model:

| Hirsel Concept | Linux Workaround in `/workspace/hirsel` | Native Zircon Primitive |
|---|---|---|
| Explicit capabilities (`CAP-1`..`CAP-7`) | `openat2(RESOLVE_BENEATH)`, `/proc/self/fd`, env filtering | Kernel handle table (`zx::Handle`) + right attenuation (`zx_handle_duplicate`) |
| Process-tree lifecycle (`--lifecycle managed`) | cgroup v2 subtree + `cgroup.kill` + `reap_orphans` | Zircon `Job` hierarchy + `zx_task_kill(job)` |
| Sealed executable images (`EXEC-4`) | `memfd_create` + `F_SEAL_*` + `execveat` | Immutable `Vmo` (`ZX_RIGHT_EXECUTE` without `ZX_RIGHT_WRITE`) |
| I/O readiness (`hirsel-core-net/src/ready.rs`) | `epoll` / `io_uring` + `eventfd` + `pidfd` | `zx::Port` (`zx_object_wait_async`, `zx_port_wait`, `ZX_PROCESS_TERMINATED`) |
| Memory bounds (`hirsel-guard`, `task_heap`) | `RLIMIT_DATA` + `madvise(MADV_DONTNEED)` + `mprotect` | Bounded `zx::Vmar` + `zx_vmo_op_range(ZX_VMO_OP_DECOMMIT)` |
| Suspend-inclusive Fleet clock (`FLT-5`) | `CLOCK_BOOTTIME` / `mach_continuous_time` | `zx_clock_get_boot()` (`CLOCK_BOOT`) |
| Hardware virtualization (untrusted / `Foreign` / `Exec`) | KVM + external VMM | Built-in `zx_guest_create` / `zx_vcpu_create` / `zx_vcpu_enter` |

---

## 2. Hardware Virtualization Strategy (Including AMD SVM Support)

Upstream Zircon's kernel hypervisor (`//zircon/kernel/arch/x86/hypervisor`) currently implements **Intel VMX** and **ARM64 EL2**, but **not AMD SVM (AMD-V)**.

On this development host (**AMD EPYC 7401P**, 48 threads, `/dev/kvm`, **`kvm_amd nested=1` already enabled**, 1.5 TB free), we can **add an AMD SVM backend to Zircon** and test hardware-accelerated nested VMs directly inside `qemu-system-x86_64 -enable-kvm -cpu host`.

### Why AMD SVM in Zircon Is Clean and Tractable (~2–3.5k LOC)

Compared to Intel VMX, AMD SVM is significantly simpler:

1. **Plain memory-mapped `VMCB` (4 KiB)**: No `VMREAD`/`VMWRITE`/`VMPTRLD`/`VMCLEAR` per-CPU ownership protocol — just a normal physical page (Control Area `0x000..0x3FF` + State Save Area `0x400..0xFFF`), making VCPU migration across cores straightforward (track per-CPU ASID generation).
2. **NPT uses standard x86-64 page tables**: Unlike Intel EPT's bespoke PTE layout (`X86PageTableEpt`), AMD Nested Page Tables use standard x86-64 4-level PTEs (with `U=1` on nested entries), reusing Zircon's existing x86 MMU abstractions (`X86PageTableNpt`).
3. **Clean entry/exit path**: Enable `EFER.SVME`, assign per-CPU 4 KiB `VM_HSAVE_PA` MSR, then `CLGI` → `VMLOAD` → `VMRUN` → `VMSAVE` → `STGI`.
4. **Shared upper hypervisor layers**: Zircon's `GuestDispatcher`, `VcpuDispatcher`, `TrapMap`, `InterruptTracker`, `GuestPhysicalAspace`, `LocalApicState`, x86 instruction decoder (`decode.cc`), and `zx_port_packet_t` (`GUEST_MEM`/`GUEST_IO`/`GUEST_BELL`/`GUEST_VCPU`) are already in place and shared across backends.

### Two Complementary Isolation Tiers on Zircon

With AMD SVM added, **all three major architectures** (AMD x64, Intel x64, ARM64 EL2) support both tiers:

1. **Tier 1 — Native Zircon Capability Processes (~1 ms start):**
   Pure Hirsel programs compiled to `*-unknown-fuchsia` run directly as isolated Zircon `Process`es inside restricted `Job`s (`ZX_POL_NEW_PROCESS=DENY`, `ZX_POL_AMBIENT_MARK_VMO_EXEC=DENY`, bounded `Vmar`) holding only the exact `zx::Handle`s passed by `hirsel-init`.
2. **Tier 2 — Zircon Hypervisor MicroVMs (`zx_guest_create` + `zx_vcpu_enter`):**
   Full NPT/EPT/Stage-2 hardware-virtualized microVMs for untrusted/hostile code, disposable compiler sandboxes, `Foreign` (C FFI), `Exec`, or unmodified Linux Hirsel ELF binaries.

### AMD SVM Backend Components in Zircon (`//zircon/kernel/arch/x86/hypervisor/`)

| Component | Responsibility |
|---|---|
| `svm_cpu.cc` / `svm.h` | Feature probe (`CPUID 0x80000001 ECX.SVM`, `0x8000000A` NPT/NRIP/ASID/FlushByAsid/DecodeAssists), `VM_CR` lock check, per-CPU `EFER.SVME` + `VM_HSAVE_PA` setup/teardown. |
| `vmcb.h` | Typed 4 KiB `Vmcb` layout (intercept bitmasks, `n_cr3`, `guest_asid`, `tlb_control`, `v_intr`/`v_tpr`, `event_inj`, `exitcode`/`exitinfo1`/`exitinfo2`/`next_rip`, save state area). |
| `X86PageTableNpt` | Thin NPT specialization of `X86PageTableBase` (standard x86 PTE bits with `U=1`, `n_cr3` root, TLB invalidation via ASID bump). |
| `svm_entry.S` | Minimal assembly trampoline (`CLGI`, save host callee-saved + GPRs, load guest GPRs, `VMLOAD`, `VMRUN`, `VMSAVE`, restore host GPRs, `STGI`, return exit status). |
| `svm_vcpu.cc` & `svm_guest.cc` | `SvmGuest` + `SvmVcpu` implementing the same interface as `NormalGuest`/`Vcpu`: VMCB init, IOPM/MSRPM setup, ASID allocator, register read/write (`zx_vcpu_read_state`/`write_state`), interrupt injection (`event_inj` + `v_irq`), `zx_vcpu_kick`. |
| `svm_vmexit.cc` | Dispatch `VMEXIT_*`: `NPF` (NPT fault → MMIO decode via shared `decode.cc` → `ZX_PKT_TYPE_GUEST_MEM` or page-fault map), `IOIO` (`ZX_PKT_TYPE_GUEST_IO`), `CPUID`, `MSR` (xAPIC/x2APIC, `EFER`, `PAT`, `TSC`), `HLT`, `INTR`/`VINTR`, `VMMCALL`, `SHUTDOWN`, `CR*`. |
| Arch dispatch | Runtime vendor/feature dispatch in `arch/x86/hypervisor` choosing `Vmx` vs. `Svm` behind the existing `NormalGuest` / `Vcpu` entrypoints. |

---

## 3. System Architecture (Talos-Style Minimal Zircon)

We **bypass full Fuchsia userspace entirely** (no `component_manager`, no FIDL mega-graph, no Netstack3, no shell, no SSH, no package manager). Zircon's `userboot` boots directly into our root binary via the ZBI command line:

```text
userboot.next=bin/hirsel-init
```

### Boot & Runtime Stack

```text
UEFI / Multiboot
  └─ zircon.bin (Zircon microkernel)
       └─ userboot (reads compressed BOOTFS from ZBI)
            └─ bin/hirsel-init (Root Job, written in Rust)
                 ├─ bin/hirsel-netd      (minimal userspace TCP/UDP/QUIC via smoltcp + virtio-net)
                 ├─ bin/hirsel-stored    (virtio-blk / NVMe block store: code store, VMO image store, Places)
                 ├─ bin/hirsel-api       (mTLS / pinned-QUIC remote operator + OS lifecycle API)
                 ├─ bin/hirsel-launcher  (adapted Hirsel launcher + platform_main control plane)
                 ├─ Tier 1 Jobs          (isolated Zircon Processes running pure Hirsel *-fuchsia binaries)
                 └─ bin/hirsel-vmm       (Tier 2 userspace VMM via zx_guest_create / zx_vcpu_enter + virtio)
```

### Talos-Style Immutability & Control Plane
- **Single signed ZBI image** (kernel + `userboot` + read-only Zstd `BOOTFS`) stored in **A/B boot slots** with verified boot hash and automatic rollback if `hirsel-init` fails health checks.
- **No interactive login**: All node configuration, OS upgrades, log streaming, recording tape retrieval, and workload deployment happen over a typed, authenticated network API (`hirsel-api`) compatible with `platform_main` ([platform_main.hirsel](/workspace/hirsel/core/programs/platform/platform_main.hirsel)) and [fleet.md](/workspace/hirsel/docs/spec/fleet.md).
- **Content-addressed code & VMO cache**: Admitted Hirsel recipes (`Admit -> Need -> AdmitData -> AdmitEnd`) assemble into immutable read-only+execute Zircon VMOs (`ZX_RIGHT_READ | ZX_RIGHT_EXECUTE`), shared zero-copy across runs of the same release identity.

---

## 4. Exact Porting Surface in `/workspace/hirsel`

`/workspace/hirsel` already cleanly separates OS concerns for Linux vs. Darwin. Adding Zircon/Fuchsia touches a well-bounded set of crates:

1. **Compiler Targets — [crates/hirsel-core-layout/src/target.rs](/workspace/hirsel/crates/hirsel-core-layout/src/target.rs)**
   - Add `Target::X86_64Fuchsia` (`x86_64-unknown-fuchsia`, `Os::Fuchsia`, `ObjectFormat::Elf`, `CAbi::SysV64`, `page_log2 = 12`) and `Target::Aarch64Fuchsia` (`aarch64-unknown-fuchsia`, `Os::Fuchsia`, `ObjectFormat::Elf`, `CAbi::Aapcs64`, `page_log2 = 12`).
   - Exhaustive `match` checks automatically guide updates in `hirsel-cranelift`, `hirsel-llvm`, and `hirsel-core-driver` (linking against `libzircon.so` / Fuchsia sysroot with `ld.lld`).

2. **Stack Switching — [crates/hirsel-stack/src/lib.rs](/workspace/hirsel/crates/hirsel-stack/src/lib.rs)**
   - Already uses System V x86-64 and AAPCS64 ELF calling conventions.
   - **Zircon ABI check**: Ensure generated Hirsel code + `hirsel-core-rt` either disable SafeStack/ShadowCallStack (`-fno-sanitize=safe-stack` / `-fno-sanitize=shadow-call-stack`) or save/restore the unsafe/shadow stack register (`fs:0x18` on x86-64, `x18` on aarch64) inside `hirsel_stack::switch`.

3. **Runtime Host Seam — [crates/hirsel-core-rt/src/host/mod.rs](/workspace/hirsel/crates/hirsel-core-rt/src/host/mod.rs)**
   Add `host/fuchsia/` implementing:
   - `clock.rs` — `zx_clock_get_monotonic` / `zx_clock_get_boot` and injected UTC clock handle.
   - `sys.rs` — `zx_cprng_draw` (`random`), `zx::Port` user packets (`Notice`), `zx::Timer` (`timer`), `zx_vmo_op_range(ZX_VMO_OP_DECOMMIT)` (`discard`), `zx_vmar_protect` (`decommit`).
   - `file.rs`, `path.rs`, `stat.rs`, `sync.rs` — Capability-native `Dir` handles backed by Zircon `Channel`s talking to `hirsel-stored` (no `openat2` hacks needed: directory confinement is enforced by the server behind the handle).
   - `unix.rs` — Local IPC directly over Zircon `Channel` / `Socket` handles.
   - `exec.rs` — Spawn into child `zx::Job` + sealed executable `zx::Vmo` (or refuse `Exec` in Tier 1 and route `Exec`-bearing recipes to Tier 2 microVMs).
   - `contained.rs` — Wasmtime AOT (or route `Foreign` to Tier 2 microVMs).

4. **Network & Readiness Reactor — [crates/hirsel-core-net/src/reactor.rs](/workspace/hirsel/crates/hirsel-core-net/src/reactor.rs) & [crates/hirsel-core-net/src/ready.rs](/workspace/hirsel/crates/hirsel-core-net/src/ready.rs)**
   - Add `reactor/zircon_port.rs` implementing `Selector`, `Wake`, `Event`, and `Child` exit watching (`ZX_PROCESS_TERMINATED`) on top of `zx_port_create` / `zx_object_wait_async` / `zx_port_wait`.
   - Add `host/fuchsia/` for `image.rs` (immutable executable VMO verification), `udp.rs`, and `unix.rs` (Zircon sockets/channels to `hirsel-netd`).

5. **Memory Guard & Driver Host Seam — [crates/hirsel-guard](/workspace/hirsel/crates/hirsel-guard/README.md) & [crates/hirsel-core-driver/src/host/mod.rs](/workspace/hirsel/crates/hirsel-core-driver/src/host/mod.rs)**
   - Replace `RLIMIT_DATA` in `hirsel-guard` with Zircon `Vmar` size caps / Job memory limits.
   - Add `host/fuchsia` to `hirsel-core-driver` and a `ZirconJob` / `ZirconGuest` lifecycle backend alongside `cgroup.rs` ([cgroup.rs](/workspace/hirsel/crates/hirsel-core-driver/src/launcher/cgroup.rs)).

---

## 5. Phased Implementation Roadmap

### Phase 0: Reproducible Toolchain & Minimal Zircon Boot in QEMU
**Goal:** Boot a custom minimal ZBI (`zircon.bin` + `userboot` + Rust `hirsel-init`) in QEMU in under 2 seconds, without a giant 80 GB Fuchsia source tree.
- Set up `/workspace/hirsel-os` with:
  - `flake.nix` / `justfile` providing `qemu-system-x86_64`, `qemu-system-aarch64`, `clang`, `lld`, and `zbi` (or a tiny Rust ZBI/BOOTFS packer!).
  - Pinned fetch script for prebuilt Zircon kernel (`zircon.zbi` / `multiboot.bin`), `userboot`, and Zircon sysroot (`libzircon.so`, `libc.so`, headers) from official Fuchsia CIPD artifacts — with a documented path to build Zircon from source later.
  - Rust `x86_64-unknown-fuchsia` (and `aarch64-unknown-fuchsia`) cross-compilation setup (`-Z build-std` or target sysroot + `ld.lld`).
- Build `crates/hirsel-init` (tiny Rust binary printing `"hirsel-os: zircon root up"` to the Zircon debuglog via `zx_debuglog_write`, inspecting its `processargs` bootstrap handles, and cleanly halting/rebooting).
- **Exit criterion:** `just boot-x64` and `just boot-arm64` boot Zircon in QEMU directly into `bin/hirsel-init` with zero Fuchsia userspace.

### Phase 1: Native Zircon Capability Runner (Tier 1) & Hirsel Fuchsia Target
**Goal:** Compile a real Hirsel program (`main(out: Stdout, c: Clock)`) for `x86_64-unknown-fuchsia` and run it inside an isolated Zircon `Job` launched by `hirsel-init`.
- Build a minimal Zircon syscall/handle crate (`crates/zx-lite` or vendored `fuchsia-zircon` subset) + minimal `processargs` builder for spawning child processes from an immutable VMO.
- In `/workspace/hirsel` (or a patch branch), add `Target::X86_64Fuchsia` / `Target::Aarch64Fuchsia` and the MVP `host/fuchsia/` subset (`clock`, `sys`, `stdout`/`stderr` over Zircon sockets, `reactor/zircon_port.rs`, `task_heap` on Zircon VMO/VMAR).
- Have `hirsel-init` spawn a Hirsel binary from `BOOTFS` inside a locked-down `Job`, stream its `Stdout` socket to the console, enforce a memory/time cap, and report its exit status.
- **Exit criterion:** Real Hirsel programs (pure compute + tasks/channels + `Stdout` + `Clock` + `Random` + `bound bytes`/`bound steps`) compile on Linux and execute natively on Zircon inside restricted `Job`s.

### Phase 2: Networking, Storage & Remote "Run Arbitrary Hirsel Code" API
**Goal:** Turn the bootable Zircon image into a live Talos-like appliance where a remote CLI submits arbitrary Hirsel code and streams back results.
- **`hirsel-netd`**: Minimal Zircon Virtio-Net driver (PCI/MMIO + DMA BTI) + `smoltcp` TCP/UDP stack exposing an authenticated operator endpoint (`hirsel-api`, ed25519/mTLS or QUIC) and attenuated per-workload `Net` capability channels.
- **`hirsel-stored`**: Minimal Zircon Virtio-Blk driver backing the persistent state disk (content-addressed code store, VMO image cache, and capability-confined `Dir` channels for Hirsel's `store` package).
- **Arbitrary code submission flow**:
  - Initially: CLI (`hirsel-os-ctl run main.hirsel`) compiles locally with `hirsel build --target x86_64-unknown-fuchsia` (or submits source to a dedicated compiler worker), streams recipe/ELF over `hirsel-api` (`Admit -> Need -> AdmitData -> AdmitEnd`), and `hirsel-init` runs it inside a fresh Tier 1 `Job` (with explicit grants, byte/step limits, wall-clock timeout, and optional recording tape capture).
- **Exit criterion:** `hirsel-os-ctl run ./app --limit 16M --grant Stdout,Clock` uploads and executes arbitrary Hirsel programs on a shell-less Zircon node over the network.

### Phase 3: Zircon AMD SVM Backend & Hypervisor VMM (`hirsel-vmm`, Tier 2)
**Goal:** Add AMD SVM support to the Zircon kernel (alongside existing Intel VMX / ARM64 EL2) and run hardware-virtualized microVMs via `zx_guest_create` / `zx_vcpu_create` / `zx_vcpu_enter` for hostile code, on-node untrusted compilation, `Foreign`, and `Exec`.
- **Phase 3A — Zircon Kernel Source Build & AMD SVM Backend**:
  - Set up a minimal Zircon-only build (`//zircon/kernel` + `//zircon/kernel/lib/userabi/userboot`, skipping full Fuchsia userspace).
  - Implement the AMD SVM backend (`svm_cpu.cc`, `vmcb.h`, `X86PageTableNpt`, `svm_entry.S`, `svm_guest.cc`, `svm_vcpu.cc`, `svm_vmexit.cc`) behind Zircon's existing `Guest`/`Vcpu` dispatch.
  - Validate against Zircon's own `hypervisor-unittest` suite + minimal guest smoke test running nested under `qemu-system-x86_64 -enable-kvm -cpu host` (`kvm_amd nested=1`).
- **Phase 3B — `crates/hirsel-vmm` (Userspace VMM on Zircon)**:
  - Allocates guest physical memory from a `zx::Vmo` mapped into the guest address space (`zx_guest_create`).
  - Loads a minimal guest image (tiny Linux micro-kernel for `Foreign`/`Exec`/compiler workloads, or a tiny Hirsel unikernel guest) and traps MMIO/I/O via `zx_guest_set_trap`.
  - Implements minimal userspace Virtio devices (`virtio-console`, `virtio-vsock`, `virtio-blk`, `virtio-net`, `virtio-rng`) wired directly to `hirsel-init`, `hirsel-stored`, and `hirsel-netd` Zircon channels.
- **Exit criterion:** `hirsel-os-ctl run --isolation vm ./untrusted_app` executes arbitrary Hirsel (including `Foreign` / `Exec` or on-node compilation) inside a Zircon-hosted hardware VM running natively on AMD SVM (as well as Intel VMX and ARM64 EL2).

### Phase 4: Talos Production Hardening & Hirsel Fleet Integration
**Goal:** Production-ready immutable appliance nodes managed as a Hirsel Fleet.
- **A/B ZBI upgrades**: Signed ZBI streaming over `hirsel-api`, atomic slot switch, automatic rollback on failed boot health check.
- **Persistent disk encryption & attestation**: Verified boot chain + sealed key for `/var/lib/hirsel`.
- **Full `platform_main` + `fleet@1` integration**: Run Hirsel's own [platform_main.hirsel](/workspace/hirsel/core/programs/platform/platform_main.hirsel) and [fleet.md](/workspace/hirsel/docs/spec/fleet.md) (`Archetype A` durable Places, `Archetype B` ephemeral units, `Archetype C` elastic compute) natively on `hirsel-os` nodes.

---

## 6. Proposed Workspace Layout (`/workspace/hirsel-os`)

```text
/workspace/hirsel-os/
├── PLAN.md                   # This architecture & roadmap
├── flake.nix                 # Reproducible dev shell (qemu, clang, lld, python3, just)
├── justfile                  # boot-x64, boot-arm64, fetch-zircon, build-zbi, test
├── Cargo.toml                # Rust workspace
├── scripts/
│   └── fetch-zircon.sh       # Pins & fetches prebuilt Zircon kernel + userboot + sysroot
├── crates/
│   ├── zbi/                  # Pure-Rust ZBI & BOOTFS image packer/inspector
│   ├── zx-sys/               # Minimal Zircon vDSO syscall bindings & safe Handle wrappers
│   ├── hirsel-init/          # Minimal Talos-style root process (userboot.next=bin/hirsel-init)
│   ├── hirsel-netd/          # Virtio-Net + smoltcp + attenuated Net capability server
│   ├── hirsel-stored/        # Virtio-Blk + code/VMO store + Dir capability server
│   ├── hirsel-vmm/           # Tier 2 Zircon hypervisor VMM (zx_guest_create / zx_vcpu_enter)
│   └── hirsel-os-ctl/        # Host CLI for deploying, running, inspecting, and upgrading nodes
└── patches/                  # Tracked diffs against /workspace/hirsel for Target::*Fuchsia
```
