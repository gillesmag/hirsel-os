# Hirsel-OS Architecture: Bare-Metal Capability OS for Servers, Daily-Driver Workstations, and Built-In Agents

> **Status:** Converged Architectural Blueprint (v1)  
> **Foundations:** [Zircon Microkernel](third-party/fuchsia/zircon) · [Hirsel-OS](README.md) · [Hirsel Language & Runtime](/workspace/hirsel/README.md) · [Figments-Hirsel](/workspace/figments-hirsel/README.md)

This document specifies the complete architecture required to evolve **Hirsel-OS** from its current QEMU/KVM prototype into a **bare-metal-bootable operating system on Zircon** that serves both **headless Fleet servers** and **daily-driver workstations**—with [`figments-hirsel`](/workspace/figments-hirsel/README.md) as the spatial application layer and built-in agents as first-class, non-ambient OS citizens.

Every design decision below has been checked against the pinned `hirsel-os`, `hirsel`, and `figments-hirsel` repositories and stress-tested through multi-model academic research and adversarial cross-review. Where hardware, kernel, or ecosystem constraints create real trade-offs, this document states them plainly rather than papering over them.

---

## 1. Core Thesis & Non-Negotiable Axioms

Hirsel-OS applies Hirsel's language invariants (`CAP-1..9`, explicit byte/step budgets, durable Places, deterministic replay) vertically from UEFI boot to the spatial desktop:

