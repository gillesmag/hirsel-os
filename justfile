set shell := ["bash", "-euo", "pipefail", "-c"]

root := justfile_directory()
fuchsia := root / "third-party/fuchsia"
out_dir := root / "out"

default:
    @just --list

# Fetch Zircon/Fuchsia source tree + prebuilt toolchains via jiri
bootstrap-zircon:
    bash "{{root}}/scripts/bootstrap-zircon.sh"

# Configure minimal bringup.x64 Zircon build
configure-zircon:
    cd "{{fuchsia}}" && PATH="{{fuchsia}}/.jiri_root/bin:$PATH" fx set bringup.x64

# Build Zircon kernel (with ktests), QEMU boot shim, and zbi host tool
build-zircon:
    cd "{{fuchsia}}" && PATH="{{fuchsia}}/.jiri_root/bin:$PATH" fx build \
      //zircon/kernel/image/eng \
      --toolchain=//zircon/kernel/phys:kernel.phys_x64 //zircon/kernel/arch/x86/phys/boot-shim:linux-x86-boot-shim \
      --host //zircon/tools/zbi

# Cross-compile hirsel-init for x86_64-unknown-fuchsia using Fuchsia's prebuilt Rust + Clang/LLD
build-init:
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    SYSROOT="${BUILD_DIR}/regenerator_outputs/fuchsia_platform_sysroot/sysroot"
    RUSTC="{{fuchsia}}/prebuilt/third_party/rust/linux-x64/bin/rustc"
    CLANG="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/clang"
    CARGO_BIN="{{fuchsia}}/prebuilt/third_party/rust/linux-x64/bin/cargo"
    RUSTC="$RUSTC" \
    CARGO_TARGET_X86_64_UNKNOWN_FUCHSIA_LINKER="$CLANG" \
    CARGO_TARGET_X86_64_UNKNOWN_FUCHSIA_RUSTFLAGS="-C link-arg=--target=x86_64-unknown-fuchsia -C link-arg=--sysroot=${SYSROOT} -C link-arg=-fuse-ld=lld -C link-arg=-L${SYSROOT}/lib -C link-arg=-L${BUILD_DIR}/user.basic_x64-shared -C link-arg=-L${BUILD_DIR}/user.libc_x64/link_stub -C link-arg=-L${BUILD_DIR}/x64-shared/link_stub -C link-arg=-L{{fuchsia}}/prebuilt/third_party/clang/linux-x64/lib/x86_64-unknown-fuchsia -C link-arg=-Wl,--no-as-needed -C link-arg=-lfdio -C link-arg=-Wl,--as-needed -C panic=abort" \
      "$CARGO_BIN" build --manifest-path "{{root}}/Cargo.toml" -p hirsel-init --target x86_64-unknown-fuchsia --release

# Cross-compile libhirsel_core_rt.a for x86_64-unknown-fuchsia
build-rt hirsel_dir="/workspace/hirsel":
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    SYSROOT="${BUILD_DIR}/regenerator_outputs/fuchsia_platform_sysroot/sysroot"
    RUSTC="{{fuchsia}}/prebuilt/third_party/rust/linux-x64/bin/rustc"
    CARGO_BIN="{{fuchsia}}/prebuilt/third_party/rust/linux-x64/bin/cargo"
    CLANG="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/clang"
    AR="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/llvm-ar"
    RUSTC="$RUSTC" \
    CC_x86_64_unknown_fuchsia="$CLANG" \
    AR_x86_64_unknown_fuchsia="$AR" \
    CFLAGS_x86_64_unknown_fuchsia="--target=x86_64-unknown-fuchsia --sysroot=${SYSROOT}" \
    CARGO_TARGET_X86_64_UNKNOWN_FUCHSIA_RUSTFLAGS="-C panic=abort" \
    CARGO_TARGET_DIR="{{root}}/target/hirsel-rt-fuchsia" \
      "$CARGO_BIN" build --manifest-path "{{hirsel_dir}}/Cargo.toml" \
        -p hirsel-core-rt --target x86_64-unknown-fuchsia --release

