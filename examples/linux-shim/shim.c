// Hirsel-OS Tier 2 Linux x86-64 Compatibility Shim (`bin/hirsel-linux-shim.elf`).
//
// Boots in Ring 0 inside Zircon's hardware-virtualized AMD SVM / Intel VMX microVM,
// sets up Ring-0/Ring-3 GDT + TSS + zero-exit SYSCALL/SYSRET MSRs, maps an unmodified
// Linux x86-64 static ELF into Ring-3 user pages ([4 MiB .. RAM)), enters Ring 3 via
// `iretq`, and emulates Linux x86-64 syscalls over Hirsel-OS paravirt ports.

#include <stdint.h>
#include <stddef.h>

#define PORT_COM1        0x03F8u
#define PORT_PV_EXIT     0x0500u
#define PORT_PV_MONO_LO  0x0504u
#define PORT_PV_MONO_HI  0x0508u
#define PORT_PV_RNG      0x050Cu

#define BOOT_INFO_MAGIC  0x4852534C564D3032ULL

#define MSR_EFER         0xC0000080u
#define MSR_STAR         0xC0000081u
#define MSR_LSTAR        0xC0000082u
#define MSR_FMASK        0xC0000084u
#define MSR_FS_BASE      0xC0000100u

// Linux x86-64 syscall numbers.
#define SYS_read            0
#define SYS_write           1
#define SYS_close           3
#define SYS_mmap            9
#define SYS_munmap          11
#define SYS_brk             12
#define SYS_writev          20
#define SYS_getpid          39
#define SYS_exit            60
#define SYS_uname           63
#define SYS_arch_prctl      158
#define SYS_gettid          186
#define SYS_set_tid_address 218
#define SYS_clock_gettime   228
#define SYS_exit_group      231
#define SYS_getrandom       318

#define ARCH_SET_FS         0x1002
#define ENOSYS              38
#define EINVAL              22
#define EBADF               9
#define ENOMEM              12

struct GuestBootInfo {
    uint64_t magic;
    uint32_t version;
    uint32_t flags;
    uint64_t ram_bytes;
    uint64_t mmio_heartbeat_gpa;
    uint64_t mmio_bell_gpa;
    uint64_t scratch_gpa;
    uint64_t scratch_bytes;
    uint64_t aux_gpa;
    uint64_t aux_bytes;
    uint64_t virtio_con_gpa;
};

struct Elf64Ehdr {
    uint8_t  e_ident[16];
    uint16_t e_type;
    uint16_t e_machine;
    uint32_t e_version;
    uint64_t e_entry;
    uint64_t e_phoff;
    uint64_t e_shoff;
    uint32_t e_flags;
    uint16_t e_ehsize;
    uint16_t e_phentsize;
    uint16_t e_phnum;
    uint16_t e_shentsize;
    uint16_t e_shnum;
    uint16_t e_shstrndx;
};

struct Elf64Phdr {
    uint32_t p_type;
    uint32_t p_flags;
    uint64_t p_offset;
    uint64_t p_vaddr;
    uint64_t p_paddr;
    uint64_t p_filesz;
    uint64_t p_memsz;
    uint64_t p_align;
};

struct SyscallFrame {
    uint64_t nr;
    uint64_t rdi;
    uint64_t rsi;
    uint64_t rdx;
    uint64_t r10;
    uint64_t r8;
    uint64_t r9;
    uint64_t rbx;
    uint64_t rbp;
    uint64_t r12;
    uint64_t r13;
    uint64_t r14;
    uint64_t r15;
    uint64_t user_rip;
    uint64_t user_rflags;
};

struct LinuxIovec {
    const uint8_t *iov_base;
    uint64_t iov_len;
};

struct LinuxTimespec {
    int64_t tv_sec;
    int64_t tv_nsec;
};

struct LinuxUtsname {
    char sysname[65];
    char nodename[65];
    char release[65];
    char version[65];
    char machine[65];
    char domainname[65];
};

struct __attribute__((packed)) Gdtr {
    uint16_t limit;
    uint64_t base;
};

struct __attribute__((packed)) Tss64 {
    uint32_t reserved0;
    uint64_t rsp0;
    uint64_t rsp1;
    uint64_t rsp2;
    uint64_t reserved1;
    uint64_t ist[7];
    uint64_t reserved2;
    uint16_t reserved3;
    uint16_t iopb_offset;
};

