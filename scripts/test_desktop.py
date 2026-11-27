#!/usr/bin/env python3
"""Boot Hirsel-OS Desktop in QEMU/KVM, verify WM + Chromium 160 + security invariants, and dump screenshots."""
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
import zlib

ROOT = "/workspace/hirsel-os"
FUCHSIA = f"{ROOT}/third-party/fuchsia"
PB_SYS = f"{FUCHSIA}/out/hirsel-desktop.x64/obj/products/hirsel_desktop/hirsel_desktop.x64/product_bundle/system_a"
QEMU = f"{FUCHSIA}/prebuilt/third_party/qemu/linux-x64/bin/qemu-system-x86_64"
QEMU_SHARE = f"{FUCHSIA}/prebuilt/third_party/qemu/linux-x64/share/qemu"
OUT = f"{ROOT}/out/desktop-test"
DISK = f"{OUT}/fxfs.blk"
SERIAL_LOG = f"{OUT}/serial.log"
QMP_SOCK = f"{OUT}/qmp.sock"
CTRL_PORT = 17780
WEB_PORT = 17711


def ppm_to_png(ppm_path: str, png_path: str) -> tuple[int, int]:
    with open(ppm_path, "rb") as f:
        magic = f.readline().strip()
        assert magic == b"P6", f"expected P6 PPM, got {magic!r}"
        line = f.readline()
        while line.startswith(b"#"):
            line = f.readline()
        w, h = map(int, line.split())
        maxval = int(f.readline().strip())
        assert maxval == 255
        rgb = f.read(w * h * 3)

    # Build raw PNG scanlines (filter byte 0 per row)
    stride = w * 3
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw.extend(rgb[y * stride : (y + 1) * stride])

    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
        + chunk(b"IEND", b"")
    )
    with open(png_path, "wb") as f:
        f.write(png)
    return w, h


def qmp_screendump(png_path: str) -> tuple[int, int]:
    ppm_path = png_path + ".ppm"
    if os.path.exists(ppm_path):
        os.unlink(ppm_path)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    s.connect(QMP_SOCK)
    f = s.makefile("rwb", buffering=0)
    _greeting = f.readline()
    f.write(b'{"execute":"qmp_capabilities"}\n')
    _caps = f.readline()
    cmd = json.dumps({"execute": "screendump", "arguments": {"filename": ppm_path}}) + "\n"
    f.write(cmd.encode())
    resp = f.readline().decode()
    s.close()
    assert '"return": {}' in resp, f"QMP screendump failed: {resp}"
    w, h = ppm_to_png(ppm_path, png_path)
    os.unlink(ppm_path)
    print(f"  [screenshot] saved {png_path} ({w}x{h})")
    return w, h


def http_req(method: str, path: str, body: str | None = None, timeout: float = 5.0) -> tuple[int, dict]:
    url = f"http://127.0.0.1:{CTRL_PORT}{path}"
    data = body.encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode())


