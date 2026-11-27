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

# Build minimal static Linux PID 1 (/init) and pack into a cpio initramfs (with /dev/console & /dev/null)
build-linux-initrd:
    #!/usr/bin/env bash
    set -euo pipefail
    STAGE="{{out_dir}}/linux/initrd-root"
    rm -rf "$STAGE"
    mkdir -p "$STAGE/dev" "$STAGE/proc"
    gcc -static -nostdlib -fno-builtin -no-pie -fno-pie -O2 -Wall -Wextra \
      -Wl,--build-id=none \
      "{{root}}/examples/linux-init/init.c" \
      -o "$STAGE/init"
    chmod 0755 "$STAGE/init"
    python3 - "$STAGE/init" "{{out_dir}}/linux/initramfs.cpio" <<'PY'
    import sys, stat
    init_bytes = open(sys.argv[1], "rb").read()
    out = bytearray()
    ino = 1
    def entry(name, mode, data=b"", rdev_maj=0, rdev_min=0):
        global ino
        ino += 1
        nb = name.encode() + b"\x00"
        hdr = (
            f"070701{ino:08x}{mode:08x}{0:08x}{0:08x}{1:08x}{0:08x}"
            f"{len(data):08x}{0:08x}{0:08x}{rdev_maj:08x}{rdev_min:08x}"
            f"{len(nb):08x}{0:08x}"
        ).encode()
        out.extend(hdr); out.extend(nb)
        while len(out) % 4: out.append(0)
        out.extend(data)
        while len(out) % 4: out.append(0)
    entry("dev", stat.S_IFDIR | 0o755)
    entry("proc", stat.S_IFDIR | 0o755)
    entry("dev/console", stat.S_IFCHR | 0o600, rdev_maj=5, rdev_min=1)
    entry("dev/null", stat.S_IFCHR | 0o666, rdev_maj=1, rdev_min=3)
    entry("init", stat.S_IFREG | 0o755, data=init_bytes)
    entry("TRAILER!!!", 0)
    while len(out) % 512: out.append(0)
    open(sys.argv[2], "wb").write(out)
    PY
    ls -lh "{{out_dir}}/linux/initramfs.cpio"

# Fetch and statically compile Dropbear SSH server & client (dropbear, dbclient/ssh, scp, dropbearkey, dropbearconvert)
build-dropbear:
    #!/usr/bin/env bash
    set -euo pipefail
    OUT="{{out_dir}}/linux"
    mkdir -p "$OUT"
    if [[ -x "$OUT/dropbear" && -x "$OUT/dropbearkey" && -x "$OUT/dbclient" && -x "$OUT/scp" && -x "$OUT/dropbearconvert" ]]; then
      ls -lh "$OUT/dropbear" "$OUT/dbclient" "$OUT/scp" "$OUT/dropbearkey"
      exit 0
    fi
    VER="2025.89"
    TAR="$OUT/dropbear-${VER}.tar.bz2"
    SRC="$OUT/dropbear-${VER}"
    if [[ ! -f "$TAR" ]]; then
      curl -fSL "https://matt.ucc.asn.au/dropbear/releases/dropbear-${VER}.tar.bz2" -o "$TAR"
    fi
    rm -rf "$SRC"
    tar -xjf "$TAR" -C "$OUT"
    (
      cd "$SRC"
      ./configure \
        --enable-static \
        --disable-zlib \
        --disable-lastlog \
        --disable-utmp \
        --disable-utmpx \
        --disable-wtmp \
        --disable-wtmpx \
        --disable-pututline \
        --disable-pututxline \
        --disable-pam \
        LDFLAGS="-static" \
        CFLAGS="-Os"
      make -j"$(nproc)" PROGRAMS="dropbear dropbearkey dbclient scp dropbearconvert" STATIC=1
      cp dropbear dropbearkey dbclient scp dropbearconvert "$OUT/"
    )
    strip "$OUT/dropbear" "$OUT/dropbearkey" "$OUT/dbclient" "$OUT/scp" "$OUT/dropbearconvert" || true
    ls -lh "$OUT/dropbear" "$OUT/dbclient" "$OUT/scp" "$OUT/dropbearkey"

