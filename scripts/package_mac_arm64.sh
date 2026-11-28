#!/usr/bin/env bash
# Package a self-contained Hirsel-OS Desktop ARM64 bundle ready to run natively
# on Apple Silicon Macs (M1/M2/M3/M4) using Homebrew QEMU + Hypervisor.framework (HVF).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PB_SYS="${ROOT}/third-party/fuchsia/out/hirsel-desktop.arm64/obj/products/hirsel_desktop/hirsel_desktop.arm64/product_bundle/system_a"
STAGE="${ROOT}/out/hirsel-desktop-arm64-mac"
TARBALL="${ROOT}/out/hirsel-desktop-arm64-mac.tar.gz"

if [[ ! -f "${PB_SYS}/fuchsia.zbi" || ! -f "${PB_SYS}/fxfs.sparse.blk" ]]; then
  echo "Missing arm64 product bundle in ${PB_SYS}. Run: just build-desktop-arm64" >&2
  exit 1
fi

KERNEL_BIN=""
for cand in "${PB_SYS}/qemu-boot-shim.bin" "${PB_SYS}/linux-arm64-boot-shim.bin"; do
  if [[ -f "$cand" ]]; then
    KERNEL_BIN="$cand"
    break
  fi
done
if [[ -z "$KERNEL_BIN" ]]; then
  KERNEL_BIN="$(find "${PB_SYS}" -maxdepth 1 -name '*boot-shim*.bin' | head -n 1 || true)"
fi
if [[ -z "$KERNEL_BIN" ]]; then
  echo "Could not locate arm64 boot shim in ${PB_SYS}:" >&2
  ls -la "${PB_SYS}" >&2
  exit 1
fi

rm -rf "${STAGE}"
mkdir -p "${STAGE}"
cp "${KERNEL_BIN}" "${STAGE}/boot-shim.bin"
cp "${PB_SYS}/fuchsia.zbi" "${STAGE}/fuchsia.zbi"
cp "${PB_SYS}/fxfs.sparse.blk" "${STAGE}/fxfs.sparse.blk"
cp "${ROOT}/scripts/unsparse_fxfs.py" "${STAGE}/unsparse_fxfs.py"

cat > "${STAGE}/run-mac.sh" <<'EOF'
#!/usr/bin/env bash
# Launch Hirsel-OS Desktop (ARM64) natively on Apple Silicon Mac using QEMU + Hypervisor.framework (HVF).
#
# Prerequisite (once):
#   brew install qemu
#
# Usage:
#   ./run-mac.sh           # Native macOS Cocoa window (1280x800) + Control API on :7780
#   ./run-mac.sh --vnc     # Headless with VNC on localhost:5900
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DISK="${DIR}/fxfs.blk"

if ! command -v qemu-system-aarch64 >/dev/null 2>&1; then
  echo "qemu-system-aarch64 not found. Install it with: brew install qemu" >&2
  exit 1
fi

if [[ ! -f "${DISK}" || "${DIR}/fxfs.sparse.blk" -nt "${DISK}" ]]; then
  echo "=== Unsparsing Fxfs workstation disk (first launch only)... ==="
  python3 "${DIR}/unsparse_fxfs.py" "${DIR}/fxfs.sparse.blk" "${DISK}"
fi

ENTROPY="$(openssl rand -hex 32 2>/dev/null || python3 -c 'import os; print(os.urandom(32).hex())')"
DISPLAY_ARGS=(-display cocoa,show-cursor=on,zoom-to-fit=on)
GIC_VER="3"
FORCE_TCG=0
RES="${HIRSEL_RES:-1600x1000}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --vnc)        DISPLAY_ARGS=(-vnc 127.0.0.1:0); shift ;;
    --fullscreen) DISPLAY_ARGS=(-display cocoa,show-cursor=on,zoom-to-fit=on,full-screen=on); shift ;;
    --res)        RES="${2:?--res requires WxH (e.g. 1920x1200)}"; shift 2 ;;
    --res=*)      RES="${1#--res=}"; shift ;;
    --gic2)       GIC_VER="2"; shift ;;
    --tcg)        FORCE_TCG=1; shift ;;
    -h|--help)
      echo "Usage: $0 [--res WxH] [--fullscreen] [--vnc] [--gic2] [--tcg]"
      echo "  --res WxH     Guest display resolution (default: 1600x1000; e.g. 1920x1200, 2560x1600)"
      echo "  --fullscreen  Start QEMU Cocoa window in fullscreen"
      echo "  --vnc         Run headless with VNC on 127.0.0.1:5900"
      echo "  --gic2        Use GICv2 instead of GICv3"
      echo "  --tcg         Force software TCG instead of HVF"
      exit 0
      ;;
    *) shift ;;
  esac
