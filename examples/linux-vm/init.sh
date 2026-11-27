#!/bin/sh
export PATH=/bin:/sbin:/usr/bin:/usr/sbin
export HOME=/root
export TERM=xterm

mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
mkdir -p /dev/pts /dev/shm /run /tmp /var/log /mnt/data
mount -t devpts devpts /dev/pts 2>/dev/null || true
mount -t tmpfs tmpfs /tmp 2>/dev/null || true
mount -t tmpfs tmpfs /run 2>/dev/null || true

VM_IP="10.0.3.2/24"
VM_GW="10.0.3.1"
VM_DNS="10.0.2.3"
VM_NAME="hirsel-vm"

for tok in $(cat /proc/cmdline); do
  case "$tok" in
    hirsel.ip=*)   VM_IP="${tok#hirsel.ip=}" ;;
    hirsel.gw=*)   VM_GW="${tok#hirsel.gw=}" ;;
    hirsel.dns=*)  VM_DNS="${tok#hirsel.dns=}" ;;
    hirsel.name=*) VM_NAME="${tok#hirsel.name=}" ;;
  esac
done

hostname "$VM_NAME"
echo "127.0.0.1 localhost $VM_NAME" > /etc/hosts
printf "nameserver %s\nnameserver 1.1.1.1\n" "$VM_DNS" > /etc/resolv.conf

ip link set lo up 2>/dev/null || true
ip addr add 127.0.0.1/8 dev lo 2>/dev/null || true

# Auto-mount persistent Virtio-Blk volume (/dev/vda) on /mnt/data if attached.
if [ -b /dev/vda ]; then
  if mount /dev/vda /mnt/data 2>/dev/null; then
    echo "[hirsel-vm] mounted existing persistent volume /dev/vda on /mnt/data"
  elif command -v mke2fs >/dev/null 2>&1; then
    mke2fs -q -F /dev/vda >/dev/null 2>&1 || true
    if mount /dev/vda /mnt/data 2>/dev/null; then
      echo "[hirsel-vm] formatted & mounted fresh persistent volume /dev/vda (ext2) on /mnt/data"
    fi
  fi
fi

# Configure virtio-net eth0 if present.
if ip link show eth0 >/dev/null 2>&1; then
  ip addr flush dev eth0 2>/dev/null || true
  ip addr add "$VM_IP" dev eth0
  ip link set eth0 up
  ip route replace default via "$VM_GW" dev eth0 2>/dev/null || true
fi

# Start Dropbear SSH server on port 22 (Ed25519 key auth via /root/.ssh/authorized_keys).
if [ -x /sbin/dropbear ]; then
  /sbin/dropbear -E -p 22 -r /etc/dropbear/dropbear_ed25519_host_key
fi

# Start Virtio-Console (/dev/hvc0) guest-agent loop for network-less `hirsel-os-ctl vm exec`.
if [ -c /dev/hvc0 ]; then
  (
    exec 3<>/dev/hvc0
    stty raw -echo <&3 2>/dev/null || true
    while IFS= read -r line <&3; do
      case "$line" in
        HREXEC\ *)
          b64cmd="${line#HREXEC }"
          cmd="$(printf '%s' "$b64cmd" | base64 -d 2>/dev/null)"
          out="$(/bin/sh -c "$cmd" 2>&1)"
          rc=$?
          b64out="$(printf '%s' "$out" | base64 | tr -d '\n')"
          printf 'HRDONE %d %s\n' "$rc" "$b64out" >&3
          ;;
      esac
    done
  ) &
fi

echo "============================================================"
echo "[hirsel-vm] Linux $(uname -r) ready (host=$VM_NAME, cpus=$(nproc 2>/dev/null || echo 1))"
ip -o -4 addr show 2>/dev/null | sed 's/^/[hirsel-vm] /'
[ -c /dev/hvc0 ] && echo "[hirsel-vm] virtio-console guest agent active on /dev/hvc0"
[ -b /dev/vda ] && echo "[hirsel-vm] virtio-blk attached at /dev/vda"
echo "[hirsel-vm] SSH daemon listening on 0.0.0.0:22"
echo "============================================================"

# Keep PID 1 running forever, serving an interactive shell on /dev/console.
while true; do
  /bin/sh </dev/console >/dev/console 2>&1 || sleep 1
done