# Build SSH-enabled BusyBox + Dropbear (server + ssh/scp client) Linux MicroVM initramfs
build-linux-ssh-initrd: build-dropbear
    #!/usr/bin/env bash
    set -euo pipefail
    OUT="{{out_dir}}/linux"
    mkdir -p "$OUT"
    if [[ ! -f "$OUT/id_ed25519" ]]; then
      ssh-keygen -t ed25519 -N "" -f "$OUT/id_ed25519" -C "hirsel-os-dev" >/dev/null
      chmod 600 "$OUT/id_ed25519"
    fi
    if [[ ! -f "$OUT/dropbear_ed25519_host_key" ]]; then
      "$OUT/dropbearkey" -t ed25519 -f "$OUT/dropbear_ed25519_host_key" >/dev/null
    fi
    AUTH_KEYS="$OUT/authorized_keys"
    cat "$OUT/id_ed25519.pub" > "$AUTH_KEYS"
    if [[ -f "$HOME/.ssh/id_ed25519.pub" ]]; then
      cat "$HOME/.ssh/id_ed25519.pub" >> "$AUTH_KEYS"
    fi
    if [[ -f "$HOME/.ssh/id_rsa.pub" ]]; then
      cat "$HOME/.ssh/id_rsa.pub" >> "$AUTH_KEYS"
    fi
    python3 - \
      "{{root}}/examples/linux-vm/init.sh" \
      "/usr/bin/busybox" \
      "$OUT/dropbear" \
      "$OUT/dbclient" \
      "$OUT/scp" \
      "$OUT/dropbearkey" \
      "$OUT/dropbearconvert" \
      "$OUT/dropbear_ed25519_host_key" \
      "$AUTH_KEYS" \
      "$OUT/initramfs-ssh.cpio" <<'PY'
    import sys, stat, subprocess

    init_sh = open(sys.argv[1], "rb").read()
    busybox = open(sys.argv[2], "rb").read()
    dropbear = open(sys.argv[3], "rb").read()
    dbclient = open(sys.argv[4], "rb").read()
    scp_bin = open(sys.argv[5], "rb").read()
    dropbearkey = open(sys.argv[6], "rb").read()
    dropbearconvert = open(sys.argv[7], "rb").read()
    host_key = open(sys.argv[8], "rb").read()
    auth_keys = open(sys.argv[9], "rb").read()
    out_path = sys.argv[10]

    applets = subprocess.check_output([sys.argv[2], "--list"], text=True).split()
    out = bytearray()
    ino = 1

    def entry(name, mode, data=b"", rdev_maj=0, rdev_min=0):
        global ino
        ino += 1
        nb = name.encode() + b"\x00"
        hdr = (
            f"070701{ino:08x}{mode:08x}{0:08x}{0:08x}{1:08x}{0:08x}"
            f"{len(data):08x}{0:08x}{0:08x}{rdev_maj:08x}{rdev_min:08x}"
            f"{len(nb):08x}{0:08x}"
        ).encode()
        out.extend(hdr); out.extend(nb)
        while len(out) % 4: out.append(0)
        out.extend(data)
        while len(out) % 4: out.append(0)

    for d in ["bin", "sbin", "usr", "usr/bin", "usr/sbin", "dev", "dev/pts", "proc", "sys",
              "etc", "etc/dropbear", "root", "root/.ssh", "tmp", "run", "var", "var/log"]:
        mode = stat.S_IFDIR | (0o700 if d.startswith("root") else 0o755)
        entry(d, mode)

    entry("dev/console", stat.S_IFCHR | 0o600, rdev_maj=5, rdev_min=1)
    entry("dev/null", stat.S_IFCHR | 0o666, rdev_maj=1, rdev_min=3)
    entry("dev/zero", stat.S_IFCHR | 0o666, rdev_maj=1, rdev_min=5)
    entry("dev/tty", stat.S_IFCHR | 0o666, rdev_maj=5, rdev_min=0)
    entry("dev/ptmx", stat.S_IFCHR | 0o666, rdev_maj=5, rdev_min=2)
    entry("dev/urandom", stat.S_IFCHR | 0o666, rdev_maj=1, rdev_min=9)

    entry("init", stat.S_IFREG | 0o755, data=init_sh)
    entry("bin/busybox", stat.S_IFREG | 0o755, data=busybox)
    for app in applets:
        if app in ("busybox", "init", "linuxrc", "[", "[["):
            continue
        entry(f"bin/{app}", stat.S_IFLNK | 0o777, data=b"busybox")
    entry("sbin/dropbear", stat.S_IFREG | 0o755, data=dropbear)
    entry("usr/bin/dbclient", stat.S_IFREG | 0o755, data=dbclient)
    entry("usr/bin/ssh", stat.S_IFLNK | 0o777, data=b"dbclient")
    entry("usr/bin/scp", stat.S_IFREG | 0o755, data=scp_bin)
    entry("usr/bin/dropbearkey", stat.S_IFREG | 0o755, data=dropbearkey)
    entry("usr/bin/dropbearconvert", stat.S_IFREG | 0o755, data=dropbearconvert)
    entry("etc/dropbear/dropbear_ed25519_host_key", stat.S_IFREG | 0o600, data=host_key)
    entry("etc/passwd", stat.S_IFREG | 0o644, data=b"root:x:0:0:root:/root:/bin/sh\n")
    entry("etc/group", stat.S_IFREG | 0o644, data=b"root:x:0:\n")
    entry("etc/shadow", stat.S_IFREG | 0o600, data=b"root:*:19000:0:99999:7:::\n")
    entry("etc/shells", stat.S_IFREG | 0o644, data=b"/bin/sh\n/bin/ash\n")
    entry("etc/nsswitch.conf", stat.S_IFREG | 0o644, data=b"passwd: files\ngroup: files\nhosts: files dns\n")
    entry("etc/hosts", stat.S_IFREG | 0o644, data=b"127.0.0.1 localhost\n")
    entry("etc/resolv.conf", stat.S_IFREG | 0o644, data=b"nameserver 10.0.2.3\nnameserver 1.1.1.1\n")
    entry("root/.ssh/authorized_keys", stat.S_IFREG | 0o600, data=auth_keys)
    entry("TRAILER!!!", 0)
    while len(out) % 512: out.append(0)
    open(out_path, "wb").write(out)
    PY
    ls -lh "$OUT/initramfs-ssh.cpio" "$OUT/id_ed25519"