# Cross-compile examples/hello-fuchsia to a stripped native x86_64-unknown-fuchsia ELF
build-hello hirsel_dir="/workspace/hirsel":
    #!/usr/bin/env bash
    set -euo pipefail
    STRIP="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/llvm-strip"
    mkdir -p "{{out_dir}}"
    "{{hirsel_dir}}/target/debug/hirsel" build "{{root}}/examples/hello-fuchsia" \
      --target x86_64-unknown-fuchsia \
      --release --backend cranelift \
      --cc "{{root}}/scripts/fuchsia-clang" \
      --runtime "{{root}}/target/hirsel-rt-fuchsia/x86_64-unknown-fuchsia/release/libhirsel_core_rt.a" \
      --libraries "{{hirsel_dir}}/core" \
      --source-core --source-std \
      -o "{{out_dir}}/hirsel-hello"
    "$STRIP" --strip-all "{{out_dir}}/hirsel-hello"
    ls -lh "{{out_dir}}/hirsel-hello"

# Build freestanding 64-bit x86-64 Tier 2 MicroVM guest payload
build-guest:
    #!/usr/bin/env bash
    set -euo pipefail
    CLANG="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/clang"
    mkdir -p "{{out_dir}}"
    "$CLANG" \
      --target=x86_64-unknown-none-elf \
      -ffreestanding -nostdlib -fno-pie -no-pie -mno-red-zone -O2 -Wall -Wextra \
      -fuse-ld=lld \
      -Wl,-T,"{{root}}/examples/guest-microvm/guest.ld" \
      -Wl,--build-id=none \
      "{{root}}/examples/guest-microvm/guest.c" \
      -o "{{out_dir}}/hirsel-guest.elf"
    ls -lh "{{out_dir}}/hirsel-guest.elf"

# Build Ring-0 -> Ring-3 Linux x86-64 compatibility shim for Tier 2 MicroVMs
build-linux-shim:
    #!/usr/bin/env bash
    set -euo pipefail
    CLANG="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/clang"
    mkdir -p "{{out_dir}}"
    "$CLANG" \
      --target=x86_64-unknown-none-elf \
      -ffreestanding -nostdlib -fno-pie -no-pie -mno-red-zone -O2 -Wall -Wextra \
      -fuse-ld=lld \
      -Wl,-T,"{{root}}/examples/linux-shim/shim.ld" \
      -Wl,--build-id=none \
      "{{root}}/examples/linux-shim/entry.S" \
      "{{root}}/examples/linux-shim/shim.c" \
      -o "{{out_dir}}/hirsel-linux-shim.elf"
    ls -lh "{{out_dir}}/hirsel-linux-shim.elf"

# Build unmodified Linux x86-64 static ELF workload (runs natively on Linux AND in Hirsel-OS Tier 2!)
build-linux-workload:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p "{{out_dir}}"
    gcc -static -nostdlib -fno-builtin -no-pie -fno-pie -O2 -Wall -Wextra \
      -Wl,--build-id=none \
      "{{root}}/examples/linux-workload/hello_linux.c" \
      -o "{{out_dir}}/hello-linux"
    ls -lh "{{out_dir}}/hello-linux"

# Pack minimal Talos-style hirsel-os.zbi (Zircon kernel + userboot + hirsel-init + Tier 1 & Tier 2 workloads)
build-os: build-init build-guest build-linux-shim
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    CLANG_LIB="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/lib/x86_64-unknown-fuchsia"
    mkdir -p "{{out_dir}}"
    cat > "{{out_dir}}/bootfs.manifest" <<EOF
    bin/hirsel-init={{root}}/target/x86_64-unknown-fuchsia/release/hirsel-init
    lib/ld.so.1=${BUILD_DIR}/user.libc_x64/libc.so
    lib/libc.so=${BUILD_DIR}/user.libc_x64/libc.so
    lib/libfdio.so=${BUILD_DIR}/x64-shared/libfdio.so
    lib/libunwind.so.1=${CLANG_LIB}/libunwind.so.1
    EOF
    if [[ -f "{{out_dir}}/hirsel-hello" ]]; then
      echo "bin/hirsel-hello={{out_dir}}/hirsel-hello" >> "{{out_dir}}/bootfs.manifest"
    fi
    if [[ -f "{{out_dir}}/hirsel-guest.elf" ]]; then
      echo "bin/hirsel-guest.elf={{out_dir}}/hirsel-guest.elf" >> "{{out_dir}}/bootfs.manifest"
    fi
    if [[ -f "{{out_dir}}/hirsel-linux-shim.elf" ]]; then
      echo "bin/hirsel-linux-shim.elf={{out_dir}}/hirsel-linux-shim.elf" >> "{{out_dir}}/bootfs.manifest"
    fi
    "${BUILD_DIR}/host_x64/zbi" \
      -o "{{out_dir}}/hirsel-os.zbi" \
      "${BUILD_DIR}/kernel.eng.zbi" \
      --files "{{out_dir}}/bootfs.manifest" \
      --type=cmdline \
      --entry="userboot.next=bin/hirsel-init"
    ls -lh "{{out_dir}}/hirsel-os.zbi"