extern void load_gdt_and_tss(const struct Gdtr *gdtr, uint16_t tss_sel);
extern void enter_ring3(uint64_t user_rip, uint64_t user_rsp) __attribute__((noreturn));
extern void syscall_entry(void);

static uint64_t g_gdt[7] __attribute__((aligned(16)));
static struct Tss64 g_tss __attribute__((aligned(16)));
static uint8_t g_kstack[16384] __attribute__((aligned(16)));

uint64_t g_kernel_rsp = 0;
uint64_t g_user_rsp = 0;

static uint64_t g_ram_bytes = 0;
static uint64_t g_brk_base = 0;
static uint64_t g_brk_cur = 0;
static uint64_t g_mmap_cur = 0;

static inline void outb(uint16_t port, uint8_t val) {
    __asm__ volatile("outb %0, %1" :: "a"(val), "Nd"(port));
}

static inline void outl(uint16_t port, uint32_t val) {
    __asm__ volatile("outl %0, %1" :: "a"(val), "Nd"(port));
}

static inline uint32_t inl(uint16_t port) {
    uint32_t val;
    __asm__ volatile("inl %1, %0" : "=a"(val) : "Nd"(port));
    return val;
}

static inline uint64_t rdmsr(uint32_t msr) {
    uint32_t lo, hi;
    __asm__ volatile("rdmsr" : "=a"(lo), "=d"(hi) : "c"(msr));
    return ((uint64_t)hi << 32) | lo;
}

static inline void wrmsr(uint32_t msr, uint64_t val) {
    __asm__ volatile("wrmsr" :: "c"(msr), "a"((uint32_t)val), "d"((uint32_t)(val >> 32)));
}

static void putc_com1(char c) {
    outb(PORT_COM1, (uint8_t)c);
}

static void puts_com1(const char *s) {
    while (*s) {
        putc_com1(*s++);
    }
}

static void *kmemset(void *dst, int c, size_t n) {
    uint8_t *p = (uint8_t *)dst;
    for (size_t i = 0; i < n; i++) p[i] = (uint8_t)c;
    return dst;
}

static void *kmemcpy(void *dst, const void *src, size_t n) {
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    for (size_t i = 0; i < n; i++) d[i] = s[i];
    return dst;
}

static void kstrcpy(char *dst, const char *src, size_t cap) {
    size_t i = 0;
    while (i + 1 < cap && src[i]) {
        dst[i] = src[i];
        i++;
    }
    dst[i] = '\0';
}

static uint64_t pv_mono_ns(void) {
    uint64_t lo = (uint64_t)inl(PORT_PV_MONO_LO);
    uint64_t hi = (uint64_t)inl(PORT_PV_MONO_HI);
    return lo | (hi << 32);
}

static void pv_fill_random(uint8_t *dst, size_t len) {
    size_t i = 0;
    while (i < len) {
        uint32_t w = inl(PORT_PV_RNG);
        for (int b = 0; b < 4 && i < len; b++, i++) {
            dst[i] = (uint8_t)((w >> (b * 8)) & 0xffu);
        }
    }
}

static void vm_exit(int32_t code) __attribute__((noreturn));
static void vm_exit(int32_t code) {
    outl(PORT_PV_EXIT, (uint32_t)code);
    for (;;) {
        __asm__ volatile("hlt");
    }
}

static int valid_user_range(uint64_t addr, uint64_t len) {
    if (addr < 0x00400000ULL) return 0;
    if (addr + len < addr) return 0;
    return (addr + len) <= g_ram_bytes;
}

