/* Bridge-V TLB microbenchmark, Linux user-mode twin of tlbbench.c (P7.9): the same chase and
 * stream kernels over W = 8 and W = 4096 pages of an mmap'd buffer, timed with clock_gettime.
 * Run under `--mem=direct` (raw host loads) and `--mem=softmmu` (the inline TLB path; misses
 * take the slow path with identity translation, no page walk): the difference is the cost of
 * the TLB machinery itself. Prints ns per access (baseline loop subtracted).
 */
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <time.h>

#define PAGES 4096
#define N_ACC 4000000L

static uint8_t *base;

static inline uint64_t node_off(uint64_t p) { return (p * 64) % 4096; }
static inline uint64_t node_va(uint64_t p) { return (uint64_t)base + p * 4096 + node_off(p); }

static double now(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1e9 + ts.tv_nsec;
}

static uint64_t __attribute__((noinline)) chase(uint64_t w, long n) {
  uint64_t a = node_va(0);
  for (long i = 0; i < n; i++) a = *(volatile uint64_t *)a;
  return a + w;
}
static uint64_t __attribute__((noinline)) chase_base(uint64_t w, long n) {
  uint64_t a = node_va(0);
  for (long i = 0; i < n; i++) {
    a = a + 4096 + 64;
    __asm__ volatile("" : "+r"(a));
  }
  return a + w;
}
static uint64_t __attribute__((noinline)) stream(uint64_t w, long n) {
  uint64_t sum = 0, p = 0;
  for (long i = 0; i < n; i++) {
    sum += *(volatile uint64_t *)((uint64_t)base + p * 4096 + node_off(p));
    p = (p + 1) & (w - 1);
  }
  return sum;
}
static uint64_t __attribute__((noinline)) stream_base(uint64_t w, long n) {
  uint64_t sum = 0, p = 0;
  for (long i = 0; i < n; i++) {
    uint64_t a = (uint64_t)base + p * 4096 + node_off(p);
    __asm__ volatile("" : "+r"(a));
    sum += a;
    p = (p + 1) & (w - 1);
  }
  return sum;
}

static volatile uint64_t sink;

static double timed(uint64_t (*f)(uint64_t, long), uint64_t w) {
  sink += f(w, N_ACC / 16);
  double t0 = now();
  sink += f(w, N_ACC);
  return now() - t0;
}

int main(void) {
  base = mmap(0, PAGES * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (base == MAP_FAILED) return 1;
  uint64_t ws[2] = {8, PAGES};
  for (int k = 0; k < 2; k++) {
    uint64_t w = ws[k];
    for (uint64_t p = 0; p < w; p++) *(uint64_t *)node_va(p) = node_va((p + 1) % w);
    double c = timed(chase, w), cb = timed(chase_base, w);
    double s = timed(stream, w), sb = timed(stream_base, w);
    printf("chase  W=%lu: %.2f ns/access\n", (unsigned long)w, (c - cb) / N_ACC);
    printf("stream W=%lu: %.2f ns/access\n", (unsigned long)w, (s - sb) / N_ACC);
  }
  return 0;
}