# Fetch and strip Firecracker's upstream Linux 6.1.102 x86-64 vmlinux for Tier 2 Linux MicroVM tests
fetch-linux-kernel:
    #!/usr/bin/env bash
    set -euo pipefail
    STRIP="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/llvm-strip"
    mkdir -p "{{out_dir}}/linux"
    if [[ ! -f "{{out_dir}}/linux/vmlinux.stripped" ]]; then
      if [[ ! -f "{{out_dir}}/linux/vmlinux" ]]; then
        curl -fSL "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.11/x86_64/vmlinux-6.1.102" \
          -o "{{out_dir}}/linux/vmlinux"
      fi
      "$STRIP" --strip-all "{{out_dir}}/linux/vmlinux" -o "{{out_dir}}/linux/vmlinux.stripped"
    fi
    ls -lh "{{out_dir}}/linux/vmlinux.stripped"

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

# Boot hirsel-os.zbi on Zircon in QEMU with virtio-net (TCP 4444 + VM SSH 2222/2223), virtio-blk (64M), and COM2 fallback
boot-os extra_cmdline="":
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    DISK="{{out_dir}}/hirsel-store.img"
    mkdir -p "{{out_dir}}"
    if [[ ! -f "$DISK" ]]; then
      truncate -s 128M "$DISK"
    fi
    QEMU_ARGS=(
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -device virtio-rng-pci
      -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:4444-:4444,hostfwd=tcp:127.0.0.1:2222-:2222,hostfwd=tcp:127.0.0.1:2223-:2223"
      -device "virtio-net-pci,netdev=net0"
      -drive "file=${DISK},format=raw,if=none,id=blk0"
      -device "virtio-blk-pci,drive=blk0"
      -m 1024
      -smp 4
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
        if grep -qaE "DHCPv4 lease acquired|falling back to static" "$log" 2>/dev/null; then
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
        if grep -qaE "DHCPv4 lease acquired|falling back to static" "$log" 2>/dev/null; then
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

# Boot a REAL upstream Linux 6.1.102 kernel + initramfs inside Hirsel-OS Zircon AMD SVM Tier 2 MicroVM!
test-linux-vm: build-os build-linux-initrd fetch-linux-kernel
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -q -p hirsel-os-ctl
    CTL="{{root}}/target/debug/hirsel-os-ctl"
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    DISK="{{out_dir}}/test-linux-vm.img"
    LOG="{{out_dir}}/test-linux-vm.log"
    PORT=14446
    rm -f "$DISK" "$LOG"
    truncate -s 64M "$DISK"

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

    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    QEMU_ARGS=(
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
      -serial "file:${LOG}"
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
    for _ in $(seq 1 80); do
      if grep -qaE "DHCPv4 lease acquired|falling back to static" "$LOG" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    grep -qaE "DHCPv4 lease acquired|falling back to static" "$LOG" || { tr -d '\000' < "$LOG" | tail -n 40; exit 1; }

    echo "=== 1. Boot REAL Linux 6.1.102 kernel + initramfs inside Zircon AMD SVM ==="
    OUT1="$(
      "$CTL" --addr "127.0.0.1:${PORT}" run --tier 2 \
        --linux-kernel \
        --ram-mib 128 \
        --initrd "{{out_dir}}/linux/initramfs.cpio" \
        --cmdline "earlycon=uart,io,0x3f8,115200n8 console=ttyS0 reboot=k panic=1 pci=off nomodules" \
        "{{out_dir}}/linux/vmlinux.stripped"
    )"
    printf '%s\n' "$OUT1"
    grep -q "Linux version 6.1.102" <<<"$OUT1"
    grep -q "REAL LINUX KERNEL BOOTED TO PID 1!" <<<"$OUT1"
    grep -q "uname:     Linux 6.1.102 x86_64" <<<"$OUT1"
    grep -q "pid:       1" <<<"$OUT1"

    echo "=== 2. Second boot (0-byte VMO cache HIT for BOTH vmlinux and initramfs!) ==="
    OUT2="$(
      "$CTL" --addr "127.0.0.1:${PORT}" run --tier 2 \
        --linux-kernel \
        --ram-mib 128 \
        --initrd "{{out_dir}}/linux/initramfs.cpio" \
        --cmdline "console=ttyS0 reboot=k panic=1 pci=off nomodules quiet" \
        "{{out_dir}}/linux/vmlinux.stripped"
    )"
    printf '%s\n' "$OUT2"
    grep -q "REAL LINUX KERNEL BOOTED TO PID 1!" <<<"$OUT2"
    grep -q "uname:     Linux 6.1.102 x86_64" <<<"$OUT2"
    echo "=== PASS: Upstream Linux 6.1.102 microVM booted cleanly to PID 1 twice inside Zircon AMD SVM! ==="

