#!/usr/bin/env python3
"""End-to-end QEMU/KVM verification of Hirsel-OS Desktop:
  1. Pure-Hirsel hirsel-wm (layout, anti-spoofing, ceremony, Bank, Caretaker)
  2. Native Chromium 160 + Chrome 160 HTML-in-Canvas (drawElementImage) + 2 isolated contexts
  3. Pure-Hirsel Figments js@1 sandbox Place (/pkg/bin/hirsel-js, zero ambient handles)
  4. Real Tier-2 2-vCPU Linux 6.1.102 AMD-SVM MicroVM in vm-dev (/dev/hvc0 + persistent /mnt/data)
  5. Real Powerbox clipboard byte delivery into Linux 6.1.102 (/mnt/data/powerbox_paste.txt)
  6. Real outbound egress-gw HTTP forwarding with host-only secret injection & Outbox execution
  7. Full cold-reboot persistence on Fxfs (room, audit log, and Linux /mnt/data ext2 volume!)
"""
import json
import os
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


def http_req(method: str, path: str, body: str | None = None, timeout: float = 15.0) -> tuple[int, dict]:
    url = f"http://127.0.0.1:{CTRL_PORT}{path}"
    data = body.encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode())


def start_qemu(log_path: str) -> subprocess.Popen:
    for p in (log_path, QMP_SOCK):
        if os.path.exists(p):
            os.unlink(p)
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
        "-drive", f"file={DISK},format=raw,if=none,id=vdisk,cache=writeback",
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
        "-serial", f"file:{log_path}",
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
    return subprocess.Popen(qemu_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def stop_qemu(proc: subprocess.Popen) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=3)


def wait_for_desktop(proc: subprocess.Popen, label: str) -> dict:
    t0 = time.time()
    for i in range(90):
        if proc.poll() is not None:
            err = proc.stderr.read().decode(errors="replace")
            raise RuntimeError(f"{label}: QEMU exited rc={proc.returncode}:\n{err}")
        try:
            code, st = http_req("GET", "/_hirsel/state", timeout=1.5)
            if code == 200 and "focused" in st:
                _, diag = http_req("GET", "/_hirsel/diag", timeout=1.5)
                print(f"[{label}] Desktop API READY in {time.time() - t0:.1f}s! diag={diag.get('diag')}")
                return st
        except Exception:
            pass
        if i >= 5 and i % 5 == 0:
            try:
                dcode, dst = http_req("GET", "/_hirsel/diag", timeout=1.0)
                if dcode == 200:
                    print(f"  [{label} {time.time() - t0:.0f}s] /_hirsel/diag -> {dst}")
                    if any("failed" in l for l in dst.get("diag", [])):
                        raise RuntimeError(f"Desktop init failed: {dst}")
            except RuntimeError:
                raise
            except Exception:
                pass
        time.sleep(1.0)
    raise TimeoutError(f"{label}: desktop API did not become ready in time")


def wait_for_linux_vm(label: str) -> None:
    t0 = time.time()
    for _ in range(45):
        code, vms = http_req("GET", "/_hirsel/vm-status", timeout=2.0)
        if code == 200 and vms.get("vm_ready") is True:
            print(f"[{label}] Tier-2 Linux 6.1.102 AMD-SVM MicroVM READY in {time.time() - t0:.1f}s!")
            return
        time.sleep(0.5)
    raise TimeoutError(f"{label}: Linux 6.1.102 vm-dev did not reach READY")