1. **Zero Ambient Authority Everywhere.** No global POSIX root, no ambient `$HOME`, no ambient sockets, no ambient secrets, and no root shell. Every process, driver, VM, figment, and agent holds only the explicit, attenuable capabilities granted to it. (On Workstation profiles, we selectively bring in Fuchsia's native graphics/display subsystem—`sysmem`, `display-coordinator`, `Magma`, and `Flatland`—in a bounded host realm rather than handing the GPU to a Linux VM.)
2. **The Place Is the Unit of Durable State.** Processes, drivers, and MicroVMs are disposable incarnations; durable state lives in transactional `store@1` Places (`control-place`, `board/*`, `integrations-place`, agent Places) and immutable content-addressed blobs.
3. **The LLM Is an Untrusted Coprocessor.** Security lives in deterministic OS brokers, revocable caretaker membranes, and kernel/hypervisor boundaries—never in prompts, classifiers, or model compliance.
4. **Universal IPv4/IPv6 Underlay, Capability Innovation Above It.** Every network interface speaks standard, interoperable IPv4 and IPv6 (DHCPv4, SLAAC/NDP, DHCPv6, TCP/UDP, QUIC/HTTP/3, WebRTC). Authority, credential brokering, cryptographic mesh identity, and spend metering sit cleanly above standard IP.
5. **One Signed Image, Two Boot Profiles.** The same immutable Zircon Boot Image (`ZKI`) boots either a **Server Profile** (headless Fleet node running Places, Tier-2 sandboxes, and elastic compute) or a **Workstation Profile** (adding native Fuchsia GPU/display/Flatland composition, trusted input, `figments-hirsel`, Wi-Fi/media driver domains, and labeled Linux App VMs).

---

## 2. Verified Baseline (What Already Exists Today)

Before adding bare-metal components, the architecture anchors on verified facts in our three checkouts:

### 2.1 `hirsel-os` (Working Today Under KVM)
- **Direct Zircon Boot:** `userboot.next=bin/hirsel-init` boots a ~4.4 MiB ZBI with no shell, no SSH, and no Fuchsia userspace ([README.md](README.md)).
- **Tier 1 Native Hirsel Jobs (~2.9–3.4 ms cold-to-exit):** Content-addressed SHA-256 ELFs sealed into executable `vmex` VMOs running in policy-locked `zx::Job`s (`ZX_POL_VMAR_WX`, `ZX_POL_AMBIENT_MARK_VMO_EXEC`, and `ZX_POL_NEW_PROCESS` all `DENY`) with `Args`, attenuated `Env`, wall-clock timeouts, and Zircon-channel `Dir` backed by `HRSLFS01`.
- **Tier 2 Hardware MicroVMs:** Upstreamable Rust AMD SVM + NPT hypervisor backend alongside Intel VMX and ARM64 EL2 ([patches/zircon-amd-svm.patch](patches/zircon-amd-svm.patch)), running Ring-3 static Linux ELFs (~5.5 ms), persistent SMP (1–8 vCPU) Linux 6.1.102 MicroVMs (~469 ms to PID 1 / ~800 ms to SSH), `virtio-net` with L3/L4 IPv4 NAT, persistent named `virtio-blk` volumes (`HRSLVOL1`), network-less `virtio-console` exec, and `--autostart` cold-boot reconciliation.
- **Known Prototype Gaps to Close:**
  - Host `smoltcp` (IPv4-only) currently runs inside `hirsel-init` ([crates/hirsel-init/src/api.rs](crates/hirsel-init/src/api.rs)).
  - Tier 1 `Net` grant allocates a channel whose server side is not yet wired ([crates/hirsel-init/src/runner.rs](crates/hirsel-init/src/runner.rs)).
  - `HRSLFS01` is a 256 KiB / 64-file demonstration store ([crates/hirsel-virtio/src/lib.rs](crates/hirsel-virtio/src/lib.rs)), not a crash-tested production backing for `store@1`.
  - **Critical P0 Kernel Finding:** Fuchsia removed Intel VT-d on March 30, 2026 (`0b2e904a`). In our pinned Zircon checkout ([third-party/fuchsia/zircon/kernel/dev/iommu/mod.rs](third-party/fuchsia/zircon/kernel/dev/iommu/mod.rs)), **`x86_64` implements ONLY `StubIommu`** (no Intel VT-d, no AMD-Vi). On `aarch64`, Rust `ArmSmmuV2` is wired into boot/dispatch, and `arm_smmu/v3` exists in-tree (`c199da91`) with controller, queues, BTI, and PMT, pending boot wiring and PCI requester-ID stream routing.

### 2.2 `hirsel` Language, `store@1`, `runner@1`, and `fleet@1`
- Explicit opaque capabilities (`CAP-1..9`), attenuation, virtual `provide.*` providers, `bound bytes` / `bound steps`, deterministic recording tapes, and `hirsel.laws` test-strength locks ([/workspace/hirsel/docs/laws.md](/workspace/hirsel/docs/laws.md)).
- `store@1` already owns path-copying CoW B+trees, `store.fork`, `adopt`, recorded `reexecute`, crash-safe WAL/journal durability, immutable packs, and S3 conditional-write `BucketBacking` ([/workspace/hirsel/core/programs/store/STORE-SPEC.md](/workspace/hirsel/core/programs/store/STORE-SPEC.md)).
- Existing network & media capabilities: `Resolve <pattern>` separated from `Connect`, `Tls <pattern> ... <trust>`, `NET-18` public-only reach, `Secret` (`TLS-5`), `Cred` (`TLS-7`), pure-Hirsel `webrtc_sfu@1` + `turn@1`, and specified sans-IO SPKI-pinned `quic@1` ([/workspace/hirsel/docs/spec/net.md](/workspace/hirsel/docs/spec/net.md), [/workspace/hirsel/docs/spec/quic.md](/workspace/hirsel/docs/spec/quic.md)).
- **Unbuilt Fleet Seams:** In [/workspace/hirsel/docs/spec/fleet.md](/workspace/hirsel/docs/spec/fleet.md), the move-authorization kernel and proofs exist, while Store `ClaimAuth`/`ORIGIN`, Runner `Deposed`, launcher envelopes, and platform seams remain `unbuilt` and are explicit dependencies for Gate 4.

### 2.3 `figments-hirsel` (The Real Application Layer)
- Multiplayer programmable spatial canvas where **each board is one Hirsel Place** (`board/`), alongside `control-place/`, `integrations-place/`, `media/` (`webrtc_sfu@1` + `turn@1`), and `codemode/` (pure-Hirsel `js@1` bytecode VM) ([/workspace/figments-hirsel/docs/ARCHITECTURE.md](/workspace/figments-hirsel/docs/ARCHITECTURE.md)).
- **Trust split already proven in product:** Outer trusted shell (`*.boards.*`, owning HUD, modals, Talk/WebRTC, Connections, and an `outer pass` credential) framing a sandboxed **Chrome 157+ `<canvas content="drawable">` (HTML-in-Canvas)** (`*.canvas.*`) with closed Shadow DOM figments, semantic zoom, app mode, and powerbox-style `paste_files`/`paste_text` bridging.
- **Existing Agent & Auth Model:** Five-tool dotted MCP catalog (`search`, `describe`, `call`, `execute`, `skills`), **scoped named MCP connections** (boards, role ceilings, leaf actions, instant narrowing revocation), `js@1` code-mode worker with transactional receipts (`not_started | unknown | succeeded | failed`), and release-pinned `SKILL.md` figment guides.
- **Concrete Hosting Constraints:** Today's server includes 3 Node 22 services (`control`, `router`, `mcp`) behind Caddy; `hirsel serve` holds **~2.1 GiB RSS** after a board deploy (1.2–1.4 GiB peak build); and `hirsel-contain` currently refuses outbound `Net` ([/workspace/figments-hirsel/docs/HIRSEL_FEEDBACK.md](/workspace/figments-hirsel/docs/HIRSEL_FEEDBACK.md)), which must lift before `integrations-place` runs natively inside Tier 1.

---

## 3. End-to-End System Architecture

```text
+======================================================================================+
| L5: APPLICATION LAYER & BUILT-IN AGENTS                                              |
|                                                                                      |
|  [Tier-1 Native Hirsel Places]   [Native Chromium on Zircon]      [Tier-2 App VMs]   |
|   control-place · board/*         WebEngine / Ozone-Flatland       vm-dev (POSIX:    |
|   integrations-place              - figments-shell context (157+)  git/cargo/IDE)    |
|   media (webrtc_sfu@1 + turn@1)   - isolated web-browse contexts   legacy Linux apps |
|   codemode (js@1) · model-broker  (Zircon Job sandbox, no VM!)     (on-demand only)  |
+--------------------------------------------------------------------------------------+
|    Zircon Channels · Flatland ViewTokens · WebEngine FIDL · Caretaker Membranes      |
+--------------------------------------------------------------------------------------+
| L4: CAPABILITY BROKERS, COMPOSITOR & CORE OS SERVICES (Tier-1 TCB Jobs)              |
|  hirsel-brokerd     | hirsel-netd + egress-gw | hirsel-stored  | Fuchsia GFX Realm & |
|  Named connections, | Dual-stack IPv4/IPv6    | NVMe store@1   | hirsel-seatd        |
|  membranes, Outbox, | NET-11/TLS-3, quic@1,   | backing, CAS,  | Flatland/Scenic,    |
|  Bank, ceremonies   | FIDL socket/DNS shim,   | CoW HRSLVOL/fs | sysmem, Audio/Fonts,|
|                     | Cred proxy + vsock ssh  |                | trusted input/SAS   |
+--------------------------------------------------------------------------------------+
|    Policy-Locked zx::Job (~3 ms) · Hardware zx_guest / zx_vcpu (5.5 ms / 470 ms)     |
+--------------------------------------------------------------------------------------+
| L3: ROOT SUPERVISOR & DUAL EXECUTION ENGINE (bin/hirsel-init + hirsel-vmm)           |
|  Tier 1: Sealed vmex VMOs, VMAR_WX=DENY, fuchsia.process.Launcher, confinement certs |
|  Tier 2: SMP AMD SVM / Intel VMX / ARM64 EL2 MicroVMs, virtio-gpu (host Vulkan)      |
+--------------------------------------------------------------------------------------+
|    ring@1 Shared-VMO SPSC Rings · Sysmem VMO Collections · Static DMA Pools          |
+--------------------------------------------------------------------------------------+
| L2: HYBRID BARE-METAL DRIVER LAYER                                                   |
|  Native Host Drivers (Rust Core + Fuchsia GFX) | IOMMU-Confined Driver VMs (Tail)    |
|  hirsel-pcid · hirsel-nvmed · hirsel-tpmd      | drv-net   (Wi-Fi iwlwifi/ath12k/BT) |
|  i8042/xHCI/I2C-HID · igc/ixgbe/ENA · uACPI    | drv-media (HDA/SOF, UVC/IPU6 cam)   |
|  Fuchsia: display-coordinator, intel-display,  | drv-usb   (Thunderbolt/USB4)        |
|           Magma (msd-intel-gen, msd-arm-mali,  | (NO drv-gpu VM — host owns GPU!)    |
|           msd-virtio-gpu), sysmem              |                                     |
+--------------------------------------------------------------------------------------+
|    Zircon vDSO Syscalls · Attenuated MMIO / IRQ / BTI / Hypervisor Resources         |
+--------------------------------------------------------------------------------------+
| L1: ZIRCON MICROKERNEL (Rust+C++) & MEASURED BOOT (hirsel-boot.efi)                  |
|  UEFI -> Signed ZKI (A/B/R GPT) -> TPM PCR 11 PolicyAuthorize + Pre-Boot Vault PIN   |
|  Zircon: Handles, VMOs, Jobs, Ports, CLOCK_BOOT, SVM/VMX/EL2 + [P0: IOMMU + IRTE]    |
+======================================================================================+
```

### Boot Profiles (One Image, Different Runtime Graphs)
- **Server Profile (Headless Fleet Node):** Boots L1–L4 + Tier-1 Places + Tier-2 Sandboxes. No seat, Flatland/Magma, Chromium, audio, or Wi-Fi VMs. Uses native NVMe + `igc`/`ixgbe`/`ENA`, exposes authenticated control plane + SPKI-pinned `quic@1`, and executes `Archetype A/B/C` workloads and remote `hirsel serve` builds.
- **Workstation Profile (Daily Driver, 16–32 GB RAM):** Same ZKI image plus the bounded **Fuchsia native graphics/web realm** (`sysmem`, `display-coordinator`, `Magma`, `Flatland/Scenic`), `hirsel-seatd`, **Native Chromium (`WebEngine` / Ozone-Flatland)** hosting `figments-hirsel` and isolated browser contexts directly on Zircon, `uACPI`/`PowerLease`, optional `drv-net` (Wi-Fi) / `drv-media`, and on-demand `vm-dev` for POSIX toolchains. Eliminating Linux browser VMs cuts gigabytes of RAM overhead; 32 GB RAM is still recommended if compiling boards locally with `hirsel serve` (~2.1 GiB RSS), while 16 GB laptops offload builds to Fleet servers via `Archetype C`.

---

## 4. L1: Measured Boot, Vault, Attestation & Zircon Kernel Work

### 4.1 UEFI Boot Chain & A/B/R Disk Layout
Replace `HRSLAB01`-on-virtio-blk with a GPT layout and a tiny Rust UEFI bootloader (`hirsel-boot.efi`, bootstrapped from Fuchsia Gigaboot during bringup, then replaced to keep the TCB small and Rust-only):

- **GPT Partitions:** `ESP` (`hirsel-boot.efi` only) · `zki_a` · `zki_b` · `zki_r` (recovery) · `hirsel_meta` (A/B/R slot state + signed descriptors) · `hirsel_data` (encrypted `hirsel-stored` volume).
- **Signed ZKI (UKI-style PE):** Bundles Zircon kernel + `userboot` + compressed `BOOTFS` + `.cmdline` + `.pcrsig` + `.pubkey`, verified via Ed25519 + TPM NV anti-rollback index.
- **A/B/R Commit & Rollback (Fuchsia `libabr` / Talos model):** `hirsel-boot` decrements `tries_remaining` before booting a candidate slot; `hirsel-init` marks `successful` only after `hirsel-stored` opens, core network/seat health gates pass, and (on servers) Fleet check-in succeeds.
- **Measured Boot Primary, Secure Boot Optional:** Secure Boot key enrollment is optional hardening (via setup-mode auto-enroll), never a hard prerequisite. `hirsel-boot` measures ZKI into **PCR 11** and cmdline into **PCR 12**, followed by boot-phase extends (`hirsel:init` → `hirsel:store-ready` → `hirsel:ready`) modeled on `systemd-stub`/`systemd-measure`.
- **Update-Surviving Sealing (`TPM2_PolicyAuthorize`):** Seal keys to a signed PCR policy over vendor/fleet keys combined with local `pcrlock`-style firmware policies (PCRs 0/2/4/7)—never to raw PCR 11 digests (which break on every A/B upgrade). **Always use salted/encrypted TPM sessions** to defeat LPC/SPI bus sniffing, and require a PIN on AMD fTPM (mitigating `faulTPM`).

### 4.2 Pre-Boot Laptop Vault & Protection Classes
- **Pre-Boot Authentication (BitLocker PBA Pattern):** `hirsel-boot.efi` prompts for the vault PIN over UEFI `ConIn` and GOP framebuffer, unseals the Volume Master Key (`VMK`) before `ExitBootServices`, and hands it to `hirsel-init` in an ephemeral ZBI item (zeroed immediately after `hirsel-stored` wraps class keys). This keeps USB-HID, I2C-HID, and the graphics/display stack completely out of the vault unlock TCB. Fallbacks: Argon2id passphrase or FIDO2 `hmac-secret`.
- **Data Protection Classes (Mapped to `store@1`):**
  - `Complete`: Class key wiped from RAM on lock/suspend (sensitive personal Places/credentials).
  - `UnlessOpen`: Asymmetric wrap allowing background writes while locked.
  - `AfterFirstUnlock`: Default for background Places and system state.
  - `None`: Verified-but-non-secret immutable system blobs.
- **Memory Encryption:** Enable AMD SME / Intel TME where supported to harden against cold-boot attacks.

### 4.3 Fleet Attestation & Confidential Computing
- **RATS Attestation (`RFC 9334`):** Node identity binds to a TPM-resident Attestation Key (`AK`, certified via EK `MakeCredential`/`ActivateCredential`). `fleet@1` verifies quotes over PCRs 0/2/4/7/11 + Hirsel event log before admitting a node or granting `ClaimAuth` write authority. Workload launches append to a Merkle log whose root is attested periodically (avoiding 10–50 ms TPM latency per Job).
- **Confidential Computing (Servers):**
  - **Phase 1 — CVM Guest Mode (Cloud AMD SEV-SNP / Intel TDX):** Add `#VC`/GHCB (SNP) or `#VE`/TDCALL (TDX), `PVALIDATE`/accept at boot, a dedicated decrypted shared-page DMA pool (matching `ring@1`'s static pools), and a COCONUT-SVSM vTPM at VMPL0 so the standard TPM sealing/attestation flow works unchanged. Enforce SNP restricted/alternate injection and firmware TCB pins (mitigating Heckler, WeSee, CacheWarp, BadRAM, TDXdown).
  - **Phase 2 — CVM Host Mode:** Explicitly deferred until a concrete workload requires hosting confidential Tier-2 VMs (high RMP/ASP/SEAMCALL kernel complexity).

### 4.4 Required Zircon Kernel Extensions (P0–P1)
| Priority | Kernel Capability | Current Status in Pinned Zircon | Engineering Requirement |
|---|---|---|---|
| **P0** | **Hardware IOMMU** | x86 has **only `StubIommu`** (VT-d removed Mar 2026; no AMD-Vi). ARM64 has `ArmSmmuV2` wired and `arm_smmu/v3` in-tree (unwired). | Implement Rust **AMD-Vi** (and/or Intel VT-d) on x86, OR target **ARM64 SMMUv2/v3 + PCI stream IDs** first as the DMA-isolated bare-metal path. |
| **P0** | **Interrupt Remapping** | Absent on x86. | Implement AMD-Vi IRTE / VT-d IR + posted interrupts before allowing any untrusted PCIe passthrough (closes MSI injection). |
| **P0** | **Guest-Bound BTIs & Teardown** | BTIs pin pages in flat host space. | Bind host-managed IOVA tables to one PCIe IOMMU group + VM lifetime; enforce strict IOTLB invalidation, ATS disable, and FLR/SBR reset before reclaiming pages. |
| **P1** | **Per-Job Committed Memory Caps** | Zircon tracks attribution only (no seL4 `Untyped` kernel quota). | Use `ZX_POL_NEW_*` immediately to restrict kernel-object creation; add a kernel patch capping committed VMO/object memory per Job. |
| **P1** | **Power & Clock Syscalls** | `zx_system_suspend_enter` and `ZX_CLOCK_BOOT` exist. | Expose narrow Ring-0 MSR syscall for Intel HWP/EPP & AMD CPPC; verify x86 S3/s2idle wakeup paths and enforce RFC-0230 timelines. |

---

## 5. L2: Hybrid Bare-Metal Driver Architecture

Synthesizing **seL4 sDDF / LionsOS (2025)**, **Asterinas (USENIX ATC 2025)**, **NOVA (EuroSys 2010)**, and **Qubes OS**, Hirsel-OS uses a strict hybrid split:

> **Rule:** A driver runs natively on the Zircon host if and only if (a) it is needed before any VM can boot, (b) it sits in the boot/input/display/GPU trust chain, or (c) it sits on the `store@1` persistence hot path. Everything else (Wi-Fi, Bluetooth, SOF audio/IPU6 cameras, Thunderbolt/USB long tail) runs in an IOMMU-confined Tier-2 Linux Driver VM.

### 5.1 Native Host Drivers: Safe-Rust Core + Fuchsia Native Graphics Stack
Core system drivers link against one small, audited **`hirsel-hal`** crate (wrapping MMIO, DMA pools, IRQs, and port I/O, Asterinas framekernel style) and enforce `#![forbid(unsafe_code)]` in driver logic. For **GPU and Display**, we intentionally bring in Fuchsia's native, battle-tested userspace graphics stack from our vendored tree ([third-party/fuchsia/src/graphics](third-party/fuchsia/src/graphics), [third-party/fuchsia/src/sysmem](third-party/fuchsia/src/sysmem), [third-party/fuchsia/src/ui/scenic](third-party/fuchsia/src/ui/scenic)) inside a bounded graphics realm hosted by `hirsel-init`—so **no Linux MicroVM ever owns the physical GPU or display scanout**.

| Driver / Service | Role | Target Profile | Bringup Gate |
|---|---|---|---|
| `hirsel-pcid` | PCIe ECAM (`MCFG`), BAR/bridge windows, MSI/MSI-X, **ACS/IOMMU-group & reset-domain computation**, declarative bind matching (DFv2 style) | Server + Workstation | Gate 1 |
| `hirsel-nvmed` | Native NVMe (admin + per-CPU SQ/CQ pairs, FUA/flush, namespaces, APST); owned exclusively by `hirsel-stored` | Server + Workstation | Gate 1 |
| `hirsel-tpmd` | TPM 2.0 CRB/TIS + salted/encrypted sessions | Server + Workstation | Gate 1 |
| Wired NIC (`ixy` style) | 1–2 families first: Intel `igc` (workstation), `ixgbe` (server), cloud `ENA`/`gVNIC` + existing `virtio-net` | Server (P0) + Workstation | Gate 1 |
| GOP Framebuffer | Early-boot & recovery console from ZBI framebuffer item | Server + Workstation | Gate 1 / 3 |
| **Fuchsia `sysmem` + `display-coordinator`** | Zero-copy typed VMO buffer collection negotiation + native KMS display drivers: **`intel-display`** (Skylake/Kaby/Coffee/Tiger Lake Gen9–12), **`virtio-gpu-display`** (QEMU/KVM), **`framebuffer-*-display`** (linear fallback), **`amlogic-display`** (ARM) | Workstation | Gate 3 |
| **Fuchsia `Magma` (`msd-*`) + Mesa Vulkan ICD** | Privileged per-client GPU address-space isolation & command submission: **`msd-intel-gen`** (Intel Gen9–12 + Mesa `anv`), **`msd-arm-mali` / `msd-arm-mali-csf`** (ARM64), **`msd-virtio-gpu`** (QEMU/KVM) | Workstation | Gate 3 |
| `hirsel-seatd` HID | Native **i8042 PS/2**, **xHCI + USB-HID**, and **DesignWare I2C-HID** (for modern laptop internal keyboards/touchpads) | Workstation | Gate 3 |
| `hirsel-acpid` + `hirsel-powerd` | Isolated **uACPI** AML process (lid, battery, EC, thermal, I2C-HID `_CRS`) + Fuchsia Power Broker-style revocable **`PowerLease`** capabilities | Workstation (P1), Server (P2) | Gate 3 / 4 |

*Honest hardware & x86 caveats:*
- Until P0 AMD-Vi/VT-d lands, Gate 1 x86 native DMA drivers (`nvmed`, wired NIC) are part of the privileged hardware TCB.
- Bringing in Fuchsia's native GPU stack gives **immediate bare-metal acceleration on Intel Gen9–Gen12 laptops/desktops** (`intel-display` + `msd-intel-gen`, e.g. ThinkPad X1/T14, Framework 11th-gen, Intel NUC), **ARM64 Mali/CSF boards**, and **QEMU/KVM** (`virtio-gpu-display` + `msd-virtio-gpu`). AMD RDNA, Intel Xe2+, and NVIDIA GPUs fall back to `framebuffer-amd-display` (software composition) until native Magma System Drivers (`msd-amdgpu`, extended `msd-intel-gen`) are added.

### 5.2 Unified Ring ABI (`ring@1`), Sysmem Buffers & Static DMA Pools
Following **seL4 sDDF** and **Markuze et al. (ASPLOS 2016)** (showing copy-into-pinned-pools beats per-I/O IOMMU map/unmap churn and eliminates descriptor TOCTOU):
- Each native I/O driver binds a **static DMA pool VMO pinned once** at startup; no dynamic pin/unpin on the I/O hot path.
- Graphics surfaces negotiate typed VMO buffer collections (format, tiling modifiers, Vulkan/display usage) directly through **`sysmem`** with Zircon acquire/release event fences.
- Services and driver VMs communicate over **`ring@1`**: a bounded shared-VMO SPSC ring (single producer/consumer per queue, event-idx notification suppression, `zx_object_signal_peer` / Port batching).
- Cross-trust-domain edges copy into bounded client buffers by default; zero-copy direct sharing is opt-in only where mapping lifetimes and immutability are proven (e.g. read-only sealed VMOs or compositor-owned `sysmem` collections).
- Drivers sit **outside** the deterministic replay domain; every message crossing `ring@1` into a Hirsel Job is an explicit `provide.*` boundary event.

### 5.3 IOMMU-Confined Tier-2 Driver VMs (Non-GPU Long Tail Only)
Because the host owns the GPU and display natively, **there is no `drv-gpu` VM**—eliminating the hardest PCIe passthrough/FLR reset problem and preserving host pixel integrity. Only the remaining firmware-heavy peripheral classes run in minimal Linux MicroVMs (~64–160 MiB RAM each), gated on verified IOMMU + IRTE + reset-domain isolation (Gate 2):

| Driver VM | Hardware Covered | Host Interface | Key Risk & Mitigation |
|---|---|---|---|
| `drv-net` | Wi-Fi (`iwlwifi`, `ath11k/12k`, `mt7921`), Bluetooth | L2 frames over `ring@1` to `hirsel-netd` | Forced on laptops; keeps massive wireless firmware & 802.11/WPA stacks out of TCB. |
| `drv-media` | HDA / Intel SOF audio DSP, UVC / Intel IPU6 cameras, fingerprint | `virtio-snd` / `virtio-video` brokered streams | Keeps huge firmware blobs isolated; camera/mic capture strictly gated by host broker. |
| `drv-usb` | Thunderbolt/USB4 controllers, printers, serial, smart cards | Scoped logical endpoints; default-deny DMA | Honor DMAR opt-in; hotplugged PCIe tunnels land in untrusted IOMMU domains with ATS off. |

*Power & S0ix Honesty:* Modern laptops often lack ACPI S3 and require `s2idle`/S0ix, which demands every device—including devices owned by `drv-net`/`drv-media`—enter D3 cleanly and coordinate via paravirtual freeze/thaw hooks (having the host own `intel-display` + `msd-intel-gen` actually *simplifies* display/panel/backlight suspend compared to GPU passthrough!). Model wakefulness as revocable `PowerLease` capabilities from day one, instrument Intel PMC residency counters early, target servers and plugged-in workstations first, and treat laptop S0ix battery parity as the final qualification gate.

---

## 6. L3: Root Supervisor & Dual Execution Substrate

`bin/hirsel-init` remains the minimal root supervisor, delegating policy/protocol work to Tier-1 daemon Jobs so `hirsel-init` stays tiny and auditable:

| Isolation Boundary | Cold Start / Crossing | Trust Model | Primary Use in Hirsel-OS |
|---|---|---|---|
| **Tier 1: Policy-Locked `zx::Job`** | ~2–3 ms cold / µs Channel IPC | Kernel-enforced handle confinement (`VMAR_WX=DENY`, `NEW_PROCESS=DENY`, sealed `vmex` VMO) | **Primary OS boundary:** all OS brokers, `control-place`, `board/*`, `integrations-place`, `media`, `codemode` (`js@1`), P-Jobs and Q-Jobs. |
| **Wasm Component (inside Tier 1 Job)** | ~µs instantiation | Defense-in-depth plugin ABI; **Job is the security boundary, never Wasm alone** | Third-party tool adapters, parsers, sandboxed plugins. |
| **Tier 2: Hardware MicroVM (`hirsel-vmm`)** | 5.5 ms static ELF / ~470–800 ms SMP Linux | Hardware NPT/EPT/Stage-2 VM boundary | Hostile/model-generated native code, `Foreign`/C FFI, Linux dev toolchains (`vm-dev`), legacy Linux apps, legacy MCP stdio servers, non-GPU Driver VMs (`drv-net`/`drv-media`/`drv-usb`). |
| **Remote CVM (SNP/TDX)** | Network | Hardware-attested remote TCB | High-secrecy remote inference or multi-tenant server compute. |

Additional L3 mechanisms:
- **Confinement Certificates (EROS Constructor Pattern):** At Job launch, `hirsel-init` signs a confinement certificate binding `(image_sha256, granted_handles, rights, job_policies)` so `hirsel-brokerd` can verify a Q-Job or `codemode` worker holds zero egress/ambient handles before handing it sensitive or untrusted payloads.
- **Tier-2 CoW Snapshot/Fork (No Promise of Tier-2 Replay in v1):** Use `zx_vmo_create_child(ZX_VMO_CHILD_SNAPSHOT)` + vCPU/virtio state serialization for fast MicroVM fork/reset (Firecracker/Catalyzer style). Do **not** promise deterministic SMP Linux replay in v1 (record at virtio boundaries for audit; 1-vCPU intercepted-TSC deterministic mode is a research track only).
- **SMT & Future Hardware Hygiene:** Enforce core scheduling (no SMT sibling sharing) between Jobs/VMs of incomparable secrecy domains; keep handle ABIs opaque and attenuable so they map cleanly onto future hardware capabilities (CHERI).

---

## 7. L4a: Storage Without a Global POSIX VFS (`hirsel-stored`)

POSIX conflates naming, ambient authority, crash consistency, and layout (Pillai et al. OSDI '14; Aghayev et al. SOSP '19). Hirsel-OS exposes **no global POSIX VFS** in the OS core, while **avoiding the mistake of building a second CoW journal underneath `store@1`**.

`hirsel-stored` owns `hirsel-nvmed` exclusively and exposes **three non-overlapping storage primitives**:

1. **Crash-Safe Place Backing (Direct NVMe Substrate for `store@1`):**
   - Replaces the 256 KiB `HRSLFS01` demo with a real extent/directory allocator implementing `store@1`'s exact atomic rename, file lock, and `host.sync`/`host.sync_dir` (NVMe `FUA`/flush) contracts, validated against simulated and physical power-cut suites.
   - `store@1` remains the **single** CoW B+tree, WAL, snapshot (`store.fork`), and S3 `BucketBacking` transaction engine—zero double-journaling.
   - Explicit durability boundaries live at the **Place** level (not whole-system orthogonal persistence like EROS/TreeSLS, avoiding persisted OS bugs and uncheckpointable hardware state).
2. **Content-Addressed Immutable Blob & Code Store (`HRSLBLK` v2):**
   - Stores SHA-256-addressed Hirsel executables, Place packs, and `figments-hirsel` uploaded blobs (`/_board/blobs`). Keeps SHA-256 as the canonical identifier for seamless parity with Hirsel code identity and TPM attestation.
   - Serves immutable read-only VMOs (`vmex` for Tier-1 binaries, shared zero-copy across Jobs). Optional `zx_pager` Merkle demand-paging (Fxblob RFC-0225 style) is deferred until cold-start measurements justify managing pager deadlock/supply-path complexity.
   - **Cross-Object Atomicity Protocol:** Matches `figments-hirsel`'s proven pattern (`apps.media.gc` / `core.comms.retired_blobs`): *durably stage immutable blob → commit blob digest inside the owning Place transaction → epoch-deferred garbage collection*.
3. **CoW Block Volumes (`HRSLVOL` v2 → `virtio-blk`) + Scoped `virtio-fs` for Tier-2 Linux:**
   - Linux workloads (`git`, `cargo`, `rustc`, Chromium, Node, IDEs) need real POSIX semantics (lockfiles, atomic rename, `inotify`, mmap, `fsync`). Give each App VM its own CoW-cloned `virtio-blk` volume running real `ext4`/`btrfs`—never fake POSIX-over-KV for compilers and browsers.
   - Share specific user-granted artifacts between Hirsel Places and Linux VMs over a narrowly scoped, buffered `virtio-fs` broker (avoiding fragile DAX + `fs-verity` conflicts on writable/revoked mappings).

---

## 8. L4b: Innovative Networking Over Standard IPv4/IPv6 (`hirsel-netd` & `egress-gw`)

Extract all networking out of `hirsel-init` into isolated Tier-1 processes: **`hirsel-netd`** (L2/L3/L4 stack) and **`egress-gw`** (L7 capability & credential broker).

### 8.1 Dual-Stack IPv4/IPv6 Datapath
- **Full Standards Qualification:** DHCPv4, ARP, ICMPv4/v6, SLAAC, NDP, DAD, DHCPv6 (`RFC 9915`), scoped link-local interface IDs (extending Hirsel's `Addr` type), ICMPv6 Packet Too Big / PMTU, Happy Eyeballs (`RFC 8305`), DoH (`RFC 8484`) / DoQ (`RFC 9250`), IPv6-only/NAT64 interoperability, and explicitly granted mDNS (`RFC 6762`).
- **Staged Netstack Strategy:**
  - **Gate 1–2:** Keep **`smoltcp`** (already integrated, bounded, `#![no_std]`, zero Fuchsia build entanglement) and enable its IPv6/SLAAC support.
  - **Gate 3–4:** Port Fuchsia's **`netstack3_core`** (verified in-tree: 17/21 crates are `#![no_std]` with only `fuchsia_sync` as a runtime dependency, and it already implements `filter/conntrack` to replace hand-rolled NAT in `vm_manager.rs`) behind minimal Hirsel Zircon bindings when high-throughput dual-stack IPv6 + stateful filtering are needed.
- **Two Scheduling Profiles:** Interrupt-driven + aggressive link-change invalidation on laptops; RSS/NUMA queue pinning + bounded adaptive polling on servers.

### 8.2 Capability-Enforced Egress (Extending Hirsel's Existing Grant Vocabulary)
Do **not** invent a parallel `Route` grant type that forks Hirsel's specs. Wire the Tier-1 `Net` channel and Tier-2 virtual gateway directly to Hirsel's existing **`NET-11` (`Resolve`/`Connect`), `NET-18` (public-only reach), and `TLS-3` (`Tls <pattern> ... <trust>`)** grants:
- **Resolve-to-Dial Binding:** `hirsel-netd` resolves hostnames on behalf of confined workloads, validates returned IPs against the workload's policy (blocking RFC 1918 / link-local / metadata SSRF under `NET-18`), dials only the vetted IP set, and rechecks every HTTP redirect—closing DNS rebinding without ambient raw-IP sockets.
- **Confined Workloads Get Zero Raw NAT:** Every Tier-1 Job, native Chromium `web_instance`, and Tier-2 VM gets its network identity from its host-side channel/VMM endpoint (never caller headers). Confined agent Jobs and VMs receive only policy-governed gateway access; human browsing contexts (`web-personal`, `web-untrusted`) receive explicitly broader user-browsing grants, isolated from agent credentials.

### 8.3 Pragmatic Split-Credential Brokering (No Blanket TLS MITM)
Because blanket per-VM MITM breaks `webpki-roots` binaries, Node, mTLS, HTTP/3, SSH, and ECH (`RFC 9849`), `egress-gw` uses a **four-path credential strategy**:
1. **Allowlisted-Origin HTTP Credential Injection:** For API-key and OAuth REST services (LLM providers, GitHub REST, MCP gateways, Google Workspace via `integrations-place`), agents speak to `egress-gw` over an explicit proxy/broker channel holding only an opaque `Cred` handle (`TLS-5`/`TLS-7`, no `READ` right). `egress-gw` terminates/originates TLS to the bound destination, injects `Authorization`/SigV4 headers, performs OAuth refreshes internally, scrubs echoed secrets from responses, and **meters API dollars per `Cred`**.
2. **Minted Short-Lived Derivative Tokens:** Prefer minting scoped, expiring tokens (GitHub App installation tokens, git/docker credential helpers) over intercepting TLS whenever the upstream supports it.
3. **Brokered `ssh-agent` Over `virtio-vsock`:** Handles `git@github.com` and SSH access from `vm-dev` without private keys ever entering guest RAM.
4. **Plain `CONNECT` / Transport Grants:** Everything else (general web browsing, mTLS, HTTP/3, ECH) passes through end-to-end encrypted under `Resolve`/`Connect` policy + byte/rate quotas.

### 8.4 Cryptographic Fleet Mesh Over IPv4/IPv6
Use Hirsel's own sans-IO **SPKI-pinned `quic@1`** + existing **`turn@1`** relay (avoiding Iroh/`quinn`'s separate key path outside `TLS-8`):
- Nodes enroll once via physical pairing or signed invite pinning the Fleet root key, receiving a stable node ID and independently rotatable QUIC SPKI key.
- Operator-signed, versioned netmaps with anti-rollback marks distribute peer pins and role grants; rendezvous exchanges expiring IPv4/IPv6 reachability candidates, racing direct UDP paths and falling back to end-to-end encrypted `turn@1` / `CONNECT-UDP` (`RFC 9298`) relays.
- Mutual SPKI auth binds to service ALPNs with per-request workload authorization (`Place.Attach`, `Compute.Submit`, `Fleet.Deploy`). Offer an optional, separately granted **WireGuard** L3 compatibility tunnel for legacy IP tools.

---

## 9. L5a: Daily-Driver Desktop & Application Layer (Native Chromium + `figments-hirsel` on Flatland)

On a daily-driver workstation, **Zircon + Fuchsia's native graphics stack owns the GPU, display scanout, compositor, and physical input**, **Native Chromium (`WebEngine` / Ozone-Flatland, Chrome 157+)** runs directly as hardened Zircon Jobs, and **`figments-hirsel` IS the desktop shell and application layer**—requiring **zero Linux VMs** on the core workstation boot path.

### 9.1 Native Host Compositor, Graphics & Exact Chromium FIDL Surface
Rather than running a Linux display VM or pulling in all of Fuchsia userspace, `hirsel-init` hosts a **bounded Fuchsia graphics & web realm** exposing the exact, finite FIDL surface verified in [`third-party/fuchsia/src/chromium/web_engine/meta/shards/`](third-party/fuchsia/src/chromium/web_engine/meta/shards):

| Capability Group | Exact FIDL Protocols Consumed by Native Chromium (`web_instance`) | Provider in Hirsel-OS |
|---|---|---|
| **Display & Scene Graph** | `fuchsia.ui.composition.Flatland`, `fuchsia.ui.composition.Allocator`, `fuchsia.accessibility.semantics.SemanticsManager` | Bounded Fuchsia `Scenic/Flatland` + `display-coordinator` (`intel-display` / `virtio-gpu-display` / `framebuffer-*`) |
| **GPU & Buffers** | `fuchsia.sysmem2.Allocator`, `fuchsia.sysmem.Allocator`, `fuchsia.vulkan.loader.Loader` | Bounded Fuchsia `sysmem` + `Magma` (`msd-intel-gen` / `msd-arm-mali` / `msd-virtio-gpu`) + Mesa Vulkan ICD |
| **Input** | `fuchsia.ui.input3.Keyboard` + Flatland pointer/touch/focus | `hirsel-seatd` (native i8042 / xHCI-HID / I2C-HID) |
| **Network (Per-Context Attenuated!)** | `fuchsia.posix.socket.Provider`, `fuchsia.net.name.Lookup`, `fuchsia.net.interfaces.State` | Thin FIDL adapter on **`hirsel-netd`**—giving each Chromium `web_instance` its own attenuated `NET-11`/`NET-18`/`TLS-3` policy! |
| **Sandbox & Process Launch** | `fuchsia.process.Launcher`, `fuchsia.kernel.VmexResource` (scoped strictly to V8 JIT children), `fuchsia.scheduler.RoleManager` | `hirsel-init` (spawns Chromium renderer/GPU/utility processes directly as policy-locked Zircon Jobs—zero Linux namespaces/seccomp) |
| **Audio, Media & Fonts** | `fuchsia.media.Audio`, `fuchsia.media.AudioDeviceEnumerator`, `fuchsia.mediacodec.CodecFactory` (optional HW decode), `fuchsia.fonts.Provider`, `fuchsia.intl.PropertyProvider`, `fuchsia.memorypressure.Provider`, `fuchsia.buildinfo.Provider`, `fuchsia.logger.LogSink` | `hirsel-gfx-realm` audio/font providers (shipping Host Grotesk + Fragment Mono + system fallbacks) |

### 9.2 Mapping `figments-hirsel`, Isolated Browsers & Legacy Linux Apps
| Component | v1 (100% Native Zircon Desktop + On-Demand Linux) | v2 (Pure-Hirsel Consolidation) |
|---|---|---|
| **GPU, Display & Root Compositor** | **Native Zircon/Fuchsia host** (`sysmem` + `display-coordinator` + `Magma` + `Flatland`). Zero Linux GPU VM. | Same + additional native Magma MSDs (`msd-amdgpu`, newer Intel Xe2+). |
| **Hirsel Places** (`control-place`, `board/*`, `integrations-place`, `media` SFU/TURN) | **Native Tier-1 Zircon Jobs** directly on `hirsel-stored` + `hirsel-netd` (once `hirsel-contain` allows outbound `Net` for `integrations-place`). | Same (with `ClaimAuth`/`ORIGIN` Fleet mobility). |
| **Code Mode (`codemode/` `js@1` VM)** | **Native Tier-1 Policy-Locked Zircon Job** (replacing Linux `prlimit` with kernel handle confinement + `bound bytes`/`steps`). | Same. |
| **Figments Spatial Shell (`*.boards` + `*.canvas`)** | **Native Chromium (Chrome 157+ Ozone-Flatland `web_instance`)** running directly on Zircon—full HTML-in-Canvas, Shadow DOM, WebRTC/RNNoise, `pdf.js`, and `outer pass`, with `hirsel-netd` scoping its network strictly to local Places + allowed origins. (During bringup, the 3 thin Node 22 shims `control`/`router`/`mcp` run in a tiny headless `vm-edge` until ported.) | Port `control`, `router`, and `mcp` into pure-Hirsel Tier-1 services so zero Node.js or Linux is involved in running `figments-hirsel`; optional native Flatland/Hirsel-UI OS overlays. |
| **General Web Browsing (`web-personal`, `web-untrusted`)** | **Separate Native Chromium `web_instance` Zircon Job Trees**—each with its own `fuchsia.web.Context`, isolated `hirsel-stored` profile dir, distinct Flatland `ViewCreationToken` (with host-drawn trust border), and independent `hirsel-netd` grant. Zero Linux VM overhead per browser profile! | Same. |
| **Legacy Linux Daily-Driver Apps** (IDE, git/cargo terminal, Docker, CAD) | On-demand labeled Tier-2 App VM (`vm-dev`) presenting windows into **Flatland** via Wayland/Sommelier + host-Vulkan-backed `virtio-gpu` (following [src/virtualization/bin/vmm/device/virtio_gpu](third-party/fuchsia/src/virtualization/bin/vmm/device/virtio_gpu)). | Full host-backed Vulkan/Venus + hardware video decode via `sysmem` NV12/P010 buffer collections. |

### 9.3 Rethinking Desktop Metaphors (Genera / Oberon / Acme / Malleable Software)
- **File Manager → Spatial Boards & Artifact Explorer:** Stable object IDs, versions, `⌘K` BM25 search across notes/docs/tasks/Talk, and **CapDesk/Polaris powerboxes**—dragging or pasting a file through the trusted shell (extending `figments-hirsel`'s `paste_files` / `paste_text` bridge with 24-file / 100 MB caps) mints a single-use, attenuated grant to the exact focused target rather than granting ambient directory access.
- **Apps & Windows → Live Figments + Semantic Zoom + App Mode:** Every figment pairs a `store@1` Hirsel module (`board/apps/*.hirsel`) with a confined UI layer, rendering at three semantic zoom tiers (`live ≥ 20%`, `summary 8–20%`, `minimap < 8%`), promoting to full-viewport **App Mode** (`/f/<id>`), or detaching into standalone Flatland windows—side by side with labeled Tier-2 Linux windows.
- **Comms & Notifications → Built-In Durable Talk & Activity Inbox:** Durable channels, @mentions, rich previews, and pure-Hirsel `webrtc_sfu@1` voice/video calls live natively in the shell, while agent proposals and high-stakes approvals route to a compositor-owned Activity/Ceremony overlay.
- **Terminal → Typed Task Workbench + Labeled `vm-dev`:** Everyday OS and board operations use typed `figments call` / `execute` with receipts and budgets; POSIX shell workflows explicitly open inside `vm-dev` with a Flatland-enforced trust label.

### 9.4 Hardware-Enforced Trusted Path, Secure Attention & Peripherals
Because the **Zircon host owns BOTH physical input (`hirsel-seatd`) AND physical display scanout (`display-coordinator` + `Flatland`)**, the single-GPU trusted-path paradox is **solved cleanly** (Genode Nitpicker + EROS Trusted Window System + Qubes guarantees without Qubes' unaccelerated-GPU trade-off):
1. **Unforgeable Input & Secure Attention (`hirsel-seatd`):** Native Zircon drivers (i8042 / USB-HID / I2C-HID) feed input straight to the root Flatland/seat policy. No VM, Chromium renderer, or agent can forge physical keypresses or intercept the Secure Attention Sequence (SAS).
2. **Unforgeable Pixels & Approval Ceremonies (`Flatland` Root Layer):** The root Zircon compositor owns the topmost Flatland transform and scanout planes. Every window border, trust label (domain name + icon + accent derived from the host `ViewCreationToken` / VMM endpoint), capture/mic indicator, and **Pending Tool Completion approval ceremony** is rendered from **compositor-owned memory** above all client and VM viewports—even in fullscreen. No compromised VM or browser tab can spoof, obscure, or screenshot an approval surface (`screen_capture2` excludes protected layers before export).
3. **Private Clipboards, Capture Broker & Audio:** Each trust domain has a private clipboard; cross-domain copy/paste requires an explicit physical gesture and transfers one bounded, provenance-tagged MIME payload. Camera, mic, and screen capture require explicit per-target user selection and bounded duration. Audio routes via `virtio-snd` / host PCM into Chromium's WebRTC/RNNoise path in v1, graduating to a PipeWire-style real-time VMO ring graph when native audio apps demand it.

---

## 10. L5b: Built-In Agents, Pragmatic Security & Selective Staging

Synthesizing **CaMeL (2025)**, **FIDES (2025)**, **Progent (2025)**, **MiniScope (2025)**, **Conseca (HotOS '25)**, **Kairos / Cordon / Agent libOS (2026)**, and [`figments-hirsel`](/workspace/figments-hirsel/docs/ARCHITECTURE.md) after adversarial review, Hirsel-OS rejects both naive ambient "LLM kernels" (AIOS / unrestricted computer-use) *and* rigid universal Dual-LLM/DIFC that destroys coding utility (*AgentDyn*, arXiv `2602.03117`).

### 10.1 OS-Native Agent Primitives (`hirsel-brokerd` + `model-broker`)
1. **Named Connections → Kernel-Enforced Caretaker Membranes:** Elevate `figments-hirsel`'s scoped MCP connections (`control.mcp_connections`, `control.mcp_grants`) into OS-level **caretaker membranes** (Redell 1974 / Miller's E) inside `hirsel-brokerd`. Because Zircon lacks native handle revocation, every agent channel terminates in a broker forwarder intersecting `live_role ∩ connection_policy ∩ expiry`. Narrowing or revoking a connection kills active authority immediately.
2. **Content-Hashed Tool Snapshots:** Pin every tool schema, `SKILL.md` guide, and effect class (`reversible | compensable | irreversible`) by SHA-256 at Run start (`Run Tool Snapshot`). Treat third-party MCP tool descriptions as low-integrity text (normalized before entering planner context), defeating tool poisoning and rug pulls.
3. **Effect-Tiered Execution (Matching Security Mechanism to Task Shape):**
   - **Tier A — Open-Ended Coding, Research & Canvas Authoring:** Let the agent read raw source files, compiler diagnostics, docs, and canvas state directly inside a **private `store.fork` draft or confined `vm-dev` sandbox** (preserving full feedback-loop utility), while mechanically enforcing Meta's *Rule of Two* at the OS boundary: a Run combining untrusted input and private data holds **zero unmediated external-write or raw-egress capabilities**.
   - **Tier B — Narrow, Consequential Data-Driven Workflows (Invoices, Email/Slack Sends, Webhooks, Deploys):** Opt into **CaMeL/FIDES-style P-Job / Q-Job splitting**: ingress triggers attach origin/integrity metadata; a constructor-certified, zero-egress Tier-1 **Q-Job** extracts a strict typed schema (re-validated by `hirsel-brokerd`); deterministic policy checks recipient, amount, and revision before admitting high-integrity sinks.
   - **Tier C — Selective Effect Staging & Hash-Bound Ceremonies (`Kairos` / `Cordon` / Figments Receipts):** Stage reversible Place mutations in a `store.fork` branch and stage irreversible external effects in a durable **Effect Outbox** (`prepare → approve → dispatch → settle`, explicitly recording `unknown` delivery on timeout—matching `figments-hirsel`'s `codemode` receipts). Approval ceremonies render strictly from **broker-owned facts** (canonical destination, normalized arguments, structural diff, data provenance, and incomparable minimal scope alternatives per arXiv `2610.31914`—never LLM prose) and mint a **single-use capability bound to `H(call ‖ snapshot ‖ diff ‖ base_rev)`**. Material base-state conflict invalidates stale approvals instead of blindly replaying stale LLM outputs.
4. **Unified `Bank` Resource Capability (EROS Space Bank):** One hierarchical, attenuable budget per Agent Run covering `bound bytes`, `bound steps`, CPU deadline profile, model tokens, API dollars (metered at `model-broker` and `egress-gw`), egress bytes, and irreversible-effect count. Destroying a sub-agent's `Bank` reclaims all sub-resources.
5. **Model-as-OS-Service (`model-broker`):** Treats inference endpoints as **clearance-labeled sinks** (`local-gpu` for private/secret data; hosted vendor APIs only for explicitly permitted clearances), manages KV-cache memory/scheduling (vLLM PagedAttention model), and logs every model response at the `provide.*` replay seam for crash recovery, audit, and counterfactual policy regression testing.
6. **Proper Role of `std.model` & `hirsel.laws`:** Use `std.model` and `hirsel.laws` to continuously model-check and lock-verify `hirsel-brokerd`, caretaker revocation races, Effect Outbox crash/retry transitions, and approval state machines (not dynamic LLM-generated plans).

---

## 11. Unified Personal-to-Server Continuum

A daily-driver laptop and a Fleet of bare-metal servers run the **same Hirsel-OS** with explicit, honest consistency contracts (no fake CRDTs over single-writer `store@1` Places):

| Workflow | Consistency & Execution Contract |
|---|---|
| **Offline Personal Work** | **Local-primary Places:** Personal boards and agent Places hold their `store@1` write claim locally on NVMe and checkpoint immutable packs to S3/Fleet when connectivity returns. |
| **Offline Team Collaboration** | **Remote-primary Places:** Team boards expose a verified local pack cache (read-only) plus Replicache/Linear-style **optimistic drafts and idempotent intent queues** (extending `shell/canvas/native.ts`); conflicts surface as typed refusals on reconnect. |
| **Heavy Builds, Tests & Inference (`Archetype C`)** | Laptop dispatches `(release_id, entry_name, codec_args, bounds)` + input digests over SPKI-pinned `quic@1` to bare-metal Fleet servers—offloading ~2.1 GiB `hirsel serve` builds, `@test` suites, local-model inference, and untrusted Tier-2 Linux builds transparently. |
| **Moving a Running Place (`Archetype A`)** | Once `ClaimAuth`, `ORIGIN`, and `Deposed` land in `store@1`/`runner@1` ([fleet.md:27](/workspace/hirsel/docs/spec/fleet.md:27)), pre-warm target from immutable packs, persist a move permit, drain at a transaction boundary, win conditioned head CAS, and park the old incumbent on `Deposed`. Works across x86 ↔ ARM64. |
| **Live Calls & Media (`Archetype B`)** | `media/` (`webrtc_sfu@1` + `turn@1`) scales under dedicated `Directory[K]` Places with suspend-inclusive `CLOCK_BOOT` leases so laptop sleep/wake never produces split-brain media publishers. |

---

## 12. Adversarial Review Log & Explicit Non-Goals ("Things Not To Do")

Preserved from our red-team reviews so future milestones do not regress into dead ends:

- **Do NOT assume x86 DMA isolation exists in Zircon today** — Intel VT-d was removed upstream (Mar 2026) and AMD-Vi does not exist yet; gate all x86 driver VMs on real IOMMU + IRTE + reset verification.
- **Do NOT build a global POSIX VFS or a second CoW journal beneath `store@1`** — let `store@1` own Place transactions and let Linux VMs own `ext4`/`btrfs` on CoW `virtio-blk`.
- **Do NOT hand the physical GPU to a Linux MicroVM or attempt to run `figments-hirsel` on Blitz/Servo/Vello** — host the GPU/display natively via Fuchsia `sysmem` + `display-coordinator` + `Magma` + `Flatland`, and run **Native Chromium (`WebEngine` / Ozone-Flatland, Chrome 157+)** directly on Zircon.
- **Do NOT pull in all of Fuchsia userspace** — scope Fuchsia components strictly to the graphics/web realm (`sysmem`, `display`, `Magma`, `Flatland`, and the exact `web_instance` FIDL surface); keep `hirsel-stored`, `hirsel-netd`, `hirsel-brokerd`, and Hirsel Places minimal and Hirsel/Rust-native.
- **Do NOT fork Hirsel's network grants (`NET-11`/`NET-18`/`TLS-3`) or pull in Iroh/`quinn`** — extend Hirsel's existing grant grammar and sans-IO SPKI `quic@1` + `turn@1`.
- **Do NOT MITM all VM TLS traffic** — intercept only allowlisted brokered origins; use minted tokens, vsock `ssh-agent`, and plain `CONNECT` for everything else.
- **Do NOT force CaMeL P/Q splitting or universal speculative rollback on every agent task** — use effect-tiered execution (Tier A/B/C) so coding/research/canvas agents keep full utility.
- **Do NOT misuse `hirsel.laws` as a runtime LLM plan prover** — use it to verify OS brokers, outboxes, and state machines.
- **Do NOT pin/unpin DMA memory per I/O, seal TPM keys to raw PCR 11 values, build an AIOS-style "LLM kernel", or attempt CVM host mode before CVM guest mode.**

---

## 13. Phased Bare-Metal Bringup Roadmap & Acceptance Gates

### Gate 1: Headless Bare-Metal Server (First Real Iron, No Passthrough)
- Implement `hirsel-hal` + `hirsel-pcid` (ECAM/MSI-X) + native Ring-3 **NVMe** + **1 wired NIC** (`igc`/`ixgbe`/`ENA`) + **TPM 2.0** + `hirsel-boot.efi` (A/B/R ZKI + PCR 11 `PolicyAuthorize`).
- Extract **`hirsel-netd`** from `hirsel-init`, wire the Tier-1 `Net` broker (`NET-11`/`NET-18`/`TLS-3`), and qualify dual-stack IPv4/IPv6 on `smoltcp`.
- Replace `HRSLFS01` with crash-tested NVMe backing for `store@1`; fix `hirsel-contain` outbound `Net`; run `control-place`, `board/*`, `integrations-place`, `media`, and `codemode` (`js@1`) natively as Tier-1 Jobs on bare metal.
- **Pass criterion:** Cold bare-metal UEFI boot → A/B upgrade + automatic rollback test → power-cut durability test on `store@1` → live remote `figments-hirsel` board served from Tier-1 Places.

### Gate 2: Hardware DMA Isolation (P0 Kernel Gate)
- Land either **ARM64 `ArmSmmuV2`/`v3` + PCI requester-ID stream routing** (fastest DMA-isolated bringup) OR **Rust AMD-Vi (and/or Intel VT-d) + Interrupt Remapping + guest-bound BTIs + FLR/SBR teardown** in Zircon.
- **Pass criterion:** Hostile DMA fault injection from a passthrough device is trapped by the IOMMU; unmapped MSI injection is blocked by IRTE; VM kill cleanly quells DMA and resets the device without leaking host pages.

### Gate 3: Daily-Driver Workstation Seat (Intel Gen9–12 / ARM Mali / QEMU)
- Bring up the bounded Fuchsia native graphics & web realm (**`sysmem` + `display-coordinator` + `intel-display`/`virtio-gpu-display` + `Magma` `msd-intel-gen`/`msd-virtio-gpu` + `Flatland/Scenic`** + `fuchsia.media.Audio` / `fuchsia.fonts.Provider` + `hirsel-netd` FIDL socket/DNS adapter) alongside `hirsel-seatd` (native i8042 / xHCI-HID / I2C-HID + Secure Attention chord + pre-boot UEFI vault PIN) and isolated `uACPI`.
- Boot **Native Chromium (`WebEngine` / Ozone-Flatland, Chrome 157+ with HTML-in-Canvas)** directly as Zircon Jobs hosting `figments-hirsel` (`figments-shell` context) and isolated browsing contexts (`web-personal`, `web-untrusted`), plus optional `drv-net` (Wi-Fi), `drv-media`, and on-demand `vm-dev` (CoW `virtio-blk` + vsock `ssh-agent` + host-backed `virtio-gpu` into Flatland).
- Ship `hirsel-brokerd` (caretaker membranes, Effect Outbox, Flatland-protected hash-bound ceremonies, `Bank`), `egress-gw` (allowlisted `Cred` injection + spend metering), and `model-broker` (clearance routing).
- **Pass criterion:** Native Zircon/Flatland compositor owns 100% of display scanout and physical input; **zero Linux VM required to boot and run `figments-hirsel`**; native Chromium spatial canvas + native Tier-1 Places + on-demand `vm-dev` run side by side on bare metal; drag-and-drop powerbox transfer works; approval ceremonies render on a protected Flatland layer unspoofable by any renderer or VM; agents execute Tier A/B/C tasks with zero raw API keys in guest RAM and verifiable receipts/tapes.

### Gate 4: Server–Workstation Continuum & Pure-Hirsel Consolidation
- Land `ClaimAuth`/`ORIGIN`/`Deposed` + SPKI `quic@1` mesh for laptop ↔ server `Archetype C` compute offload and `Archetype A` Place handoff.
- Port `figments-hirsel`'s remaining Node shims (`control`, `router`, `mcp`) into native Tier-1 Hirsel services.
- Graduate `hirsel-netd` to bounded `netstack3_core` for IPv6/conntrack; add CVM guest support (SNP/TDX); execute the `PowerLease` + laptop S0ix residency campaign last.

---

## 14. Target Workspace Crate & Service Map

```text
/workspace/hirsel-os/
├── ARCHITECTURE.md            # This converged bare-metal blueprint
├── crates/
│   ├── hirsel-boot/           # UEFI Rust bootloader (ZKI verify, A/B/R, PCR 11, pre-boot PIN)
│   ├── hirsel-hal/            # Audited unsafe HAL (MMIO, static DMA pools, IRQ, ring@1)
│   ├── hirsel-pcid/           # Ring-3 PCIe ECAM, ACS/IOMMU groups, reset domains, bind rules
│   ├── hirsel-nvmed/          # Native Ring-3 NVMe driver (safe Rust over hirsel-hal)
│   ├── hirsel-tpmd/           # Native Ring-3 TPM 2.0 CRB/TIS + salted sessions
│   ├── hirsel-seatd/          # Native GOP, i8042/xHCI-HID/I2C-HID, SAS, Flatland root shell & powerbox
│   ├── hirsel-gfx-realm/      # Bounded Fuchsia GFX/Web host (sysmem, display, Magma, Flatland, WebEngine FIDL)
│   ├── hirsel-acpid/          # Isolated uACPI + PowerLease daemon
│   ├── hirsel-stored/         # NVMe store@1 backing, SHA-256 Blob/VMO CAS, CoW HRSLVOL/virtio-fs
│   ├── hirsel-netd/           # Isolated dual-stack IPv4/IPv6 (smoltcp -> netstack3_core) + quic@1
│   ├── hirsel-egress-gw/      # Allowlisted Cred proxy, spend ledger, vsock ssh-agent
│   ├── hirsel-brokerd/        # Caretaker membranes, Effect Outbox, ceremonies, Bank
│   ├── hirsel-model-broker/   # Clearance-labeled LLM sink router & KV-cache scheduler
│   ├── hirsel-init/           # Minimal root supervisor & confinement-cert launcher (existing)
│   ├── hirsel-vmm/            # Tier 2 SMP hypervisor VMM + CoW snapshot/fork (existing)
│   ├── hirsel-virtio/         # Ring-3 virtio + ring@1 shared-VMO queues (existing)
│   ├── hirsel-proto/          # AEAD RPC & wire codecs (existing)
│   ├── hirsel-os-ctl/         # Operator CLI (existing)
│   ├── zx-sys/ & zbi/         # Zircon vDSO bindings & ZBI/ZKI packer (existing)
└── patches/                   # Upstreamable Zircon patches (AMD SVM, AMD-Vi/SMMUv3, Job caps)
```

---

## 15. Selected Verified References

- **Microkernels, Drivers & Isolation:** LionsOS / seL4 sDDF (Heiser et al., arXiv `2501.06234`, 2025) · Asterinas Framekernel (Peng et al., USENIX ATC 2025) · NOVA (Steinberg & Kauer, EuroSys 2010) · Markuze et al., *True IOMMU Protection* (ASPLOS 2016) · Thunderclap (Markettos et al., NDSS 2019) · Firecracker (Agache et al., NSDI 2020) · EROS Space Banks & Constructors (Shapiro et al., SOSP 1999).
- **Storage & Durability:** Pillai et al., *All File Systems Are Not Created Equal* (OSDI 2014) · Aghayev et al., *File Systems Unfit as Distributed Storage Backends* (SOSP 2019) · Fuchsia RFC-0136 (Fxfs) & RFC-0225 (Fxblob) · Rethink the Sync / External Synchrony (Nightingale et al., OSDI 2006) · Crotty, Leis & Pavlo, *Are You Sure You Want to Use MMAP in Your DBMS?* (CIDR 2022).
- **Networking & Distributed Systems:** Fuchsia Netstack3 `CORE_BINDINGS` · Demikernel (Zhang et al., SOSP 2021) · Snap (Marty et al., SOSP 2019) · Capsicum (Watson et al., USENIX Security 2010) · Macaroons (Birgisson et al., NDSS 2014) · WireGuard (Donenfeld, NDSS 2017) · Plan 9 (Pike et al., 1995) · Invariant Confluence (Bailis et al., PVLDB 2015) · RFC 8305 (Happy Eyeballs) · RFC 9334 (RATS) · RFC 9849 (ECH) · RFC 9915 (DHCPv6).
- **Desktop, Malleable UI & Trusted Path:** Nitpicker (Feske & Helmuth, ACSAC 2005) · EROS Trusted Window System (Shapiro et al., USENIX Security 2004) · Qubes OS GUI Domain · Yee, *User Interaction Design for Secure Systems* (2002) / Polaris · Morphic (Maloney & Smith, UIST 1995) · Local-First Software (Kleppmann et al., Onward! 2019) · Malleable Software & Patchwork (Ink & Switch, 2024–2026) · DynaVis (CHI 2024) · Jelly (CHI 2025) · Cocoa (CHI 2026).
- **Agent Security & IFC:** Flume (Krohn et al., SOSP 2007) · HiStar (Zeldovich et al., OSDI 2006) · Laminar (Roy et al., PLDI 2009) · CaMeL (Debenedetti et al., arXiv `2503.18813`, 2025) · FIDES (Costa et al., arXiv `2505.23643`, 2025) · Progent (Shi et al., arXiv `2504.11703`, 2025) · MiniScope (Zhu et al., arXiv `2512.11147`, 2025) · Conseca (Tsai & Bagdasarian, HotOS 2025) · AgentDyn (arXiv `2602.03117`, 2026) · Cordon (`2606.17573`), Kairos (`2610.18457`), Agent libOS (`2606.03895`), Incomparable Minima (`2610.31914`).