# Boot hirsel-os.zbi on Zircon in QEMU with virtio-net (TCP 4444), virtio-blk (out/hirsel-store.img), and COM2 fallback
boot-os extra_cmdline="":
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    DISK="{{out_dir}}/hirsel-store.img"
    mkdir -p "{{out_dir}}"
    if [[ ! -f "$DISK" ]]; then
      truncate -s 32M "$DISK"
    fi
    QEMU_ARGS=(
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -device virtio-rng-pci
      -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:4444-:4444"
      -device "virtio-net-pci,netdev=net0"
      -drive "file=${DISK},format=raw,if=none,id=blk0"
      -device "virtio-blk-pci,drive=blk0"
      -m 1024
      -smp 2
      -vga none
      -nographic
      -serial mon:stdio
      -serial "tcp:127.0.0.1:4445,server=on,wait=off"
      -kernel "${BUILD_DIR}/kernel.phys_x64/linux-x86-boot-shim.bin"
      -initrd "{{out_dir}}/hirsel-os.zbi"
      -append "kernel.serial=legacy kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true {{extra_cmdline}}"
    )
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
      exec "$QEMU" -enable-kvm -cpu "host,migratable=no,+invtsc" "${QEMU_ARGS[@]}"
    elif id -nG "$USER" | tr ' ' '\n' | grep -qx kvm; then
      exec sg kvm -c "$(printf '%q ' "$QEMU" -enable-kvm -cpu "host,migratable=no,+invtsc" "${QEMU_ARGS[@]}")"
    else
      exec "$QEMU" -cpu "Skylake-Client,+pdpe1gb,-check" "${QEMU_ARGS[@]}"
    fi

# Run a Zircon kernel unit test suite non-interactively under KVM
ktest suite="x86_hypervisor_guest":
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    LOG="{{out_dir}}/ktest-{{suite}}.log"
    rm -f "$LOG"
    CMD=(
      timeout -k 2s 15s "$QEMU"
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -device virtio-rng-pci
      -enable-kvm -cpu "host,migratable=no,+invtsc"
      -m 1024
      -smp 2
      -nic none
      -vga none
      -display none
      -serial "file:${LOG}"
      -monitor none
      -kernel "${BUILD_DIR}/kernel.phys_x64/linux-x86-boot-shim.bin"
      -initrd "{{out_dir}}/hirsel-os.zbi"
      -append "kernel.serial=legacy kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true kernel.shell=true kernel.shell.script=ut+{{suite}};graceful-shutdown"
    )
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
      "${CMD[@]}" || true
    else
      sg kvm -c "$(printf '%q ' "${CMD[@]}")" || true
    fi
    tr -d '\000' < "$LOG" | tail -n 60

# Run Tier 1 + Tier 2 end-to-end boot verification under KVM
test-tiers: test-tier1

# Run Tier 1 + Tier 2 end-to-end boot test under KVM and print hirsel-init + job + microVM output
test-tier1: build-os
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    LOG="{{out_dir}}/tier1-boot.log"
    rm -f "$LOG"
    CMD=(
      timeout -k 2s 4s "$QEMU"
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -device virtio-rng-pci
      -enable-kvm -cpu "host,migratable=no,+invtsc"
      -m 1024
      -smp 2
      -nic none
      -vga none
      -display none
      -serial "file:${LOG}"
      -monitor none
      -kernel "${BUILD_DIR}/kernel.phys_x64/linux-x86-boot-shim.bin"
      -initrd "{{out_dir}}/hirsel-os.zbi"
      -append "kernel.serial=legacy kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true"
    )
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
      "${CMD[@]}" || true
    else
      sg kvm -c "$(printf '%q ' "${CMD[@]}")" || true
    fi
    tr -d '\000' < "$LOG" | grep -E "hirsel-os|hirsel-job|hirsel-vm"