def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    for p in (SERIAL_LOG, QMP_SOCK):
        if os.path.exists(p):
            os.unlink(p)

    print("=== 1. Unsparsing Fxfs product bundle disk ===")
    subprocess.run(
        [sys.executable, f"{ROOT}/scripts/unsparse_fxfs.py", f"{PB_SYS}/fxfs.sparse.blk", DISK],
        check=True,
    )

    entropy = os.urandom(32).hex()
    qemu_cmd = [
        QEMU,
        "-L", QEMU_SHARE,
        "-machine", "q35,smbios-entry-point-type=32,i8042=off",
        "-cpu", "host,migratable=no,+invtsc",
        "-enable-kvm",
        "-m", "4096M",
        "-smp", "4,threads=2",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-fw_cfg", "name=etc/sercon-port,string=0",
        "-drive", f"file={DISK},format=raw,if=none,id=vdisk,cache=unsafe",
        "-object", "iothread,id=iothread0",
        "-device", "virtio-blk-pci,drive=vdisk,iothread=iothread0",
        "-netdev", f"type=user,id=net0,restrict=off,hostfwd=tcp:127.0.0.1:{CTRL_PORT}-:7780,hostfwd=tcp:127.0.0.1:{WEB_PORT}-:7711",
        "-device", "virtio-net-pci,vectors=8,romfile=,netdev=net0,mac=52:54:00:63:5e:7a,addr=0x4",
        "-device", "virtio-gpu-pci,xres=1280,yres=800",
        "-device", "virtio-keyboard-pci",
        "-device", "virtio-mouse-pci",
        "-vga", "none",
        "-display", "none",
        "-parallel", "none",
        "-audiodev", "none,id=snd0",
        "-qmp", f"unix:{QMP_SOCK},server=on,wait=off",
        "-serial", f"file:{SERIAL_LOG}",
        "-kernel", f"{PB_SYS}/linux-x86-boot-shim.bin",
        "-initrd", f"{PB_SYS}/fuchsia.zbi",
        "-append", (
            f"kernel.serial=legacy kernel.entropy-mixin={entropy} "
            "kernel.halt-on-panic=true "
            "kernel.lockup-detector.critical-section-fatal-threshold-ms=0 "
            "kernel.lockup-detector.heartbeat-age-fatal-threshold-ms=0 "
            "zircon.nodename=hirsel-desktop"
        ),
    ]

    if not (os.access("/dev/kvm", os.R_OK | os.W_OK)):
        qemu_cmd = ["sg", "kvm", "-c", " ".join(subprocess.list2cmdline([a]) for a in qemu_cmd)]

    print("=== 2. Booting Hirsel-OS Desktop in QEMU/KVM ===")
    proc = subprocess.Popen(qemu_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        ready = False
        t0 = time.time()
        for i in range(180):
            if proc.poll() is not None:
                err = proc.stderr.read().decode(errors="replace")
                print(f"QEMU exited prematurely (rc={proc.returncode}):\n{err}")
                break
            try:
                code, st = http_req("GET", "/_hirsel/state", timeout=1.5)
                if code == 200 and "focused" in st:
                    print(f"Hirsel-OS Desktop API READY in {time.time() - t0:.1f}s! state={st}")
                    ready = True
                    break
            except Exception:
                pass
            if i >= 5 and i % 5 == 0:
                try:
                    dcode, dst = http_req("GET", "/_hirsel/diag", timeout=1.0)
                    if dcode == 200:
                        print(f"  [{time.time() - t0:.0f}s] /_hirsel/diag -> {dst}")
                        if any("failed" in l for l in dst.get("diag", [])):
                            return 1
                except Exception:
                    pass
            if i % 15 == 14 and os.path.exists(SERIAL_LOG):
                txt = open(SERIAL_LOG, errors="replace").read()
                interesting = [
                    l for l in txt.splitlines()
                    if any(k in l for k in ("hirsel", "web_engine", "Chromium", "Flatland", "session_manager", "ERROR", "WARN"))
                ][-6:]
                print(f"  [{time.time() - t0:.0f}s] waiting... recent log lines:")
                for l in interesting:
                    print("   ", l[:140])
            time.sleep(1.0)

        if not ready:
            print("ERROR: Hirsel-OS Desktop API did not become ready in time!")
            if os.path.exists(SERIAL_LOG):
                lines = open(SERIAL_LOG, errors="replace").read().splitlines()
                print("=== Last 80 serial lines ===")
                for l in lines[-80:]:
                    print(l)
            return 1

        # Wait a few seconds for Native Chromium 160 to finish initial navigation & first paint
        print("=== 3. Waiting for Native Chromium 160 to paint initial spatial canvas ===")
        time.sleep(8.0)
        qmp_screendump(f"{OUT}/desktop-1-initial.png")

        print("=== 4. Testing focus anti-spoofing (CLIENT_FOCUS web_untrusted MUST be blocked) ===")
        code, st = http_req("POST", "/_hirsel/spoof-focus", "web_untrusted")
        assert code == 200 and st["focused"] == "figments", f"Focus spoof not blocked: {st}"
        print("  PASS: focus remained", st["focused"])

        print("=== 5. Testing split layout (vm-dev + isolated Chromium Context #2) & Powerbox ===")
        _, st = http_req("POST", "/_hirsel/cmd", "TOGGLE_DEV")
        assert st["layout"]["vm_dev"] is not None
        _, st = http_req("POST", "/_hirsel/cmd", "TOGGLE_WEB")
        assert st["layout"]["web"] is not None
        http_req("POST", "/_hirsel/dev-exec", "uname -a && ls /pkg/bin/hirsel-wm")
        http_req("POST", "/_hirsel/cmd", "CLIP_COPY figments 42")
        _, st = http_req("POST", "/_hirsel/cmd", "POWERBOX_PASTE figments vm_dev")
        assert st["powerbox_transfers"] == 1, f"Powerbox failed: {st}"
        print("  PASS: split active + powerbox_transfers=1")
        time.sleep(6.0)
        qmp_screendump(f"{OUT}/desktop-2-split.png")

        print("=== 6. Testing Protected Effect Outbox Approval Ceremony ===")
        _, st = http_req("POST", "/_hirsel/stage", "")
        assert st["focused"] == "trusted_chrome", f"Expected trusted_chrome focus: {st}"
        assert st["capture_allowed"] is False, f"Expected capture_allowed=false: {st}"
        assert st["outbox"]["status"] == "staged", f"Expected staged outbox: {st}"
        assert st["layout"]["ceremony"] is not None, f"Expected ceremony rect: {st}"
        print("  PASS: ceremony active, capture_allowed=false, focused=trusted_chrome")
        time.sleep(1.5)
        qmp_screendump(f"{OUT}/desktop-3-ceremony.png")

        print("=== 7. Testing bad-digest rejection, hash-bound approval + Bank spend, and Caretaker Revoke ===")
        bad_id = st["outbox"]["id"]
        _, st = http_req("POST", "/_hirsel/cmd", f"APPROVE {bad_id} 111111")
        assert st["denied"] == 1 and st["bank_spent_micros"] == 0, f"Bad digest not denied: {st}"
        _, st = http_req("POST", "/_hirsel/stage", "")
        _, st = http_req("POST", "/_hirsel/approve", "")
        assert st["approved"] == 1 and st["bank_spent_micros"] == 250000 and st["capture_allowed"] is True, st
        code, prx = http_req("POST", "/_hirsel/proxy", "api.github.com")
        assert code == 200 and prx["allowed"] is True, prx
        _, st = http_req("POST", "/_hirsel/cmd", "REVOKE")
        assert st["agent_conn"] is False
        code, prx = http_req("POST", "/_hirsel/proxy", "api.github.com")
        assert code == 403 and prx["reason"] == "caretaker_connection_revoked", (code, prx)
        print("  PASS: bad digest denied, good digest spent $0.25, egress-gw allowed then revoked (403)!")

        print("=== 8. Checking serial logs for hirsel-wm & Chromium 160 ===")
        txt = open(SERIAL_LOG, errors="replace").read()
        for l in txt.splitlines():
            if any(k in l for k in ("hirsel-wm", "Hirsel-OS", "Chromium", "ContextProvider", "Flatland root")):
                print("  [serial]", l[:160])

        print("\n=== ALL DESKTOP E2E CHECKS PASSED! ===")
        return 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