# End-to-end KVM verification of ALL 5 production milestones:
# 1. Persistent Virtio-Blk VM volumes (/dev/vda) surviving VM restart AND node cold reboot
# 2. Multi-vCPU SMP Linux 6.1.102 MicroVMs (vcpus = 2, nproc == 2)
# 3. Declarative autostart reconciliation on cold boot + `node reboot` + A/B commit/rollback
# 4. Dynamic DHCPv4 + X25519 Perfect Forward Secrecy (PFS) AEAD control plane
# 5. Network-less `vm exec` over Virtio-Console (/dev/hvc0) on `--no-net` VM + inbound/outbound SSH/DNS/NAT
test-vm-ssh: build-os fetch-linux-kernel build-linux-ssh-initrd
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -q -p hirsel-os-ctl
    CTL="{{root}}/target/debug/hirsel-os-ctl"
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    DISK="{{out_dir}}/test-vm-ssh.img"
    LOG="{{out_dir}}/test-vm-ssh.log"
    CTL_PORT=14447
    SSH_PORT=12222
    rm -f "$DISK" "$LOG"
    truncate -s 128M "$DISK"

    QEMU_PID=""
    stop_qemu() {
      if [[ -n "$QEMU_PID" ]]; then
        kill "$QEMU_PID" 2>/dev/null || true
      fi
      pkill -f "hostfwd=tcp:127.0.0.1:${CTL_PORT}-:4444" 2>/dev/null || true
      QEMU_PID=""
      sleep 0.2
    }
    trap stop_qemu EXIT

    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    QEMU_ARGS=(
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -device virtio-rng-pci
      -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${CTL_PORT}-:4444,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:2222"
      -device "virtio-net-pci,netdev=net0"
      -drive "file=${DISK},format=raw,if=none,id=blk0"
      -device "virtio-blk-pci,drive=blk0"
      -enable-kvm -cpu "host,migratable=no,+invtsc"
      -m 1024
      -smp 4
      -vga none
      -display none
      -serial "file:${LOG}"
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
    for _ in $(seq 1 80); do
      if grep -qaE "DHCPv4 lease acquired|falling back to static" "$LOG" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    grep -qaE "DHCPv4 lease acquired|falling back to static" "$LOG" || { tr -d '\000' < "$LOG" | tail -n 40; exit 1; }

    echo "=== 1. Verify X25519 PFS AEAD + dynamic DHCPv4 lease ==="
    INFO_OUT="$("$CTL" --addr "127.0.0.1:${CTL_PORT}" info)"
    printf '%s\n' "$INFO_OUT"
    grep -q "pfs_x25519=true" <<<"$INFO_OUT"
    grep -q "dhcp=true" <<<"$INFO_OUT"

    echo "=== 2. Create persistent named VM volume (8 MiB on virtio-blk HRSLVOL1) ==="
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vol create data-1 8
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vol ls

    echo "=== 3. Start 2-vCPU SMP Linux 6.1.102 VM with persistent /dev/vda & autostart ==="
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm start \
      --spec "{{root}}/examples/linux-vm/vm.spec" \
      --disk data-1 \
      --autostart

    for _ in $(seq 1 100); do
      LOGS="$("$CTL" --addr "127.0.0.1:${CTL_PORT}" vm logs dev-1)"
      if grep -q "SSH daemon listening on 0.0.0.0:22" <<<"$LOGS"; then
        printf '%s\n' "$LOGS"
        break
      fi
      sleep 0.2
    done
    grep -q "cpus=2" <<<"$LOGS"
    grep -q "virtio-blk attached at /dev/vda" <<<"$LOGS"
    grep -q "virtio-console guest agent active on /dev/hvc0" <<<"$LOGS"

    echo "=== 4. Verify 2 vCPUs, /dev/vda persistent write, inbound SSH & outbound DNS/NAT/SSH ==="
    OUT_KEY_B64="$(base64 -w0 "{{out_dir}}/linux/id_ed25519")"
    SSH_OUT="$(
      ssh \
        -i "{{out_dir}}/linux/id_ed25519" \
        -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null \
        -o ConnectTimeout=10 \
        -o LogLevel=ERROR \
        -p "${SSH_PORT}" \
        root@127.0.0.1 \
        "echo '[ssh] CONNECTED TO:' \$(hostname) && \
         echo 'NPROC='\"\$(nproc)\" && \
         echo 'PERSIST_MARKER_42' > /mnt/data/hello.txt && sync && \
         cat /mnt/data/hello.txt && \
         nslookup example.com && \
         ping -c 1 -W 3 example.com && \
         echo '${OUT_KEY_B64}' | base64 -d > /tmp/id_openssh && \
         chmod 600 /tmp/id_openssh && \
         dropbearconvert openssh dropbear /tmp/id_openssh /tmp/id_dropbear && \
         ping -c 2 -W 2 10.0.2.2 && \
         ssh -y -i /tmp/id_dropbear -p ${SSH_PORT} root@10.0.2.2 'echo OUTBOUND_SSH_OK && nproc'"
    )"
    printf '%s\n' "$SSH_OUT"
    grep -q "CONNECTED TO: dev-1" <<<"$SSH_OUT"
    grep -q "NPROC=2" <<<"$SSH_OUT"
    grep -q "PERSIST_MARKER_42" <<<"$SSH_OUT"
    grep -q "Name:.*example.com" <<<"$SSH_OUT"
    grep -q "OUTBOUND_SSH_OK" <<<"$SSH_OUT"

    echo "=== 5. Verify network-less --no-net VM + virtio-console /dev/hvc0 vm exec! ==="
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm start \
      --name airgap-1 \
      --kernel "{{out_dir}}/linux/vmlinux.stripped" \
      --initrd "{{out_dir}}/linux/initramfs-ssh.cpio" \
      --vcpus 2 \
      --no-net
    for _ in $(seq 1 100); do
      AG_LOGS="$("$CTL" --addr "127.0.0.1:${CTL_PORT}" vm logs airgap-1)"
      if grep -q "virtio-console guest agent active on /dev/hvc0" <<<"$AG_LOGS"; then
        break
      fi
      sleep 0.2
    done
    EXEC_OUT="$("$CTL" --addr "127.0.0.1:${CTL_PORT}" vm exec airgap-1 -- "echo AIRGAP_EXEC_OK && uname -r && nproc && ip link show")"
    printf '%s\n' "$EXEC_OUT"
    grep -q "AIRGAP_EXEC_OK" <<<"$EXEC_OUT"
    grep -q "6.1.102" <<<"$EXEC_OUT"
    ! grep -q "eth0" <<<"$EXEC_OUT"
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm stop airgap-1
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm rm airgap-1

    echo "=== 6. Cold-reboot Hirsel-OS node via 'node reboot' & verify autostart + persistent /dev/vda! ==="
    BOOT1_COUNT="$(grep -ac "DHCPv4 lease acquired" "$LOG" || true)"
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" node reboot
    for _ in $(seq 1 120); do
      BOOT2_COUNT="$(grep -ac "DHCPv4 lease acquired" "$LOG" || true)"
      if [[ "$BOOT2_COUNT" -gt "$BOOT1_COUNT" ]]; then
        break
      fi
      sleep 0.2
    done
    [[ "$BOOT2_COUNT" -gt "$BOOT1_COUNT" ]] || { echo "Node did not complete cold reboot:"; tr -d '\000' < "$LOG" | tail -n 50; exit 1; }

    echo "=== 7. Wait for auto-reconciled dev-1 & verify /mnt/data/hello.txt survived cold reboot! ==="
    for _ in $(seq 1 100); do
      LOGS2="$("$CTL" --addr "127.0.0.1:${CTL_PORT}" vm logs dev-1 2>/dev/null || true)"
      if grep -q "SSH daemon listening on 0.0.0.0:22" <<<"$LOGS2"; then
        break
      fi
      sleep 0.2
    done
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm ls
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm inspect dev-1
    PERSIST_OUT="$(
      "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm exec dev-1 -- "cat /mnt/data/hello.txt && nproc"
    )"
    printf '%s\n' "$PERSIST_OUT"
    grep -q "PERSIST_MARKER_42" <<<"$PERSIST_OUT"
    grep -q "^2$" <<<"$PERSIST_OUT"

    echo "=== 8. Verify A/B upgrade commit & rollback + clean teardown ==="
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" upgrade "{{out_dir}}/hirsel-os.zbi"
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" upgrade-commit
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" upgrade-rollback
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm stop dev-1
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vm rm dev-1
    "$CTL" --addr "127.0.0.1:${CTL_PORT}" vol rm data-1
    echo "=== PASS: ALL 5 PRODUCTION MILESTONES VERIFIED END-TO-END UNDER KVM! ==="

