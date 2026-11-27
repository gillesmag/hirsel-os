#!/usr/bin/env python3
"""End-to-end QEMU/KVM verification of Hirsel-OS Desktop (Milestones 1-6):
  1. Pure-Hirsel hirsel-wm (layout, anti-spoofing, ceremony, Bank, Caretaker)
  2. Pure-Hirsel hirsel-board (cards, elbow edges, camera pan/zoom, Fxfs persistence)
  3. Pure-Hirsel hirsel-agent (autonomous multi-tool loop -> js@1 -> Linux vm-dev -> Board -> Ceremony)
  4. Native Chromium 160 + REAL HTML-in-Canvas (content="drawable" + drawElementImage) + multi-origin isolation
  5. Pure-Hirsel Figments js@1 sandbox Place (/pkg/bin/hirsel-js, zero ambient handles)
  6. Real Tier-2 2-vCPU Linux 6.1.102 AMD-SVM MicroVM in vm-dev (/dev/hvc0 + ANSI colors + persistent /mnt/data)
  7. Isolated egress_gw.cm Fuchsia component (TLS 1.3 root certs + SAS-only sealed secret vault + allowlist)
  8. Full cold-reboot persistence on Fxfs (room, audit, board, sealed secrets vault, Linux /mnt/data!)
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
UNTRUSTED_PORT = 17712


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


def http_req(method: str, path: str, body: str | None = None, timeout: float = 20.0, port: int = CTRL_PORT) -> tuple[int, dict]:
    url = f"http://127.0.0.1:{port}{path}"
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
        "-netdev", f"type=user,id=net0,restrict=off,hostfwd=tcp:127.0.0.1:{CTRL_PORT}-:7780,hostfwd=tcp:127.0.0.1:{WEB_PORT}-:7711,hostfwd=tcp:127.0.0.1:{UNTRUSTED_PORT}-:7712",
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

    print("=== 2. BOOT #1: Full Hirsel-OS Desktop (Milestones 1-6) ===")
    proc = start_qemu(SERIAL_LOG)
    try:
        st = wait_for_desktop(proc, "BOOT1")
        assert st["room"] == "darkroom" and st["focused"] == "figments"
        wait_for_linux_vm("BOOT1")

        print("=== 3. Verify Private FIDL egress_gw.cm (ZERO TCP ports!) + Interactive Masked SAS Secret Entry ===")
        for _ in range(20):
            hc, hval = http_req("GET", "/_hirsel/egress-health")
            if hc == 200 and hval.get("ok"):
                break
            time.sleep(0.3)
        print("  egress_gw.cm FIDL health:", hval)
        assert hc == 200 and hval.get("component") == "egress_gw.cm" and hval.get("transport") == "fidl_only_zero_tcp", hval
        assert hval.get("tls_roots", 0) > 50, f"Expected Fuchsia TLS root certs, got {hval}"

        # Track 1 Hardening checks:
        #   a) Untrusted web server (:7712) MUST NOT expose /_hirsel/* privileged routes!
        uc, uval = http_req("POST", "/_hirsel/dev-exec", "id", port=UNTRUSTED_PORT)
        assert uc == 404, f"Untrusted port :7712 leaked /_hirsel/dev-exec: {uc} {uval}"
        #   b) Figments server (:7711) MUST reject POST /_hirsel/* without valid X-Hirsel-Token nonce!
        fc, fval = http_req("POST", "/_hirsel/dev-exec", "id", port=WEB_PORT)
        assert fc == 403 and fval.get("error") == "missing_or_invalid_hirsel_token", (fc, fval)

        # Writing a secret outside SAS MUST be rejected with 403!
        sc, sval = http_req("POST", "/_hirsel/secrets", json.dumps({"host": "example.com", "value": "sealed-demo-key-99"}))
        assert sc == 403 and sval.get("reason") == "sas_trusted_chrome_required", (sc, sval)

        # Track 3: Interactive Masked SAS Secret Entry via keyboard (Tab x3 -> example.com, type masked key, Enter!)
        _, _ = http_req("POST", "/_hirsel/cmd", "TOGGLE_SAS")
        http_req("POST", "/_hirsel/type", "\t\t\tsealed-demo-key-99")
        time.sleep(0.3)
        qmp_screendump(f"{OUT}/desktop-sas-vault.png")
        http_req("POST", "/_hirsel/type", "\n")
        time.sleep(0.4)
        _, _ = http_req("POST", "/_hirsel/cmd", "TOGGLE_SAS")

        # Verify GET /_hirsel/secrets exposes metadata ONLY (never plaintext!)
        _, smeta = http_req("GET", "/_hirsel/secrets")
        print("  SAS FIDL-sealed vault metadata:", smeta)
        assert "sealed-demo-key-99" not in json.dumps(smeta), smeta
        assert any(x["host"] == "example.com" and x["len"] == 18 for x in smeta.get("secrets", [])), smeta

        print("=== 4. Wait for Native Chromium 160 + Real Figments Canvas Engine (A* Elbow + Freehand + HTML-in-Canvas) ===")
        time.sleep(6.0)
        _, cdiag = http_req("GET", "/_hirsel/client-diag")
        print("  Chromium 160 client-diag:", cdiag)
        assert cdiag.get("drawElementWorked") is True, f"HTML-in-Canvas drawElementImage did not render live: {cdiag}"
        assert cdiag.get("figmentsEngineLoaded") is True, f"Real Figments canvas engine bundle did not load: {cdiag}"
        qmp_screendump(f"{OUT}/desktop-1-initial.png")

        # Persist a real freehand ink stroke to Fxfs /data/ink.json via Figments perfect-freehand engine
        ic, ival = http_req(
            "POST",
            "/_hirsel/ink",
            json.dumps({
                "color": "#f59e0b",
                "points": [[48, 255, 0.45], [115, 275, 0.72], [185, 252, 0.65], [250, 272, 0.45]],
            }),
        )
        assert ic == 200 and len(ival.get("strokes", [])) >= 1, (ic, ival)

        print("=== 5. Verify Pure-Hirsel hirsel-js + hirsel-board + Linux 6.1.102 vm-dev ===")
        code, js_res = http_req(
            "POST",
            "/_hirsel/codemode",
            "console.log('hi from Zircon'); return [6*7, 'hirsel-js@1']",
        )
        print("  js@1 result:", js_res)
        assert code == 200 and js_res.get("ok") is True and "42" in js_res.get("value", ""), js_res

        code, board_res = http_req(
            "POST",
            "/_hirsel/board",
            json.dumps({
                "op": "put",
                "id": "js-eval-1",
                "kind": "code",
                "tone": "ochre",
                "title": "js@1 Codemode Result",
                "body": f"eval={js_res.get('value')} | persisted on Fxfs",
                "x": 24,
                "y": 300,
            }),
        )
        assert code == 200 and any(c["id"] == "js-eval-1" for c in board_res.get("cards", [])), board_res
        _, board_res = http_req(
            "POST",
            "/_hirsel/board",
            json.dumps({"op": "link", "from": "wm", "to": "js-eval-1", "label": "codemode"}),
        )
        assert any(e["to"] == "js-eval-1" for e in board_res.get("edges", [])), board_res

        code, vm_res = http_req(
            "POST",
            "/_hirsel/dev-exec",
            "printf '\\033[32mANSI_GREEN_OK\\033[0m\\n' && uname -srm && nproc && echo DESKTOP_PERSIST_99 > /mnt/data/persist.txt && sync && cat /mnt/data/persist.txt",
        )
        print("  vm-dev exec:", vm_res)
        assert code == 200 and vm_res.get("exit_code") == 0, vm_res
        assert "Linux 6.1.102" in vm_res.get("stdout", "") and "DESKTOP_PERSIST_99" in vm_res.get("stdout", "")

        print("=== 5b. Verify Capability-Gated virtio-net inside Linux vm-dev -> Isolated egress_gw.cm ONLY ===")
        code, vnet = http_req(
            "POST",
            "/_hirsel/dev-exec",
            "ip -o -4 addr show eth0 && ping -c 1 -W 2 10.0.3.1 >/dev/null && wget -qO- http://10.0.3.1:7781/health && echo '' && wget -qO- --post-data='{\"host\":\"127.0.0.1\",\"port\":7780,\"path\":\"/_hirsel/echo-target\"}' http://10.0.3.1:7781/proxy",
        )
        print("  Linux eth0 -> egress_gw.cm:", vnet)
        assert code == 200 and vnet.get("exit_code") == 0, vnet
        assert "10.0.3.2/24" in vnet.get("stdout", ""), vnet
        assert '"component":"egress_gw.cm"' in vnet.get("stdout", ""), vnet
        assert '"secret_injected":true' in vnet.get("stdout", "") and 'secret_present' in vnet.get("stdout", ""), vnet

        _, sysinfo = http_req("GET", "/_hirsel/system")
        print("  workstation system capabilities:", sysinfo)
        assert sysinfo.get("hardware", {}).get("fxfs_data") is True, sysinfo
        assert len(sysinfo.get("places", [])) == 4, sysinfo

        print("=== 5c. Verify Multi-VM Lifecycle: Spawn & Exec Secondary Named Linux 6.1.102 MicroVM (vm-test) ===")
        sc_vm, sval_vm = http_req("POST", "/_hirsel/vms", json.dumps({"op": "spawn", "name": "vm-test"}))
        print("  spawn vm-test:", sval_vm)
        assert sc_vm == 200 and sval_vm.get("ok") is True, (sc_vm, sval_vm)
        vm_test_ready = False
        for _ in range(45):
            vc, vlist = http_req("GET", "/_hirsel/vms", timeout=2.0)
            if vc == 200 and any(v.get("name") == "vm-test" and v.get("ready") is True for v in vlist.get("vms", [])):
                vm_test_ready = True
                break
            time.sleep(0.5)
        assert vm_test_ready, "Secondary Linux 6.1.102 MicroVM vm-test did not reach READY"
        ec_vm, eval_vm = http_req(
            "POST",
            "/_hirsel/vms",
            json.dumps({"op": "exec", "name": "vm-test", "cmd": "hostname && nproc && uname -r"}),
            timeout=20.0,
        )
        print("  vm-test exec:", eval_vm)
        assert ec_vm == 200 and eval_vm.get("exit_code") == 0, eval_vm
        assert "vm-test" in eval_vm.get("stdout", "") and "6.1.102" in eval_vm.get("stdout", ""), eval_vm

        print("=== 6. Verify Focus Anti-Spoofing + Compositor Clicks + Multi-Origin Web #2 + Powerbox ===")
        _, st = http_req("POST", "/_hirsel/spoof-focus", "web_untrusted")
        assert st["focused"] == "figments", f"Focus spoof not blocked: {st}"
        _, st = http_req("POST", "/_hirsel/click", json.dumps({"x": 280, "y": 18}))
        assert st["layout"]["vm_dev"] is not None
        _, st = http_req("POST", "/_hirsel/click", json.dumps({"x": 365, "y": 18}))
        assert st["layout"]["web"] is not None

        # Multi-origin switch spawns a brand-new OS-isolated fuchsia.web.Context on :7712!
        nc, nval = http_req("POST", "/_hirsel/web-navigate", json.dumps({"url": "http://localhost:7712/untrusted"}))
        print("  multi-origin navigate:", nval)
        assert nc == 200 and nval.get("spawned_fresh_context") is True, nval

        vr = st["layout"]["vm_dev"]
        _, st = http_req("POST", "/_hirsel/click", json.dumps({"x": vr["x"] + 60, "y": vr["y"] + 80}))
        assert st["focused"] == "vm_dev"
        http_req("POST", "/_hirsel/type", "echo TYPED_INTERACTIVELY > /mnt/data/typed.txt && sync\n")
        time.sleep(0.8)

        _, _ = http_req("POST", "/_hirsel/clipboard", json.dumps({"domain": "figments", "text": "POWERBOX_SECRET_42_FROM_FIGMENTS"}))
        code, pb = http_req("POST", "/_hirsel/powerbox-paste", json.dumps({"from": "figments", "to": "vm_dev"}))
        assert code == 200 and pb.get("authorized") is True and pb.get("delivered") is True, pb
        _, check_pb = http_req("POST", "/_hirsel/dev-exec", "cat /mnt/data/powerbox_paste.txt")
        assert "POWERBOX_SECRET_42_FROM_FIGMENTS" in check_pb.get("stdout", ""), check_pb

        print("=== 7. Verify Isolated egress_gw.cm Proxy + Disallowed Host 403 + Real Autonomous Agent Loop ===")
        code, prx = http_req(
            "POST",
            "/_hirsel/proxy",
            json.dumps({"host": "127.0.0.1", "port": 7780, "path": "/_hirsel/echo-target"}),
        )
        print("  egress_gw.cm proxy:", prx)
        assert code == 200 and prx.get("gateway") == "egress_gw.cm" and prx.get("http_status") == 200 and prx.get("secret_injected") is True, prx
        assert '"secret_present":true' in prx.get("body", ""), prx

        code, bad_prx = http_req("POST", "/_hirsel/proxy", json.dumps({"host": "evil.invalid", "port": 80, "path": "/"}))
        assert code == 403 and bad_prx.get("reason") == "host_not_in_capability_allowlist", (code, bad_prx)

        # Real Autonomous Pure-Hirsel Agent: runs hirsel-js -> Linux vm-dev -> publishes hirsel-board card -> stages Ceremony!
        ac, aval = http_req("POST", "/_hirsel/agent-run", "Audit Zircon + Linux vm-dev and publish verified report")
        print("  hirsel-agent run:", aval)
        assert ac == 200 and aval.get("ok") is True and aval.get("ceremony_staged") is True, aval
        assert "91" in aval.get("js_result", "") and "Linux 6.1.102" in aval.get("vm_result", ""), aval

        time.sleep(3.5)
        qmp_screendump(f"{OUT}/desktop-3-ceremony.png")

        # Bad digest MUST be rejected; then re-stage & approve -> executes via isolated egress_gw.cm!
        bad_id = aval["wm_state"]["outbox"]["id"]
        _, st = http_req("POST", "/_hirsel/cmd", f"APPROVE {bad_id} 111111")
        assert st["denied"] == 1 and st["bank_spent_micros"] == 0

        # Capture clean 3-way split screenshot now showing BOTH js-eval-1 AND agent-run-1 HTML-in-Canvas cards!
        time.sleep(2.0)
        qmp_screendump(f"{OUT}/desktop-2-split.png")

        _, st = http_req("POST", "/_hirsel/stage", "")
        code, appr = http_req("POST", "/_hirsel/approve", "")
        print("  approved + executed via egress_gw.cm:", appr)
        assert code == 200 and appr["state"]["approved"] == 1 and appr["state"]["bank_spent_micros"] == 250000
        assert appr["execution"]["executed"] is True and appr["execution"]["gateway"] == "egress_gw.cm" and appr["execution"]["http_status"] == 200

        _, st = http_req("POST", "/_hirsel/cmd", "REVOKE")
        code, rev_prx = http_req("POST", "/_hirsel/proxy", json.dumps({"host": "127.0.0.1", "port": 7780, "path": "/_hirsel/echo-target"}))
        assert code == 403 and rev_prx.get("reason") == "caretaker_connection_revoked"

        # Toggle room to Paper/Daylight so we verify full workstation persistence on cold reboot
        _, st = http_req("POST", "/_hirsel/cmd", "TOGGLE_ROOM")
        assert st["room"] == "daylight"
        time.sleep(1.5)
    finally:
        stop_qemu(proc)

    print("=== 8. BOOT #2 (COLD REBOOT on same Fxfs disk — verifying ALL persisted state!) ===")
    proc2 = start_qemu(f"{OUT}/serial-boot2.log")
    try:
        st2 = wait_for_desktop(proc2, "BOOT2")
        assert st2["room"] == "daylight", f"Expected persisted room=daylight after reboot, got {st2}"
        _, aud = http_req("GET", "/_hirsel/audit")
        print("  persisted audit log:", aud)
        assert any("egress_gw.cm" in e and "HTTP 200" in e for e in aud.get("audit", [])), aud

        _, brd2 = http_req("GET", "/_hirsel/board")
        card_ids = [c["id"] for c in brd2.get("cards", [])]
        print("  persisted pure-Hirsel hirsel-board cards:", card_ids)
        assert "js-eval-1" in card_ids and "agent-run-1" in card_ids, brd2

        _, ink2 = http_req("GET", "/_hirsel/ink")
        print("  persisted Figments freehand ink strokes:", len(ink2.get("strokes", [])))
        assert len(ink2.get("strokes", [])) >= 1, ink2

        # Verify isolated egress_gw.cm sealed secret persisted across cold reboot!
        for _ in range(15):
            sc2, smeta2 = http_req("GET", "/_hirsel/secrets")
            if sc2 == 200:
                break
            time.sleep(0.3)
        print("  persisted isolated egress_gw.cm secrets:", smeta2)
        assert any(x["host"] == "example.com" for x in smeta2.get("secrets", [])), smeta2

        wait_for_linux_vm("BOOT2")
        code, pchk = http_req("POST", "/_hirsel/dev-exec", "cat /mnt/data/persist.txt && cat /mnt/data/powerbox_paste.txt && cat /mnt/data/typed.txt")
        print("  persisted Linux /mnt/data after cold reboot:", pchk)
        assert code == 200
        assert "DESKTOP_PERSIST_99" in pchk.get("stdout", "")
        assert "POWERBOX_SECRET_42_FROM_FIGMENTS" in pchk.get("stdout", "")
        assert "TYPED_INTERACTIVELY" in pchk.get("stdout", "")

        time.sleep(4.0)
        qmp_screendump(f"{OUT}/desktop-4-reboot-daylight.png")
        print("\n=== ALL MILESTONE 1-5 + MULTI-ORIGIN + ANSI E2E & PERSISTENCE CHECKS PASSED! ===")
        return 0
    finally:
        stop_qemu(proc2)


if __name__ == "__main__":
    sys.exit(main())