# Run host CLI against a live Hirsel-OS node (default: 127.0.0.1:4444)
ctl *args:
    cargo run -q -p hirsel-os-ctl -- {{args}}

# End-to-end Phase 2 test: boot live Zircon node, remotely compile + upload + run Tier 1 & Tier 2 workloads
test-api: build-os
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -q -p hirsel-os-ctl
    CTL="{{root}}/target/debug/hirsel-os-ctl"
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    LOG="{{out_dir}}/api-boot.log"
    SOCK="{{out_dir}}/hirsel-api.sock"
    rm -f "$LOG" "$SOCK"
    QEMU_ARGS=(
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -device virtio-rng-pci
      -enable-kvm -cpu "host,migratable=no,+invtsc"
      -m 1024
      -smp 2
      -nic none
      -vga none
      -display none
      -serial "file:${LOG}"
      -serial "unix:${SOCK},server=on,wait=off"
      -monitor none
      -kernel "${BUILD_DIR}/kernel.phys_x64/linux-x86-boot-shim.bin"
      -initrd "{{out_dir}}/hirsel-os.zbi"
      -append "kernel.serial=legacy kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true"
    )
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
      "$QEMU" "${QEMU_ARGS[@]}" &
    else
      sg kvm -c "$(printf '%q ' "$QEMU" "${QEMU_ARGS[@]}")" &
    fi
    QEMU_PID=$!
    trap 'kill "$QEMU_PID" 2>/dev/null || true; pkill -f "unix:${SOCK}" 2>/dev/null || true; rm -f "$SOCK"' EXIT

    for _ in $(seq 1 60); do
      if [[ -S "$SOCK" ]] && grep -qa "Control Plane API ready" "$LOG" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    grep -qa "Control Plane API ready" "$LOG" || { echo "Node failed to start API:"; tail -n 30 "$LOG"; exit 1; }

    echo "=== 1. ping ==="
    "$CTL" --unix "$SOCK" ping
    echo "=== 2. info (before remote deploys) ==="
    "$CTL" --unix "$SOCK" info
    echo "=== 3. remote compile + upload + Tier 1 run (examples/hello-remote, NOT in BOOTFS!) ==="
    "$CTL" --unix "$SOCK" run --tier 1 "{{root}}/examples/hello-remote"
    echo "=== 4. second Tier 1 run (content-addressed zero-copy VMO cache HIT!) ==="
    "$CTL" --unix "$SOCK" run --tier 1 "{{root}}/examples/hello-remote"
    echo "=== 5. remote upload + Tier 2 AMD SVM MicroVM run (out/hirsel-guest.elf) ==="
    "$CTL" --unix "$SOCK" run --tier 2 "{{out_dir}}/hirsel-guest.elf"
    echo "=== 6. info (after remote deploys) ==="
    "$CTL" --unix "$SOCK" info

