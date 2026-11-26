// Minimal static Linux x86-64 PID 1 (`/init`) for Hirsel-OS Tier 2 Linux MicroVMs.
//
// Executed by a REAL upstream Linux kernel inside Zircon AMD SVM, prints kernel
// identity, PID, monotonic clock, and kernel CSPRNG entropy, then triggers a clean
// microVM exit via `SYS_reboot(LINUX_REBOOT_CMD_RESTART)` (`reboot=k` -> i8042 0x64=0xFE).

#include <stdint.h>
#include <stddef.h>

#define SYS_write         1
#define SYS_open          2
#define SYS_ioctl         16
#define SYS_dup2          33
#define SYS_getpid        39
#define SYS_uname         63
#define SYS_mount         165
#define SYS_reboot        169
#define SYS_clock_gettime 228
#define SYS_exit_group    231
#define SYS_getrandom     318

#define O_RDWR            2
#define TCSBRK            0x5409
#define CLOCK_MONOTONIC   1

#define LINUX_REBOOT_MAGIC1       0xfee1deadL
#define LINUX_REBOOT_MAGIC2       672274793L
#define LINUX_REBOOT_CMD_RESTART  0x01234567L

struct timespec_linux {
    int64_t tv_sec;
    int64_t tv_nsec;
};

struct utsname_linux {
    char sysname[65];
    char nodename[65];
    char release[65];
    char version[65];
    char machine[65];
    char domainname[65];
};

static inline long sys0(long nr) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr) : "rcx", "r11", "memory");
    return ret;
}

static inline long sys1(long nr, long a1) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a1) : "rcx", "r11", "memory");
    return ret;
}

static inline long sys2(long nr, long a1, long a2) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a1), "S"(a2) : "rcx", "r11", "memory");
    return ret;
}

static inline long sys3(long nr, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a1), "S"(a2), "d"(a3) : "rcx", "r11", "memory");
    return ret;
}

static inline long sys4(long nr, long a1, long a2, long a3, long a4) {
    long ret;
    register long r10 __asm__("r10") = a4;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(r10) : "rcx", "r11", "memory");
    return ret;
}

static inline long sys5(long nr, long a1, long a2, long a3, long a4, long a5) {
    long ret;
    register long r10 __asm__("r10") = a4;
    register long r8 __asm__("r8") = a5;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8) : "rcx", "r11", "memory");
    return ret;
}

static size_t slen(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}

static void write_str(const char *s) {
    sys3(SYS_write, 1, (long)s, (long)slen(s));
}

static void write_u64(uint64_t v) {
    char buf[24];
    int n = 0;
    if (v == 0) {
        write_str("0");
        return;
    }
    while (v > 0) {
        buf[n++] = (char)('0' + (v % 10));
        v /= 10;
    }
    char rev[24];
    for (int i = 0; i < n; i++) rev[i] = buf[n - 1 - i];
    sys3(SYS_write, 1, (long)rev, n);
}

static void write_hex64(uint64_t v) {
    static const char hex[] = "0123456789abcdef";
    char buf[18];
    buf[0] = '0';
    buf[1] = 'x';
    for (int i = 0; i < 16; i++) {
        buf[2 + i] = hex[(v >> (60 - i * 4)) & 0xf];
    }
    sys3(SYS_write, 1, (long)buf, 18);
}

static void run_init(void) {
    // Ensure fd 1 is connected to /dev/console (works even if initramfs omitted /dev/console).
    if (sys3(SYS_write, 1, (long)"", 0) < 0) {
        sys5(SYS_mount, (long)"devtmpfs", (long)"/dev", (long)"devtmpfs", 0, 0);
        long fd = sys3(SYS_open, (long)"/dev/console", O_RDWR, 0);
        if (fd >= 0 && fd != 1) {
            sys2(SYS_dup2, fd, 1);
            sys2(SYS_dup2, fd, 2);
        }
    }

    struct utsname_linux u;
    sys1(SYS_uname, (long)&u);

    long pid = sys0(SYS_getpid);

    struct timespec_linux ts = {0, 0};
    sys2(SYS_clock_gettime, CLOCK_MONOTONIC, (long)&ts);

    uint64_t rnd = 0;
    sys3(SYS_getrandom, (long)&rnd, sizeof(rnd), 0);

    write_str("\n============================================================\n");
    write_str("[hirsel-linux-init] REAL LINUX KERNEL BOOTED TO PID 1!\n");
    write_str("  uname:     ");
    write_str(u.sysname);
    write_str(" ");
    write_str(u.release);
    write_str(" ");
    write_str(u.machine);
    write_str("\n  version:   ");
    write_str(u.version);
    write_str("\n  pid:       ");
    write_u64((uint64_t)pid);
    write_str("\n  uptime_ms: ");
    write_u64((uint64_t)ts.tv_sec * 1000ULL + (uint64_t)ts.tv_nsec / 1000000ULL);
    write_str("\n  getrandom: ");
    write_hex64(rnd);
    write_str("\n============================================================\n");

    // Drain TTY TX buffer (tcdrain) before triggering clean `reboot=k` exit.
    sys3(SYS_ioctl, 1, TCSBRK, 1);

    // Trigger clean `reboot=k` microVM exit via i8042 reset (port 0x64 = 0xFE).
    sys4(SYS_reboot, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2, LINUX_REBOOT_CMD_RESTART, 0);
    sys1(SYS_exit_group, 0);
    for (;;) {}
}

__attribute__((naked)) void _start(void) {
    __asm__ volatile(
        "and $-16, %%rsp\n"
        "call %P0\n"
        :: "i"(run_init)
    );
}