done

XRES="${RES%x*}"
YRES="${RES#*x}"
if [[ -z "$XRES" || -z "$YRES" || "$XRES" == "$RES" ]]; then
  echo "Invalid --res '${RES}'. Expected WxH (e.g. 1600x1000 or 1920x1200)." >&2
  exit 1
fi

MEM="${HIRSEL_MEM:-4096M}"
EXTRA_KARGS=""
if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" && "$FORCE_TCG" == "0" ]]; then
  MACHINE_ARGS=(-machine "virt,gic-version=${GIC_VER},highmem-ecam=off")
  ACCEL_ARGS=(-accel hvf -cpu host)
  EXTRA_KARGS="kernel.page-scanner.page-table-eviction-policy=never"
else
  MACHINE_ARGS=(-machine "virt,virtualization=true,gic-version=${GIC_VER},highmem-ecam=off")
  ACCEL_ARGS=(-accel tcg,thread=single -cpu max)
  if [[ "$(uname -s)" != "Darwin" && "${DISPLAY_ARGS[0]}" == "-display" ]]; then
    DISPLAY_ARGS=(-display gtk,show-cursor=on,zoom-to-fit=on)
  fi
fi

echo "=== Booting Hirsel-OS Desktop (ARM64, ${XRES}x${YRES}) ==="
echo "  Control API : http://127.0.0.1:7780/_hirsel/state"
echo "  Figments UI : http://127.0.0.1:7711/"
echo "  UI Scale    : Top-right Settings -> Rooms -> UI Scale (100% - 200%)"
echo "  Mouse Grab  : Click window to capture; Ctrl+Option+G to release"
echo "  Hotkeys     : F1 Figments | F2 vm-dev | F3 Web | F4 Room | F5 Outbox | F6 SAS | , Settings"

exec qemu-system-aarch64 \
  "${MACHINE_ARGS[@]}" \
  "${ACCEL_ARGS[@]}" \
  -m "${MEM}" \
  -smp 4 \
  -drive "file=${DISK},format=raw,if=none,id=vdisk,cache=writeback" \
  -object iothread,id=iothread0 \
  -device virtio-blk-pci,drive=vdisk,iothread=iothread0 \
  -netdev "type=user,id=net0,restrict=off,hostfwd=tcp:127.0.0.1:7780-:7780,hostfwd=tcp:127.0.0.1:7711-:7711,hostfwd=tcp:127.0.0.1:7712-:7712" \
  -device "virtio-net-pci,vectors=8,romfile=,netdev=net0,mac=52:54:00:63:5e:7a,addr=0x4" \
  -device "virtio-gpu-pci,xres=${XRES},yres=${YRES}" \
  -device virtio-keyboard-pci \
  -device virtio-mouse-pci \
  -device virtio-rng-pci \
  -vga none \
  -parallel none \
  -serial mon:stdio \
  "${DISPLAY_ARGS[@]}" \
  -kernel "${DIR}/boot-shim.bin" \
  -initrd "${DIR}/fuchsia.zbi" \
  -append "${EXTRA_KARGS} kernel.entropy-mixin=${ENTROPY} kernel.halt-on-panic=true kernel.lockup-detector.critical-section-fatal-threshold-ms=0 kernel.lockup-detector.heartbeat-age-fatal-threshold-ms=0 zircon.nodename=hirsel-desktop-arm64"
EOF
chmod +x "${STAGE}/run-mac.sh"

tar -czf "${TARBALL}" -C "${ROOT}/out" "hirsel-desktop-arm64-mac"
ls -lh "${STAGE}" "${TARBALL}"