# End-to-end Virtio-Net (smoltcp TCP) + Virtio-Blk persistence test across a full VM reboot!
test-net-blk: build-os
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -q -p hirsel-os-ctl
    CTL="{{root}}/target/debug/hirsel-os-ctl"
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    DISK="{{out_dir}}/test-net-blk.img"
    LOG1="{{out_dir}}/net-blk-boot1.log"
    LOG2="{{out_dir}}/net-blk-boot2.log"
    PORT=14444
    rm -f "$DISK" "$LOG1" "$LOG2"
    truncate -s 32M "$DISK"

    QEMU_PID=""
    stop_qemu() {
      if [[ -n "$QEMU_PID" ]]; then
        kill "$QEMU_PID" 2>/dev/null || true
      fi
      pkill -f "hostfwd=tcp:127.0.0.1:${PORT}-:4444" 2>/dev/null || true
      QEMU_PID=""
      sleep 0.2
    }
    trap stop_qemu EXIT

    start_qemu() {
      local log="$1"
      local entropy
      entropy="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
      local args=(
        -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
        -machine q35,smbios-entry-point-type=32,i8042=off
        -device isa-debug-exit,iobase=0xf4,iosize=0x04
        -device virtio-rng-pci
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${PORT}-:4444"
        -device "virtio-net-pci,netdev=net0"
        -drive "file=${DISK},format=raw,if=none,id=blk0"
        -device "virtio-blk-pci,drive=blk0"
        -enable-kvm -cpu "host,migratable=no,+invtsc"
        -m 1024
        -smp 2
        -vga none
        -display none
        -serial "file:${log}"
        -monitor none
        -kernel "${BUILD_DIR}/kernel.phys_x64/linux-x86-boot-shim.bin"
        -initrd "{{out_dir}}/hirsel-os.zbi"
        -append "kernel.serial=legacy kernel.entropy-mixin=${entropy} kernel.halt-on-panic=true"
      )
      if [[ -r /dev/kvm && -w /dev/kvm ]]; then
        "$QEMU" "${args[@]}" &
      else
        sg kvm -c "$(printf '%q ' "$QEMU" "${args[@]}")" &
      fi
      QEMU_PID=$!
      for _ in $(seq 1 80); do
        if grep -qa "Control Plane TCP API listening" "$log" 2>/dev/null; then
          return 0
        fi
        sleep 0.1
      done
      echo "Node failed to start TCP API:"
      tr -d '\000' < "$log" | tail -n 40
      exit 1
    }

    echo "================ BOOT #1 (fresh 32 MiB virtio-blk disk) ================"
    start_qemu "$LOG1"
    tr -d '\000' < "$LOG1" | grep -E "hirsel-stored|hirsel-netd" || true
    "$CTL" --addr "127.0.0.1:${PORT}" ping
    "$CTL" --addr "127.0.0.1:${PORT}" info
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 1 "{{root}}/examples/hello-remote"
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 2 "{{out_dir}}/hirsel-guest.elf"
    stop_qemu

    echo "================ BOOT #2 (COLD REBOOT on same virtio-blk disk!) ================"
    start_qemu "$LOG2"
    tr -d '\000' < "$LOG2" | grep -E "hirsel-stored|hirsel-netd" || true
    "$CTL" --addr "127.0.0.1:${PORT}" info
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 1 "{{root}}/examples/hello-remote"
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 2 "{{out_dir}}/hirsel-guest.elf"
    "$CTL" --addr "127.0.0.1:${PORT}" info

