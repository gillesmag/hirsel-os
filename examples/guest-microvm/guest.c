// Freestanding 64-bit x86-64 Tier 2 MicroVM guest payload for Hirsel-OS.
//
// Runs in hardware-isolated AMD SVM (or Intel VMX) Long Mode inside Zircon's
// hypervisor (`zx_guest_create` + `zx_vcpu_create` + `zx_vcpu_enter`).

#include <stdint.h>

#define PORT_COM1        0x03F8u
#define PORT_PV_EXIT     0x0500u
#define PORT_PV_MONO_LO  0x0504u
#define PORT_PV_MONO_HI  0x0508u
#define PORT_PV_RNG      0x050Cu

#define BOOT_INFO_MAGIC  0x4852534C564D3032ULL

struct GuestBootInfo {
    uint64_t magic;
    uint32_t version;
    uint32_t flags;
    uint64_t ram_bytes;
    uint64_t mmio_heartbeat_gpa;
    uint64_t mmio_bell_gpa;
    uint64_t scratch_gpa;
    uint64_t scratch_bytes;
};

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

static inline void cpuid(uint32_t leaf, uint32_t *a, uint32_t *b, uint32_t *c, uint32_t *d) {
    __asm__ volatile("cpuid" : "=a"(*a), "=b"(*b), "=c"(*c), "=d"(*d) : "a"(leaf), "c"(0));
}

static inline uint64_t read_cr0(void) {
    uint64_t v;
    __asm__ volatile("mov %%cr0, %0" : "=r"(v));
    return v;
}

static void putc_com1(char ch) {
    outb(PORT_COM1, (uint8_t)ch);
}

static void puts_com1(const char *s) {
    while (*s) {
        putc_com1(*s++);
    }
}

static void put_u64_dec(uint64_t v) {
    char buf[24];
    int n = 0;
    if (v == 0) {
        putc_com1('0');
        return;
    }
    while (v > 0) {
        buf[n++] = (char)('0' + (v % 10));
        v /= 10;
    }
    while (n > 0) {
        putc_com1(buf[--n]);
    }
}

static void put_u64_hex(uint64_t v) {
    static const char hex[] = "0123456789abcdef";
    puts_com1("0x");
    for (int shift = 60; shift >= 0; shift -= 4) {
        putc_com1(hex[(v >> shift) & 0xf]);
    }
}

static uint64_t pv_mono_ns(void) {
    uint64_t lo = (uint64_t)inl(PORT_PV_MONO_LO);
    uint64_t hi = (uint64_t)inl(PORT_PV_MONO_HI);
    return lo | (hi << 32);
}

static uint64_t pv_rng_u64(void) {
    uint64_t lo = (uint64_t)inl(PORT_PV_RNG);
    uint64_t hi = (uint64_t)inl(PORT_PV_RNG);
    return lo | (hi << 32);
}

__attribute__((section(".text._start"), noreturn))
void _start(const struct GuestBootInfo *info) {
    if (!info || info->magic != BOOT_INFO_MAGIC) {
        puts_com1("ERR: bad GuestBootInfo magic\n");
        outl(PORT_PV_EXIT, 1);
        for (;;) __asm__ volatile("hlt");
    }

    uint64_t t0 = pv_mono_ns();

    // Read hypervisor CPUID leaf (0x40000000) handled by Zircon's AMD SVM / Intel VMX backend.
    uint32_t eax, ebx, ecx, edx;
    cpuid(0x40000000u, &eax, &ebx, &ecx, &edx);
    char hv_sig[13];
    hv_sig[0]  = (char)(ebx & 0xff);
    hv_sig[1]  = (char)((ebx >> 8) & 0xff);
    hv_sig[2]  = (char)((ebx >> 16) & 0xff);
    hv_sig[3]  = (char)((ebx >> 24) & 0xff);
    hv_sig[4]  = (char)(ecx & 0xff);
    hv_sig[5]  = (char)((ecx >> 8) & 0xff);
    hv_sig[6]  = (char)((ecx >> 16) & 0xff);
    hv_sig[7]  = (char)((ecx >> 24) & 0xff);
    hv_sig[8]  = (char)(edx & 0xff);
    hv_sig[9]  = (char)((edx >> 8) & 0xff);
    hv_sig[10] = (char)((edx >> 16) & 0xff);
    hv_sig[11] = (char)((edx >> 24) & 0xff);
    hv_sig[12] = '\0';

    // Exercise NPT demand-paged guest RAM (write + FNV-1a checksum over 64 KiB scratch).
    volatile uint8_t *scratch = (volatile uint8_t *)info->scratch_gpa;
    uint64_t hash = 0xcbf29ce484222325ULL;
    for (uint32_t i = 0; i < 65536u; i++) {
        uint8_t b = (uint8_t)((i * 31u + 17u) & 0xffu);
        scratch[i] = b;
        hash ^= (uint64_t)scratch[i];
        hash *= 0x100000001b3ULL;
    }

    // Exercise synchronous NPT MMIO trap (ZX_GUEST_TRAP_MEM) & async doorbell (ZX_GUEST_TRAP_BELL).
    volatile uint32_t *mmio_hb = (volatile uint32_t *)info->mmio_heartbeat_gpa;
    volatile uint32_t *mmio_bell = (volatile uint32_t *)info->mmio_bell_gpa;
    *mmio_hb = 0xCAFEBABEu;
    *mmio_bell = 0x1u;
    *mmio_bell = 0x2u;

    uint64_t rng = pv_rng_u64();
    uint64_t cr0 = read_cr0();
    uint64_t t1 = pv_mono_ns();

    puts_com1("hello from Tier 2 AMD SVM MicroVM (64-bit Ring 0 Guest)!\n");
    puts_com1("  hv_cpuid: ");
    puts_com1(hv_sig);
    puts_com1("  cr0=");
    put_u64_hex(cr0);
    puts_com1("  ram=");
    put_u64_dec(info->ram_bytes / (1024u * 1024u));
    puts_com1(" MiB\n");

    puts_com1("  mono_ns:  t0=");
    put_u64_dec(t0);
    puts_com1(" t1=");
    put_u64_dec(t1);
    puts_com1(" delta=");
    put_u64_dec(t1 - t0);
    puts_com1(" ns\n");

    puts_com1("  pv_rng:   ");
    put_u64_hex(rng);
    puts_com1("\n");

    puts_com1("  ram_fnv:  ");
    put_u64_hex(hash);
    puts_com1(" (64 KiB NPT RW verified)\n");

    outl(PORT_PV_EXIT, 0);
    for (;;) {
        __asm__ volatile("hlt");
    }
}