# Run host Rust unit tests (zbi, zx-sys, hirsel-proto, hirsel-vmm, hirsel-virtio)
test:
    cargo test --workspace

# Run pure-Hirsel std.model.explore formal security verification for hirsel-wm, hirsel-board, hirsel-agent
test-wm hirsel_dir="/workspace/hirsel":
    #!/usr/bin/env bash
    set -euo pipefail
    for pkg in hirsel-wm hirsel-board hirsel-agent; do
      PATH="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin:$PATH" \
        "{{hirsel_dir}}/target/debug/hirsel" test "{{root}}/$pkg" \
          --libraries "{{hirsel_dir}}/core" --source-core --source-std \
          --cc /usr/bin/cc \
          --runtime "{{hirsel_dir}}/target/release/libhirsel_core_rt.a"
    done

# Cross-compile pure-Hirsel hirsel-wm to native x86_64-unknown-fuchsia ELF and stage into hirsel_session
build-wm hirsel_dir="/workspace/hirsel":
    #!/usr/bin/env bash
    set -euo pipefail
    STRIP="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/llvm-strip"
    mkdir -p "{{out_dir}}" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt"
    for pkg in hirsel-wm hirsel-board hirsel-agent; do
      PATH="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin:$PATH" \
        "{{hirsel_dir}}/target/debug/hirsel" build "{{root}}/$pkg" \
          --libraries "{{hirsel_dir}}/core" --source-core --source-std \
          --release --backend llvm --target x86_64-unknown-fuchsia \
          --cc "{{root}}/scripts/fuchsia-clang" \
          --runtime "{{root}}/target/hirsel-rt-fuchsia/x86_64-unknown-fuchsia/release/libhirsel_core_rt.a" \
          --grant Stdin --grant Stdout --limit 67108864 \
          -o "{{out_dir}}/$pkg"
      "$STRIP" --strip-all "{{out_dir}}/$pkg"
      cp "{{out_dir}}/$pkg" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt/$pkg"
      ls -lh "{{out_dir}}/$pkg"
    done