def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    print("=== 1. Unsparsing Fxfs product bundle disk ===")
    subprocess.run(
        [sys.executable, f"{ROOT}/scripts/unsparse_fxfs.py", f"{PB_SYS}/fxfs.sparse.blk", DISK],
        check=True,
    )

    print("=== 2. BOOT #1: Full Hirsel-OS Desktop ===")
    proc = start_qemu(SERIAL_LOG)
    try:
        st = wait_for_desktop(proc, "BOOT1")
        assert st["room"] == "darkroom" and st["focused"] == "figments"
        wait_for_linux_vm("BOOT1")

        print("=== 3. Wait for Native Chromium 160 + HTML-in-Canvas paint ===")
        time.sleep(6.0)
        _, cdiag = http_req("GET", "/_hirsel/client-diag")
        print("  Chromium 160 client-diag:", cdiag)
        assert cdiag.get("drawElementWorked") is True, f"HTML-in-Canvas drawElementImage did not render live: {cdiag}"
        qmp_screendump(f"{OUT}/desktop-1-initial.png")

        print("=== 4. Verify Pure-Hirsel Figments js@1 Codemode Sandbox + Persistent Board Card ===")
        code, js_res = http_req(
            "POST",
            "/_hirsel/codemode",
            "console.log('hi from Zircon'); return [6*7, 'hirsel-js@1']",
        )
        print("  js@1 result:", js_res)
        assert code == 200 and js_res.get("ok") is True, js_res
        assert "42" in js_res.get("value", "") and "hirsel-js@1" in js_res.get("value", ""), js_res
        assert "hi from Zircon" in js_res.get("logs", ""), js_res

        code, board_res = http_req(
            "POST",
            "/_hirsel/board",
            json.dumps({
                "id": "js-eval-1",
                "title": "js@1 Codemode Result",
                "sub": f"eval={js_res.get('value')} | persisted on Fxfs",
                "color": "amber",
                "x": 28,
                "y": 300,
            }),
        )
        assert code == 200 and len(board_res.get("cards", [])) == 1, board_res

        print("=== 5. Verify REAL 2-vCPU Linux 6.1.102 AMD-SVM MicroVM + Persistent /mnt/data ===")
        code, vm_res = http_req(
            "POST",
            "/_hirsel/dev-exec",
            "uname -srm && nproc && echo DESKTOP_PERSIST_99 > /mnt/data/persist.txt && sync && cat /mnt/data/persist.txt",
        )
        print("  vm-dev exec:", vm_res)
        assert code == 200 and vm_res.get("exit_code") == 0, vm_res
        out_txt = vm_res.get("stdout", "")
        assert "Linux 6.1.102" in out_txt and "DESKTOP_PERSIST_99" in out_txt, out_txt

        print("=== 6. Verify Focus Anti-Spoofing + Compositor Click + 3-Way Split + Powerbox ===")
        _, st = http_req("POST", "/_hirsel/spoof-focus", "web_untrusted")
        assert st["focused"] == "figments", f"Focus spoof not blocked: {st}"
        # Click F2:VM-DEV pill in Top Trust Bar (x=280, y=18) -> opens Linux vm-dev pane!
        _, st = http_req("POST", "/_hirsel/click", json.dumps({"x": 280, "y": 18}))
        assert st["layout"]["vm_dev"] is not None, f"Top-bar F2 click failed: {st}"
        # Click F3:WEB pill in Top Trust Bar (x=365, y=18) -> opens isolated Chromium #2!
        _, st = http_req("POST", "/_hirsel/click", json.dumps({"x": 365, "y": 18}))
        assert st["layout"]["web"] is not None, f"Top-bar F3 click failed: {st}"
        # Click inside vm_dev pane & type a live command into the Linux terminal prompt
        vr = st["layout"]["vm_dev"]
        _, st = http_req("POST", "/_hirsel/click", json.dumps({"x": vr["x"] + 60, "y": vr["y"] + 80}))
        assert st["focused"] == "vm_dev", f"Click vm_dev focus failed: {st}"
        http_req("POST", "/_hirsel/type", "echo TYPED_INTERACTIVELY > /mnt/data/typed.txt && sync\n")
        time.sleep(0.8)

        _, _ = http_req(
            "POST",
            "/_hirsel/clipboard",
            json.dumps({"domain": "figments", "text": "POWERBOX_SECRET_42_FROM_FIGMENTS"}),
        )
        code, pb = http_req(
            "POST",
            "/_hirsel/powerbox-paste",
            json.dumps({"from": "figments", "to": "vm_dev"}),
        )
        print("  powerbox result:", pb)
        assert code == 200 and pb.get("authorized") is True and pb.get("delivered") is True, pb
        _, check_pb = http_req("POST", "/_hirsel/dev-exec", "cat /mnt/data/powerbox_paste.txt")
        assert "POWERBOX_SECRET_42_FROM_FIGMENTS" in check_pb.get("stdout", ""), check_pb
        print("  PASS: real Powerbox bytes verified inside Linux /mnt/data/powerbox_paste.txt!")

        time.sleep(5.0)
        qmp_screendump(f"{OUT}/desktop-2-split.png")

        print("=== 7. Verify Real Outbound egress-gw + Host-Only Secret Injection + Disallowed Host 403 ===")
        code, prx = http_req(
            "POST",
            "/_hirsel/proxy",
            json.dumps({"host": "127.0.0.1", "port": 7780, "path": "/_hirsel/echo-target"}),
        )
        print("  egress-gw ok:", prx)
        assert code == 200 and prx.get("http_status") == 200 and prx.get("secret_injected") is True, prx
        assert '"secret_present":true' in prx.get("body", ""), prx

        code, bad_prx = http_req("POST", "/_hirsel/proxy", json.dumps({"host": "evil.invalid", "port": 80, "path": "/"}))
        assert code == 403 and bad_prx.get("reason") == "host_not_in_capability_allowlist", (code, bad_prx)

        print("=== 8. Verify Protected Outbox Ceremony + Bad Digest Denial + Real Approved Execution ===")
        _, st = http_req("POST", "/_hirsel/stage", "")
        assert st["focused"] == "trusted_chrome" and st["capture_allowed"] is False and st["outbox"]["status"] == "staged"
        time.sleep(1.2)
        qmp_screendump(f"{OUT}/desktop-3-ceremony.png")

        bad_id = st["outbox"]["id"]
        _, st = http_req("POST", "/_hirsel/cmd", f"APPROVE {bad_id} 111111")
        assert st["denied"] == 1 and st["bank_spent_micros"] == 0

        _, st = http_req("POST", "/_hirsel/stage", "")
        code, appr = http_req("POST", "/_hirsel/approve", "")
        print("  approved + executed:", appr)
        assert code == 200 and appr["state"]["approved"] == 1 and appr["state"]["bank_spent_micros"] == 250000
        assert appr["execution"]["executed"] is True and appr["execution"]["http_status"] == 200

        _, st = http_req("POST", "/_hirsel/cmd", "REVOKE")
        code, rev_prx = http_req("POST", "/_hirsel/proxy", json.dumps({"host": "127.0.0.1", "port": 7780, "path": "/_hirsel/echo-target"}))
        assert code == 403 and rev_prx.get("reason") == "caretaker_connection_revoked"

        # Toggle room to daylight so we can verify Fxfs persistence across cold reboot!
        _, st = http_req("POST", "/_hirsel/cmd", "TOGGLE_ROOM")
        assert st["room"] == "daylight"
        _, diag1 = http_req("GET", "/_hirsel/diag")
        print("  boot1 final diag:", diag1)
        time.sleep(1.5)
    finally:
        stop_qemu(proc)

    print("=== 9. BOOT #2 (COLD REBOOT on same Fxfs disk — verifying workstation persistence!) ===")
    proc2 = start_qemu(f"{OUT}/serial-boot2.log")
    try:
        st2 = wait_for_desktop(proc2, "BOOT2")
        assert st2["room"] == "daylight", f"Expected persisted room=daylight after reboot, got {st2}"
        _, aud = http_req("GET", "/_hirsel/audit")
        print("  persisted audit log:", aud)
        assert any("APPROVED & EXECUTED" in e for e in aud.get("audit", [])), aud
        _, brd2 = http_req("GET", "/_hirsel/board")
        print("  persisted Figments board:", brd2)
        assert any(c.get("id") == "js-eval-1" for c in brd2.get("cards", [])), brd2

        wait_for_linux_vm("BOOT2")
        code, pchk = http_req("POST", "/_hirsel/dev-exec", "cat /mnt/data/persist.txt && cat /mnt/data/powerbox_paste.txt && cat /mnt/data/typed.txt")
        print("  persisted Linux /mnt/data after cold reboot:", pchk)
        assert code == 200
        assert "DESKTOP_PERSIST_99" in pchk.get("stdout", ""), pchk
        assert "POWERBOX_SECRET_42_FROM_FIGMENTS" in pchk.get("stdout", ""), pchk
        assert "TYPED_INTERACTIVELY" in pchk.get("stdout", ""), pchk
        time.sleep(4.0)
        qmp_screendump(f"{OUT}/desktop-4-reboot-daylight.png")
        print("\n=== ALL MILESTONE 1-3 E2E + PERSISTENCE CHECKS PASSED! ===")
        return 0
    finally:
        stop_qemu(proc2)


if __name__ == "__main__":
    sys.exit(main())
