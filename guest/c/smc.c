/* Self-modifying code (P8.6): machine code written at run time into an RWX mapping.
 *   a. write a function, flush, call; rewrite, flush, call again
 *   b. the same without any cache flush (x86-style coherence: eager invalidation)
 *   c. a loop that patches an instruction of its own next iteration
 *   d. a tiny "JIT inside the guest": many functions generated at one address
 *   e. a chained predecessor: A (page 1) jumps to B (page 2); B's page is rewritten
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

enum { A0 = 10, A1 = 11, T0 = 5, T1 = 6, T2 = 7 };
#define RET 0x00008067u /* jalr x0, 0(ra) */

static uint32_t addi(int rd, int rs1, int imm) {
  return ((uint32_t)(imm & 0xfff) << 20) | (uint32_t)rs1 << 15 | (uint32_t)rd << 7 | 0x13;
}
static uint32_t lw(int rd, int rs1, int imm) {
  return ((uint32_t)(imm & 0xfff) << 20) | (uint32_t)rs1 << 15 | 2u << 12 | (uint32_t)rd << 7 | 0x03;
}
static uint32_t sw(int rs2, int rs1, int imm) {
  return ((uint32_t)(imm >> 5) & 0x7f) << 25 | (uint32_t)rs2 << 20 | (uint32_t)rs1 << 15 |
         2u << 12 | ((uint32_t)imm & 31) << 7 | 0x23;
}
static uint32_t lui(int rd, uint32_t imm20) { return imm20 << 12 | (uint32_t)rd << 7 | 0x37; }
static uint32_t add(int rd, int rs1, int rs2) {
  return (uint32_t)rs2 << 20 | (uint32_t)rs1 << 15 | (uint32_t)rd << 7 | 0x33;
}
static uint32_t mul(int rd, int rs1, int rs2) { return 1u << 25 | add(rd, rs1, rs2); }
static uint32_t bne(int rs1, int rs2, int off) {
  uint32_t o = (uint32_t)off;
  return ((o >> 12) & 1) << 31 | ((o >> 5) & 0x3f) << 25 | (uint32_t)rs2 << 20 |
         (uint32_t)rs1 << 15 | 1u << 12 | ((o >> 1) & 0xf) << 8 | ((o >> 11) & 1) << 7 | 0x63;
}
static uint32_t jal0(int off) {
  uint32_t o = (uint32_t)off;
  return ((o >> 20) & 1) << 31 | ((o >> 1) & 0x3ff) << 21 | ((o >> 11) & 1) << 20 |
         ((o >> 12) & 0xff) << 12 | 0x6f;
}

typedef long (*fn2)(long, long);

static void flush(void *p, size_t n) { __builtin___clear_cache((char *)p, (char *)p + n); }

static long call(uint32_t *code, long a, long b) { return ((fn2)(void *)code)(a, b); }

int main(void) {
  uint32_t *buf = mmap(0, 4 * 4096, PROT_READ | PROT_WRITE | PROT_EXEC,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (buf == MAP_FAILED) {
    puts("mmap failed");
    return 1;
  }

  /* a. */
  buf[0] = addi(A0, 0, 42);
  buf[1] = RET;
  flush(buf, 8);
  long a1 = call(buf, 0, 0);
  buf[0] = addi(A0, 0, 43);
  flush(buf, 8);
  long a2 = call(buf, 0, 0);
  printf("a: %ld %ld\n", a1, a2);

  /* b. No flush: each version is called often enough to be translated and chained. */
  long b1 = 0, b2 = 0;
  buf[0] = addi(A0, 0, 44);
  for (int i = 0; i < 1000; i++) b1 += call(buf, 0, 0);
  buf[0] = addi(A0, 0, 45);
  for (int i = 0; i < 1000; i++) b2 += call(buf, 0, 0);
  printf("b: %ld %ld\n", b1, b2);

  /* c. Instruction 2 (addi a0, a0, k) gets k + 1 stored into it by every iteration. */
  uint32_t *c = buf + 64;
  c[0] = addi(A0, 0, 0);
  c[1] = addi(T0, 0, 100);
  c[2] = addi(A0, A0, 1);
  c[3] = lw(T1, A1, 8);
  c[4] = lui(T2, 0x100); /* t2 = 1 << 20: +1 in the I-immediate */
  c[5] = add(T1, T1, T2);
  c[6] = sw(T1, A1, 8);
  c[7] = addi(T0, T0, -1);
  c[8] = bne(T0, 0, -24);
  c[9] = RET;
  long c1 = call(c, 0, (long)c);
  long c2 = call(c, 0, (long)c); /* continues from k = 101 */
  printf("c: %ld %ld\n", c1, c2);

  /* d. Generate f_k(x) = x * k + 3k at one address, 50 times; flush on even k only. */
  uint32_t *d = buf + 256;
  long dsum = 0;
  int dbad = 0;
  for (int k = 0; k < 50; k++) {
    d[0] = addi(T0, 0, k);
    d[1] = mul(A0, A0, T0);
    d[2] = addi(A0, A0, 3 * k);
    d[3] = RET;
    if (k % 2 == 0) flush(d, 16);
    for (int r = 0; r < 20; r++) {
      long v = call(d, 7, 0);
      dsum += v;
      dbad += v != 7L * k + 3 * k;
    }
  }
  printf("d: %ld %d\n", dsum, dbad);

  /* e. A on page 1 jumps to B at the start of page 2; B returns a constant. */
  uint32_t *pa = buf + 1024, *pb = buf + 2048;
  pa[0] = addi(A0, 0, 0);
  pa[1] = jal0((int)((char *)pb - (char *)&pa[1]));
  pb[0] = addi(A0, A0, 1);
  pb[1] = RET;
  flush(buf, 4 * 4096);
  long e1 = 0, e2 = 0;
  for (int i = 0; i < 1000; i++) e1 += call(pa, 0, 0);
  pb[0] = addi(A0, A0, 2);
  for (int i = 0; i < 1000; i++) e2 += call(pa, 0, 0);
  printf("e: %ld %ld\n", e1, e2);
  return 0;
}