# Cross-compile pure-Hirsel Figments js@1 sandbox (hirsel-js) to native x86_64-unknown-fuchsia ELF
build-js hirsel_dir="/workspace/hirsel":
    #!/usr/bin/env bash
    set -euo pipefail
    STRIP="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/llvm-strip"
    mkdir -p "{{out_dir}}" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt"
    PATH="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin:$PATH" \
      "{{hirsel_dir}}/target/debug/hirsel" build "{{root}}/hirsel-js" \
        --libraries "{{hirsel_dir}}/core" --source-core --source-std \
        --release --backend llvm --target x86_64-unknown-fuchsia \
        --cc "{{root}}/scripts/fuchsia-clang" \
        --runtime "{{root}}/target/hirsel-rt-fuchsia/x86_64-unknown-fuchsia/release/libhirsel_core_rt.a" \
        --grant Stdin --grant Stdout --limit 134217728 \
        -o "{{out_dir}}/hirsel-js"
    "$STRIP" --strip-all "{{out_dir}}/hirsel-js"
    cp "{{out_dir}}/hirsel-js" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt/hirsel-js"
    ls -lh "{{out_dir}}/hirsel-js"

# Cross-compile Rust Tier-2 Linux 6.1.102 AMD-SVM MicroVM host (hirsel-vm-host) for x86_64-unknown-fuchsia
build-vm-host:
    #!/usr/bin/env bash
    set -euo pipefail
    BUILD_DIR="{{fuchsia}}/$(cat "{{fuchsia}}/.fx-build-dir")"
    SYSROOT="${BUILD_DIR}/regenerator_outputs/fuchsia_platform_sysroot/sysroot"
    RUSTC="{{fuchsia}}/prebuilt/third_party/rust/linux-x64/bin/rustc"
    CLANG="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/clang"
    STRIP="{{fuchsia}}/prebuilt/third_party/clang/linux-x64/bin/llvm-strip"
    CARGO_BIN="{{fuchsia}}/prebuilt/third_party/rust/linux-x64/bin/cargo"
    mkdir -p "{{out_dir}}" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt"
    RUSTC="$RUSTC" \
    CARGO_TARGET_X86_64_UNKNOWN_FUCHSIA_LINKER="$CLANG" \
    CARGO_TARGET_X86_64_UNKNOWN_FUCHSIA_RUSTFLAGS="-C link-arg=--target=x86_64-unknown-fuchsia -C link-arg=--sysroot=${SYSROOT} -C link-arg=-fuse-ld=lld -C link-arg=-L${SYSROOT}/lib -C link-arg=-L${BUILD_DIR}/user.basic_x64-shared -C link-arg=-L${BUILD_DIR}/user.libc_x64/link_stub -C link-arg=-L${BUILD_DIR}/x64-shared/link_stub -C link-arg=-L{{fuchsia}}/prebuilt/third_party/clang/linux-x64/lib/x86_64-unknown-fuchsia -C link-arg=-Wl,--no-as-needed -C link-arg=-lfdio -C link-arg=-Wl,--as-needed -C panic=abort" \
      "$CARGO_BIN" build --manifest-path "{{root}}/Cargo.toml" -p hirsel-vm-host --target x86_64-unknown-fuchsia --release
    "$STRIP" --strip-all "{{root}}/target/x86_64-unknown-fuchsia/release/hirsel-vm-host" -o "{{out_dir}}/hirsel-vm-host"
    cp "{{out_dir}}/hirsel-vm-host" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt/hirsel-vm-host"
    ls -lh "{{out_dir}}/hirsel-vm-host"

# Re-bundle real Figments canvas engine (A* elbow router + perfect-freehand + OKLCH themes) from /workspace/figments-hirsel
bundle-figments figments_dir="/workspace/figments-hirsel":
    #!/usr/bin/env bash
    set -euo pipefail
    ASSETS="{{root}}/desktop/hirsel_desktop/hirsel_session/assets"
    mkdir -p "$ASSETS" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/assets"
    TMP_ENTRY="$(mktemp --suffix=.ts)"
    trap 'rm -f "$TMP_ENTRY"' EXIT
    cat > "$TMP_ENTRY" <<EOF
    export { routeElbow, autoAnchor } from '{{figments_dir}}/shell/canvas/elbow_router.ts';
    export { getStroke, getSvgPathFromStroke } from '{{figments_dir}}/shell/canvas/freehand.ts';
    export { shapePath } from '{{figments_dir}}/shell/canvas/shapes.ts';
    EOF
    npx --yes esbuild "$TMP_ENTRY" --bundle --minify --format=iife --global-name=FigmentsCanvas \
      --outfile="$ASSETS/figments-canvas.js"
    cp "{{figments_dir}}/shell/themes.css" "$ASSETS/themes.css"
    mkdir -p "$ASSETS/fonts" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/assets/fonts"
    cp "{{figments_dir}}/shell/fonts/fonts.css" \
       "{{figments_dir}}/shell/fonts/host-grotesk-latin.woff2" \
       "{{figments_dir}}/shell/fonts/host-grotesk-latin-ext.woff2" \
       "{{figments_dir}}/shell/fonts/fragment-mono-latin.woff2" \
       "$ASSETS/fonts/"
    cp -r "$ASSETS/"* "{{fuchsia}}/products/hirsel_desktop/hirsel_session/assets/"
    ls -lh "$ASSETS/" "$ASSETS/fonts/"

