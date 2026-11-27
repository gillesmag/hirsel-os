# Declarative Hirsel-OS Tier 2 Linux MicroVM Specification
# Launch with:
#   just ctl vm start --spec examples/linux-vm/vm.spec
# SSH into the running VM:
#   just ctl vm ssh dev-1
#   # or directly: ssh -i out/linux/id_ed25519 -p 2222 root@127.0.0.1

name = "dev-1"
kernel = "out/linux/vmlinux.stripped"
initrd = "out/linux/initramfs-ssh.cpio"
ram_mib = 128
vcpus = 2
guest_ip = "10.0.3.2/24"
gateway_ip = "10.0.3.1"
ssh_port = 2222
cmdline = "console=ttyS0 reboot=k panic=1 pci=off nomodules quiet"