# Complete end-to-end verification under KVM:
# - Mutual auth + ChaCha20-Poly1305 AEAD (including wrong-key rejection)
# - Tier 1 Args + Env + persistent Zircon-channel Dir across runs & cold reboot
# - Tier 2 unmodified Linux x86-64 static ELF running in Ring 3 inside AMD SVM
# - A/B ZBI upgrade staging verified across cold reboot
test-all: build-rt build-os build-linux-workload
    #!/usr/bin/env bash
    set -euo pipefail
    cargo test --workspace
    cargo build -q -p hirsel-os-ctl
    CTL="{{root}}/target/debug/hirsel-os-ctl"
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    DISK="{{out_dir}}/test-all.img"
    LOG1="{{out_dir}}/test-all-boot1.log"
    LOG2="{{out_dir}}/test-all-boot2.log"
    PORT=14445
    rm -f "$DISK" "$LOG1" "$LOG2"
    truncate -s 32M "$DISK"

    QEMU_PID=""
    stop_qemu() {
      if [[ -n "$QEMU_PID" ]]; then
        kill "$QEMU_PID" 2>/dev/null || true
      fi
      pkill -f "hostfwd=tcp:127.0.0.1:${PORT}-:4444" 2>/dev/null || true
      QEMU_PID=""
      sleep 0.2
    }
    trap stop_qemu EXIT

    start_qemu() {
      local log="$1"
      local entropy
      entropy="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
      local args=(
        -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
        -machine q35,smbios-entry-point-type=32,i8042=off
        -device isa-debug-exit,iobase=0xf4,iosize=0x04
        -device virtio-rng-pci
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${PORT}-:4444"
        -device "virtio-net-pci,netdev=net0"
        -drive "file=${DISK},format=raw,if=none,id=blk0"
        -device "virtio-blk-pci,drive=blk0"
        -enable-kvm -cpu "host,migratable=no,+invtsc"
        -m 1024
        -smp 2
        -vga none
        -display none
        -serial "file:${log}"
        -monitor none
        -kernel "${BUILD_DIR}/kernel.phys_x64/linux-x86-boot-shim.bin"
        -initrd "{{out_dir}}/hirsel-os.zbi"
        -append "kernel.serial=legacy kernel.entropy-mixin=${entropy} kernel.halt-on-panic=true"
      )
      if [[ -r /dev/kvm && -w /dev/kvm ]]; then
        "$QEMU" "${args[@]}" &
      else
        sg kvm -c "$(printf '%q ' "$QEMU" "${args[@]}")" &
      fi
      QEMU_PID=$!
      for _ in $(seq 1 80); do
        if grep -qa "Control Plane TCP API listening" "$log" 2>/dev/null; then
          return 0
        fi
        sleep 0.1
      done
      echo "Node failed to start TCP API:"
      tr -d '\000' < "$log" | tail -n 40
      exit 1
    }

    echo "=== 0. Verify hello-linux runs natively on host Linux ==="
    "{{out_dir}}/hello-linux"

    echo "================ BOOT #1 (fresh 32 MiB virtio-blk disk) ================"
    start_qemu "$LOG1"

    echo "=== 1. Wrong cluster key MUST be rejected ==="
    if "$CTL" --addr "127.0.0.1:${PORT}" --key "wrong-secret" ping 2>&1; then
      echo "ERROR: wrong key was accepted!"
      exit 1
    else
      echo "PASS: wrong cluster key rejected!"
    fi

    echo "=== 2. Authenticated AEAD ping & info ==="
    "$CTL" --addr "127.0.0.1:${PORT}" ping
    "$CTL" --addr "127.0.0.1:${PORT}" info

    echo "=== 3. Tier 1 hello-caps (Args + Env + persistent Zircon-channel Dir, run #1) ==="
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 1 \
      --arg "alpha" --arg "zircon" \
      --env "HIRSEL_ROLE=worker-1" \
      --timeout-ms 5000 \
      --grant-dir \
      "{{root}}/examples/hello-caps"

    echo "=== 4. Tier 1 hello-caps (run #2: reads back persisted Dir file!) ==="
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 1 \
      --arg "beta" --arg "persisted" \
      --env "HIRSEL_ROLE=worker-2" \
      --timeout-ms 5000 \
      --grant-dir \
      "{{root}}/examples/hello-caps"

    echo "=== 5. Tier 2 UNMODIFIED Linux x86-64 static ELF in Ring 3 inside AMD SVM! ==="
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 2 --linux-elf "{{out_dir}}/hello-linux"

    echo "=== 6. List cached/persisted images ==="
    "$CTL" --addr "127.0.0.1:${PORT}" images

    echo "=== 7. Stream & verify A/B ZBI upgrade onto inactive slot ==="
    "$CTL" --addr "127.0.0.1:${PORT}" upgrade "{{out_dir}}/hirsel-os.zbi"
    "$CTL" --addr "127.0.0.1:${PORT}" upgrade-status
    stop_qemu

    echo "================ BOOT #2 (COLD REBOOT on same virtio-blk disk!) ================"
    start_qemu "$LOG2"
    tr -d '\000' < "$LOG2" | grep -E "hirsel-stored|hirsel-netd" || true

    echo "=== 8. Verify A/B upgrade status & images survived cold reboot ==="
    "$CTL" --addr "127.0.0.1:${PORT}" upgrade-status
    "$CTL" --addr "127.0.0.1:${PORT}" images

    echo "=== 9. Verify Tier 1 Dir file survived cold reboot (run #3, 0-byte cache HIT!) ==="
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 1 \
      --arg "post-reboot" \
      --env "HIRSEL_ROLE=survivor" \
      --grant-dir \
      "{{root}}/examples/hello-caps"

    echo "=== 10. Verify Tier 2 Linux static ELF cached 0-byte HIT after cold reboot! ==="
    "$CTL" --addr "127.0.0.1:${PORT}" run --tier 2 --linux-elf "{{out_dir}}/hello-linux"

# Run host Rust unit tests (zbi, zx-sys, hirsel-proto, hirsel-vmm, hirsel-virtio)
test:
    cargo test --workspace

# Inspect a ZBI container image
zbi-list image:
    cargo run -p zbi -- list "{{image}}"