# Assemble full Hirsel-OS Desktop product bundle (Zircon AMD-SVM + Flatland + Native Chromium 160 + real Figments canvas + hirsel-wm + hirsel-js + Linux 6.1.102 vm-dev)
build-desktop: build-wm build-js build-vm-host fetch-linux-kernel build-linux-ssh-initrd
    #!/usr/bin/env bash
    set -euo pipefail
    cp "{{root}}/desktop/hirsel_desktop.gni" "{{fuchsia}}/products/"
    mkdir -p "{{fuchsia}}/products/hirsel_desktop/hirsel_session/meta" \
             "{{fuchsia}}/products/hirsel_desktop/hirsel_session/fidl" \
             "{{fuchsia}}/products/hirsel_desktop/hirsel_session/assets" \
             "{{fuchsia}}/products/hirsel_desktop/hirsel_session/src" \
             "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt/vm"
    cp "{{root}}/desktop/hirsel_desktop/BUILD.gn" "{{root}}/desktop/hirsel_desktop/BUILD.bazel" "{{fuchsia}}/products/hirsel_desktop/"
    cp "{{root}}/desktop/hirsel_desktop/hirsel_session/BUILD.gn" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/"
    cp "{{root}}/desktop/hirsel_desktop/hirsel_session/fidl/"*.fidl "{{fuchsia}}/products/hirsel_desktop/hirsel_session/fidl/"
    cp "{{root}}/desktop/hirsel_desktop/hirsel_session/meta/"*.cml "{{fuchsia}}/products/hirsel_desktop/hirsel_session/meta/"
    cp -r "{{root}}/desktop/hirsel_desktop/hirsel_session/assets/"* "{{fuchsia}}/products/hirsel_desktop/hirsel_session/assets/"
    cp "{{root}}/desktop/hirsel_desktop/hirsel_session/src/"*.rs "{{fuchsia}}/products/hirsel_desktop/hirsel_session/src/"
    cp "{{out_dir}}/linux/vmlinux.stripped" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt/vm/vmlinux.stripped"
    cp "{{out_dir}}/linux/initramfs-ssh.cpio" "{{fuchsia}}/products/hirsel_desktop/hirsel_session/prebuilt/vm/initramfs-ssh.cpio"
    cd "{{fuchsia}}"
    if git apply --check "{{root}}/patches/fuchsia-hirsel-desktop.patch" 2>/dev/null; then
      git apply "{{root}}/patches/fuchsia-hirsel-desktop.patch"
    fi
    if [[ ! -f "out/hirsel-desktop.x64/args.gn" ]]; then
      ORIG_BUILD_DIR="$(cat .fx-build-dir 2>/dev/null || echo out/bringup.x64-balanced)"
      PATH="{{fuchsia}}/.jiri_root/bin:$PATH" fx --dir out/hirsel-desktop.x64 set hirsel_desktop.x64 --rbe-mode=off
      echo "$ORIG_BUILD_DIR" > .fx-build-dir
    fi
    PATH="{{fuchsia}}/.jiri_root/bin:$PATH" fx --dir out/hirsel-desktop.x64 build //products/hirsel_desktop:hirsel_desktop.x64
    ls -lh out/hirsel-desktop.x64/obj/products/hirsel_desktop/hirsel_desktop.x64/product_bundle/system_a/

# Build GPT A/B UEFI bootable raw disk image for bare-metal USB/NVMe flashing
build-usb-image out_img="out/hirsel-desktop-uefi.img":
    bash "{{root}}/scripts/build_uefi_usb.sh" "{{out_img}}"