uint64_t linux_syscall_dispatch(struct SyscallFrame *f) {
    switch (f->nr) {
        case SYS_read:
            return 0; // EOF
        case SYS_write: {
            int fd = (int)f->rdi;
            uint64_t buf = f->rsi;
            uint64_t len = f->rdx;
            if (fd != 1 && fd != 2) return (uint64_t)-EBADF;
            if (len > 0 && !valid_user_range(buf, len)) return (uint64_t)-EINVAL;
            const uint8_t *p = (const uint8_t *)buf;
            for (uint64_t i = 0; i < len; i++) {
                outb(PORT_COM1, p[i]);
            }
            return len;
        }
        case SYS_writev: {
            int fd = (int)f->rdi;
            uint64_t iov_addr = f->rsi;
            uint64_t iovcnt = f->rdx;
            if (fd != 1 && fd != 2) return (uint64_t)-EBADF;
            if (iovcnt > 64) return (uint64_t)-EINVAL;
            if (!valid_user_range(iov_addr, iovcnt * sizeof(struct LinuxIovec))) {
                return (uint64_t)-EINVAL;
            }
            const struct LinuxIovec *iov = (const struct LinuxIovec *)iov_addr;
            uint64_t total = 0;
            for (uint64_t i = 0; i < iovcnt; i++) {
                uint64_t base = (uint64_t)iov[i].iov_base;
                uint64_t len = iov[i].iov_len;
                if (len == 0) continue;
                if (!valid_user_range(base, len)) return (uint64_t)-EINVAL;
                for (uint64_t j = 0; j < len; j++) {
                    outb(PORT_COM1, iov[i].iov_base[j]);
                }
                total += len;
            }
            return total;
        }
        case SYS_close:
            return 0;
        case SYS_brk: {
            uint64_t req = f->rdi;
            if (req == 0) return g_brk_cur;
            uint64_t aligned = (req + 0xfffULL) & ~0xfffULL;
            if (aligned >= g_brk_base && aligned < g_mmap_cur - 0x10000ULL) {
                if (aligned > g_brk_cur) {
                    kmemset((void *)g_brk_cur, 0, (size_t)(aligned - g_brk_cur));
                }
                g_brk_cur = aligned;
            }
            return g_brk_cur;
        }
        case SYS_mmap: {
            uint64_t len = (f->rsi + 0xfffULL) & ~0xfffULL;
            if (len == 0 || g_mmap_cur - len <= g_brk_cur + 0x10000ULL) {
                return (uint64_t)-ENOMEM;
            }
            g_mmap_cur -= len;
            kmemset((void *)g_mmap_cur, 0, (size_t)len);
            return g_mmap_cur;
        }
        case SYS_munmap:
            return 0;
        case SYS_arch_prctl: {
            if (f->rdi == ARCH_SET_FS) {
                wrmsr(MSR_FS_BASE, f->rsi);
                return 0;
            }
            return (uint64_t)-EINVAL;
        }
        case SYS_getpid:
        case SYS_gettid:
        case SYS_set_tid_address:
            return 1;
        case SYS_uname: {
            uint64_t dst = f->rdi;
            if (!valid_user_range(dst, sizeof(struct LinuxUtsname))) return (uint64_t)-EINVAL;
            struct LinuxUtsname *u = (struct LinuxUtsname *)dst;
            kmemset(u, 0, sizeof(*u));
            kstrcpy(u->sysname, "Linux", sizeof(u->sysname));
            kstrcpy(u->nodename, "hirsel-microvm", sizeof(u->nodename));
            kstrcpy(u->release, "6.12.0-hirsel-svm", sizeof(u->release));
            kstrcpy(u->version, "#1 Hirsel-OS Zircon Tier-2 Ring-3", sizeof(u->version));
            kstrcpy(u->machine, "x86_64", sizeof(u->machine));
            return 0;
        }
        case SYS_clock_gettime: {
            uint64_t dst = f->rsi;
            if (!valid_user_range(dst, sizeof(struct LinuxTimespec))) return (uint64_t)-EINVAL;
            uint64_t ns = pv_mono_ns();
            struct LinuxTimespec *ts = (struct LinuxTimespec *)dst;
            ts->tv_sec = (int64_t)(ns / 1000000000ULL);
            ts->tv_nsec = (int64_t)(ns % 1000000000ULL);
            return 0;
        }
        case SYS_getrandom: {
            uint64_t dst = f->rdi;
            uint64_t len = f->rsi;
            if (len > 4096) len = 4096;
            if (len > 0 && !valid_user_range(dst, len)) return (uint64_t)-EINVAL;
            pv_fill_random((uint8_t *)dst, (size_t)len);
            return len;
        }
        case SYS_exit:
        case SYS_exit_group:
            vm_exit((int32_t)f->rdi);
        default:
            return (uint64_t)-ENOSYS;
    }
}

