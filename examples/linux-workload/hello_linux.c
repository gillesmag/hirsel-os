// Unmodified Linux x86-64 static ELF workload.
//
// Uses ONLY standard Linux x86-64 `syscall` ABI (SYS_uname, SYS_clock_gettime,
// SYS_getrandom, SYS_brk, SYS_write, SYS_exit_group). Runs identically:
//   1. Natively on Linux (`./out/hello-linux`)
//   2. Inside Hirsel-OS Tier 2 AMD SVM Ring 3 (`hirsel-os-ctl run --tier 2 --linux-elf out/hello-linux`)

#include <stdint.h>
#include <stddef.h>

#define SYS_write         1
#define SYS_brk           12
#define SYS_uname         63
#define SYS_clock_gettime 228
#define SYS_exit_group    231
#define SYS_getrandom     318

#define CLOCK_MONOTONIC   1

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

static void run_main(void) {
    struct utsname_linux u;
    sys1(SYS_uname, (long)&u);

    struct timespec_linux t0 = {0, 0}, t1 = {0, 0};
    sys2(SYS_clock_gettime, CLOCK_MONOTONIC, (long)&t0);

    uint64_t rand_val = 0;
    sys3(SYS_getrandom, (long)&rand_val, sizeof(rand_val), 0);

    // Exercise Linux brk() heap allocation in Ring 3.
    long brk0 = sys1(SYS_brk, 0);
    long brk1 = sys1(SYS_brk, brk0 + 4096);
    uint64_t heap_sum = 0;
    if (brk1 >= brk0 + 4096) {
        volatile uint8_t *heap = (volatile uint8_t *)brk0;
        for (int i = 0; i < 256; i++) {
            heap[i] = (uint8_t)i;
            heap_sum += heap[i];
        }
    }

    sys2(SYS_clock_gettime, CLOCK_MONOTONIC, (long)&t1);
    uint64_t dt_ns = (uint64_t)((t1.tv_sec - t0.tv_sec) * 1000000000LL + (t1.tv_nsec - t0.tv_nsec));

    write_str("hello from UNMODIFIED Linux x86-64 static ELF in Ring 3!\n");
    write_str("  uname:     ");
    write_str(u.sysname);
    write_str(" ");
    write_str(u.nodename);
    write_str(" ");
    write_str(u.release);
    write_str(" ");
    write_str(u.machine);
    write_str("\n");
    write_str("  getrandom: ");
    write_hex64(rand_val);
    write_str("\n");
    write_str("  brk heap:  ");
    write_hex64((uint64_t)brk0);
    write_str(" -> ");
    write_hex64((uint64_t)brk1);
    write_str(" sum(0..255)=");
    write_u64(heap_sum);
    write_str("\n");
    write_str("  clock_ns:  delta=");
    write_u64(dt_ns);
    write_str(" ns\n");

    sys1(SYS_exit_group, 0);
    for (;;) {}
}

__attribute__((naked)) void _start(void) {
    __asm__ volatile(
        "and $-16, %%rsp\n"
        "call %P0\n"
        :: "i"(run_main)
    );
}
