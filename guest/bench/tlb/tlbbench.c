/* Bridge-V TLB microbenchmark (P7.9), bare metal (riscv-tests benchmarks/common runtime).
 *
 * Maps 4096 data pages at VA 0x4000_0000 with Sv39 4 KiB pages (plus an identity 1 GiB page
 * for code, stack and tohost), switches to S-mode, and times four kernels with rdtime (10 MHz):
 *   chase  N dependent loads following a ring of pointers, one node per page
 *          (latency: address -> TLB -> load -> next address);
 *   stream N independent loads, one per page, walking the pages in order (throughput);
 * each over W = 8 pages (every access hits the 256-entry direct-mapped TLB after warm-up)
 * and W = 4096 pages (consecutive visits to one TLB slot are different pages: every access
 * misses and walks). Each kernel has a baseline with the same loop and ALU work but no load,
 * which is subtracted. Output: ns per access (and per-kernel raw times) via HTIF printf.
 */
#include <stdint.h>

int printf(const char *fmt, ...);
void exit(int code);

#define PAGES 4096
#define VBASE 0x40000000UL
#define N_ACC 4000000L

#define PTE_V 0x1
#define PTE_R 0x2
#define PTE_W 0x4
#define PTE_X 0x8
#define PTE_A 0x40
#define PTE_D 0x80

static uint64_t root[512] __attribute__((aligned(4096)));
static uint64_t l1[512] __attribute__((aligned(4096)));
static uint64_t l0[PAGES / 512][512] __attribute__((aligned(4096)));
static uint8_t data[PAGES][4096] __attribute__((aligned(4096)));

static inline uint64_t rdtime(void) {
  uint64_t t;
  asm volatile("rdtime %0" : "=r"(t));
  return t;
}

/* Node of page p: spread over cache sets so the data footprint is one line per page. */
static inline uint64_t node_off(uint64_t p) { return (p * 64) % 4096; }
static inline uint64_t node_va(uint64_t p) { return VBASE + p * 4096 + node_off(p); }

static void build_ring(uint64_t w) {
  for (uint64_t p = 0; p < w; p++) *(volatile uint64_t *)node_va(p) = node_va((p + 1) % w);
}

static uint64_t __attribute__((noinline)) chase(uint64_t w, long n) {
  uint64_t a = node_va(0);
  for (long i = 0; i < n; i++) a = *(volatile uint64_t *)a;
  return a + w;
}

static uint64_t __attribute__((noinline)) chase_base(uint64_t w, long n) {
  uint64_t a = node_va(0);
  for (long i = 0; i < n; i++) {
    a = a + 4096 + 64; /* same dependent chain length (one ALU op) */
    asm volatile("" : "+r"(a));
  }
  return a + w;
}

static uint64_t __attribute__((noinline)) stream(uint64_t w, long n) {
  uint64_t sum = 0, p = 0;
  for (long i = 0; i < n; i++) {
    sum += *(volatile uint64_t *)(VBASE + p * 4096 + node_off(p));
    p = (p + 1) & (w - 1);
  }
  return sum;
}

static uint64_t __attribute__((noinline)) stream_base(uint64_t w, long n) {
  uint64_t sum = 0, p = 0;
  for (long i = 0; i < n; i++) {
    uint64_t a = VBASE + p * 4096 + node_off(p);
    asm volatile("" : "+r"(a));
    sum += a;
    p = (p + 1) & (w - 1);
  }
  return sum;
}

static volatile uint64_t sink;

static uint64_t timed(uint64_t (*f)(uint64_t, long), uint64_t w) {
  sink += f(w, N_ACC / 16); /* warm-up: fills the TLB (hit case), translates the code */
  uint64_t t0 = rdtime();
  sink += f(w, N_ACC);
  return rdtime() - t0;
}

static void report(const char *name, uint64_t w, uint64_t t, uint64_t tb) {
  /* 10 MHz ticks: 100 ns each. Print ns per access with two decimals. */
  int64_t d = (int64_t)(t - tb) * 100 * 100 / N_ACC; /* hundredths of ns */
  printf("%s W=%ld: %ld ticks, baseline %ld ticks, %ld.%02ld ns/access\n", name, (long)w, (long)t,
         (long)tb, (long)(d / 100), (long)((d < 0 ? -d : d) % 100));
}

static void s_main(void) {
  uint64_t ws[2] = {8, PAGES};
  for (int k = 0; k < 2; k++) {
    uint64_t w = ws[k];
    build_ring(w);
    report("chase ", w, timed(chase, w), timed(chase_base, w));
    report("stream", w, timed(stream, w), timed(stream_base, w));
  }
  exit(0);
}

int main(void) {
  root[2] = ((0x80000000UL >> 12) << 10) | PTE_V | PTE_R | PTE_W | PTE_X | PTE_A | PTE_D;
  root[1] = (((uintptr_t)l1 >> 12) << 10) | PTE_V;
  for (int k = 0; k < PAGES / 512; k++) l1[k] = (((uintptr_t)l0[k] >> 12) << 10) | PTE_V;
  for (int p = 0; p < PAGES; p++)
    l0[p / 512][p % 512] = (((uintptr_t)data[p] >> 12) << 10) | PTE_V | PTE_R | PTE_W | PTE_A | PTE_D;
  uint64_t satp = (8UL << 60) | ((uintptr_t)root >> 12);
  uintptr_t ms;
  asm volatile("csrr %0, mstatus" : "=r"(ms));
  ms = (ms & ~(3UL << 11)) | (1UL << 11); /* MPP = S */
  asm volatile("csrw mcounteren, %0" ::"r"(7UL));
  asm volatile("csrw mstatus, %0\n csrw mepc, %1\n csrw satp, %2\n sfence.vma\n mret"
               ::"r"(ms), "r"((uintptr_t)s_main), "r"(satp));
  return 0;
}