static void setup_cpu_ring3_and_syscalls(void) {
    // Enable SSE/FXSR so unmodified Linux user binaries using XMM work cleanly.
    uint64_t cr0, cr4;
    __asm__ volatile("mov %%cr0, %0" : "=r"(cr0));
    cr0 = (cr0 & ~(1ULL << 2)) | (1ULL << 1); // clear EM, set MP
    __asm__ volatile("mov %0, %%cr0" :: "r"(cr0));
    __asm__ volatile("mov %%cr4, %0" : "=r"(cr4));
    cr4 |= (1ULL << 9) | (1ULL << 10); // OSFXSR | OSXMMEXCPT
    __asm__ volatile("mov %0, %%cr4" :: "r"(cr4));

    // Set up kernel syscall stack and TSS64.
    g_kernel_rsp = ((uint64_t)(g_kstack + sizeof(g_kstack))) & ~0xfULL;
    kmemset(&g_tss, 0, sizeof(g_tss));
    g_tss.rsp0 = g_kernel_rsp;
    g_tss.iopb_offset = sizeof(g_tss);

    // Build 64-bit GDT with textbook SYSCALL/SYSRET descriptor order:
    // 0x00: null, 0x08: KCS, 0x10: KDS, 0x18: UDS, 0x20: UCS, 0x28..0x38: TSS64.
    g_gdt[0] = 0;
    g_gdt[1] = 0x00AF9A000000FFFFULL; // 0x08 Kernel CS (L=1, DPL=0)
    g_gdt[2] = 0x00CF92000000FFFFULL; // 0x10 Kernel DS (DPL=0)
    g_gdt[3] = 0x00CFF2000000FFFFULL; // 0x18 User DS (DPL=3 -> sel 0x1B)
    g_gdt[4] = 0x00AFFA000000FFFFULL; // 0x20 User CS (L=1, DPL=3 -> sel 0x23)

    uint64_t tss_base = (uint64_t)&g_tss;
    uint64_t tss_limit = sizeof(g_tss) - 1;
    g_gdt[5] = (tss_limit & 0xffffULL)
             | ((tss_base & 0xffffffULL) << 16)
             | (0x89ULL << 40) // Present, Available 64-bit TSS
             | (((tss_limit >> 16) & 0xfULL) << 48)
             | (((tss_base >> 24) & 0xffULL) << 56);
    g_gdt[6] = (tss_base >> 32) & 0xffffffffULL;

    struct Gdtr gdtr;
    gdtr.limit = sizeof(g_gdt) - 1;
    gdtr.base = (uint64_t)g_gdt;
    load_gdt_and_tss(&gdtr, 0x28);

    // Enable EFER.SCE and program STAR/LSTAR/FMASK (zero-exit under AMD SVM!).
    uint64_t efer = rdmsr(MSR_EFER);
    wrmsr(MSR_EFER, efer | 1ULL);
    wrmsr(MSR_STAR, (0x0010ULL << 48) | (0x0008ULL << 32));
    wrmsr(MSR_LSTAR, (uint64_t)syscall_entry);
    wrmsr(MSR_FMASK, 0x200ULL); // clear IF on syscall entry
}

static void enable_user_pages(uint64_t ram_bytes) {
    // Allow Ring-3 access to [4 MiB .. ram_bytes) while keeping [0 .. 4 MiB) supervisor-only!
    volatile uint64_t *pml4 = (volatile uint64_t *)0x0000ULL;
    volatile uint64_t *pdpt = (volatile uint64_t *)0x1000ULL;
    volatile uint64_t *pd_low = (volatile uint64_t *)0x2000ULL;
    pml4[0] |= 0x4ULL; // User
    pdpt[0] |= 0x4ULL; // User
    uint64_t num_2m = ram_bytes >> 21;
    for (uint64_t i = 2; i < num_2m && i < 512; i++) {
        pd_low[i] |= 0x4ULL; // P | W | U | PS
    }
    uint64_t cr3;
    __asm__ volatile("mov %%cr3, %0; mov %0, %%cr3" : "=r"(cr3) :: "memory");
}