# Boot Hirsel-OS Desktop interactively in QEMU/KVM (VNC on :0 / port 5900, control API on :7780, Figments UI on :7711)
boot-desktop vnc=":0":
    #!/usr/bin/env bash
    set -euo pipefail
    PB_SYS="{{fuchsia}}/out/hirsel-desktop.x64/obj/products/hirsel_desktop/hirsel_desktop.x64/product_bundle/system_a"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    DISK="{{out_dir}}/desktop-live.blk"
    mkdir -p "{{out_dir}}"
    if [[ ! -f "$DISK" || "${PB_SYS}/fxfs.sparse.blk" -nt "$DISK" ]]; then
      python3 "{{root}}/scripts/unsparse_fxfs.py" "${PB_SYS}/fxfs.sparse.blk" "$DISK"
    fi
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    echo "=== Booting Hirsel-OS Desktop (VNC={{vnc}}, Control=http://127.0.0.1:7780/_hirsel/state, UI=http://127.0.0.1:7711/) ==="
    QEMU_ARGS=(
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -m 4096M
      -smp 4,threads=2
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -fw_cfg name=etc/sercon-port,string=0
      -drive "file=${DISK},format=raw,if=none,id=vdisk,cache=writeback"
      -object iothread,id=iothread0
      -device virtio-blk-pci,drive=vdisk,iothread=iothread0
      -netdev "type=user,id=net0,restrict=off,hostfwd=tcp:127.0.0.1:7780-:7780,hostfwd=tcp:127.0.0.1:7711-:7711"
      -device "virtio-net-pci,vectors=8,romfile=,netdev=net0,mac=52:54:00:63:5e:7a,addr=0x4"
      -device virtio-gpu-pci,xres=1280,yres=800
      -device virtio-keyboard-pci
      -device virtio-mouse-pci
      -vga none
      -vnc "{{vnc}}"
      -parallel none
      -audiodev none,id=snd0
      -serial mon:stdio
      -kernel "${PB_SYS}/linux-x86-boot-shim.bin"
      -initrd "${PB_SYS}/fuchsia.zbi"
      -append "kernel.serial=legacy kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true kernel.lockup-detector.critical-section-fatal-threshold-ms=0 kernel.lockup-detector.heartbeat-age-fatal-threshold-ms=0 zircon.nodename=hirsel-desktop"
    )
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
      exec "$QEMU" -enable-kvm -cpu "host,migratable=no,+invtsc" "${QEMU_ARGS[@]}"
    elif id -nG "$USER" | tr ' ' '\n' | grep -qx kvm; then
      exec sg kvm -c "$(printf '%q ' "$QEMU" -enable-kvm -cpu "host,migratable=no,+invtsc" "${QEMU_ARGS[@]}")"
    else
      exec "$QEMU" -cpu "Haswell,+smap,-check,-fsgsbase" "${QEMU_ARGS[@]}"
    fi

# Boot Hirsel-OS Desktop locally with GTK/OpenGL virtio-gpu-gl + Intel HDA audio (for bare-metal/desktop workstation use)
boot-desktop-gl:
    #!/usr/bin/env bash
    set -euo pipefail
    PB_SYS="{{fuchsia}}/out/hirsel-desktop.x64/obj/products/hirsel_desktop/hirsel_desktop.x64/product_bundle/system_a"
    QEMU="{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
    DISK="{{out_dir}}/desktop-live.blk"
    mkdir -p "{{out_dir}}"
    if [[ ! -f "$DISK" || "${PB_SYS}/fxfs.sparse.blk" -nt "$DISK" ]]; then
      python3 "{{root}}/scripts/unsparse_fxfs.py" "${PB_SYS}/fxfs.sparse.blk" "$DISK"
    fi
    ENTROPY="$(head -c 32 /dev/urandom | sha256sum | awk '{print $1}')"
    QEMU_ARGS=(
      -L "{{fuchsia}}/prebuilt/third_party/qemu/linux-x64/share/qemu"
      -machine q35,smbios-entry-point-type=32,i8042=off
      -m 4096M
      -smp 4,threads=2
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -fw_cfg name=etc/sercon-port,string=0
      -drive "file=${DISK},format=raw,if=none,id=vdisk,cache=writeback"
      -object iothread,id=iothread0
      -device virtio-blk-pci,drive=vdisk,iothread=iothread0
      -netdev "type=user,id=net0,restrict=off,hostfwd=tcp:127.0.0.1:7780-:7780,hostfwd=tcp:127.0.0.1:7711-:7711"
      -device "virtio-net-pci,vectors=8,romfile=,netdev=net0,mac=52:54:00:63:5e:7a,addr=0x4"
      -device virtio-gpu-pci,xres=1280,yres=800
      -device virtio-keyboard-pci
      -device virtio-mouse-pci
      -device intel-hda -device hda-duplex
      -vga none
      -display gtk
      -serial mon:stdio
      -kernel "${PB_SYS}/linux-x86-boot-shim.bin"
      -initrd "${PB_SYS}/fuchsia.zbi"
      -append "kernel.serial=legacy kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true kernel.lockup-detector.critical-section-fatal-threshold-ms=0 kernel.lockup-detector.heartbeat-age-fatal-threshold-ms=0 zircon.nodename=hirsel-desktop"
    )
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
      exec "$QEMU" -enable-kvm -cpu "host,migratable=no,+invtsc" "${QEMU_ARGS[@]}"
    else
      exec sg kvm -c "$(printf '%q ' "$QEMU" -enable-kvm -cpu "host,migratable=no,+invtsc" "${QEMU_ARGS[@]}")"
    fi

# End-to-end automated QEMU/KVM desktop verification + framebuffer screenshot capture
test-desktop:
    python3 "{{root}}/scripts/test_desktop.py"

# Inspect a ZBI container image
zbi-list image:
    cargo run -p zbi -- list "{{image}}"