__attribute__((section(".text._start"), noreturn))
void _start(const struct GuestBootInfo *info) {
    if (!info || info->magic != BOOT_INFO_MAGIC || info->aux_gpa == 0 || info->aux_bytes < sizeof(struct Elf64Ehdr)) {
        puts_com1("[linux-shim] ERR: missing Linux static ELF payload in GuestBootInfo\n");
        vm_exit(1);
    }

    g_ram_bytes = info->ram_bytes;
    setup_cpu_ring3_and_syscalls();
    enable_user_pages(g_ram_bytes);

    // Parse unmodified Linux x86-64 static ELF at info->aux_gpa.
    const uint8_t *elf = (const uint8_t *)info->aux_gpa;
    const struct Elf64Ehdr *eh = (const struct Elf64Ehdr *)elf;
    if (eh->e_ident[0] != 0x7f || eh->e_ident[1] != 'E' || eh->e_ident[2] != 'L' || eh->e_ident[3] != 'F'
        || eh->e_ident[4] != 2 || eh->e_machine != 62) {
        puts_com1("[linux-shim] ERR: payload is not an x86-64 ELF64 binary\n");
        vm_exit(2);
    }

    // Relocate ET_DYN (PIE) to 0x0040_0000; keep ET_EXEC at its native p_vaddr (>= 0x0040_0000).
    uint64_t load_bias = (eh->e_type == 3) ? 0x00400000ULL : 0ULL;
    uint64_t max_end = 0x00400000ULL;

    for (uint16_t i = 0; i < eh->e_phnum; i++) {
        const struct Elf64Phdr *ph = (const struct Elf64Phdr *)(elf + eh->e_phoff + (uint64_t)i * eh->e_phentsize);
        if (ph->p_type != 1 || ph->p_memsz == 0) continue; // PT_LOAD
        uint64_t dst = ph->p_vaddr + load_bias;
        if (!valid_user_range(dst, ph->p_memsz) || ph->p_offset + ph->p_filesz > info->aux_bytes) {
            puts_com1("[linux-shim] ERR: PT_LOAD segment outside user RAM [4 MiB .. RAM)\n");
            vm_exit(3);
        }
        kmemcpy((void *)dst, elf + ph->p_offset, (size_t)ph->p_filesz);
        if (ph->p_memsz > ph->p_filesz) {
            kmemset((void *)(dst + ph->p_filesz), 0, (size_t)(ph->p_memsz - ph->p_filesz));
        }
        if (dst + ph->p_memsz > max_end) {
            max_end = dst + ph->p_memsz;
        }
    }

    g_brk_base = (max_end + 0xfffULL) & ~0xfffULL;
    g_brk_cur = g_brk_base;

    // Reserve top 256 KiB of guest RAM for Linux Ring-3 user stack, mmap grows below stack.
    uint64_t stack_top = (g_ram_bytes - 0x4000ULL) & ~0xfULL;
    g_mmap_cur = (g_ram_bytes - 0x40000ULL) & ~0xfffULL;

    // Populate initial Linux x86-64 user stack (strings + 16-byte AT_RANDOM + argc/argv/envp/auxv).
    uint64_t sp = stack_top;
    sp -= 16;
    uint64_t rand_ptr = sp;
    pv_fill_random((uint8_t *)rand_ptr, 16);

    const char arg0[] = "hello-linux";
    const char env0[] = "HIRSEL_TIER=2";
    sp -= sizeof(arg0);
    uint64_t arg0_ptr = sp;
    kmemcpy((void *)arg0_ptr, arg0, sizeof(arg0));

    sp -= sizeof(env0);
    uint64_t env0_ptr = sp;
    kmemcpy((void *)env0_ptr, env0, sizeof(env0));

    sp &= ~0xfULL;
    // Push 12 qwords (even count -> 16-byte aligned RSP at _start per System V AMD64 ABI).
    uint64_t frame[12] = {
        1,          // argc
        arg0_ptr,   // argv[0]
        0,          // argv[1] = NULL
        env0_ptr,   // envp[0]
        0,          // envp[1] = NULL
        6, 4096,    // AT_PAGESZ = 4096
        25, rand_ptr, // AT_RANDOM
        0, 0,       // AT_NULL
        0,
    };
    sp -= sizeof(frame);
    kmemcpy((void *)sp, frame, sizeof(frame));

    uint64_t entry = eh->e_entry + load_bias;
    enter_ring3(entry, sp);
}
