/* Bridge-V FP benchmark (P6.6): the floating-point work the integer benchmarks lack.
 *
 * One "unit" of work, repeated argv[1] times from the same initial state:
 *   nbody    1000 steps of the 5-body simulation (Computer Language Benchmarks Game): double
 *            add/sub/mul/div/sqrt, FMA when the compiler contracts (-O2 GNU mode does on rv64).
 *   sgemm    24x24x24 single-precision matrix multiply plus row norms (sqrtf): NaN-boxed
 *            single-precision loads, stores and arithmetic.
 *   convert  4096 int<->float conversions: fcvt.d.w, fcvt.l.d (rtz), fcvt.s.w, fcvt.w.s (rtz),
 *            llrint (round to nearest even through frm).
 *
 * Every unit is validated against references that do not depend on the FP code under test:
 *   nbody    the published energies for 1000 steps (-0.169075164 -> -0.169087605, 1e-9);
 *   sgemm    all inputs are small dyadic rationals, so every product and sum is exact in single
 *            precision: C must equal an integer-arithmetic reference bit-exactly; the sum of row
 *            norms must match a double-precision computation to 1e-5 relative;
 *   convert  closed-form integer sums.
 * Prints "FP validated" only if every unit passed; the harness (tools/bench.py) requires it.
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#define PI 3.141592653589793
#define SOLAR_MASS (4 * PI * PI)
#define DAYS_PER_YEAR 365.24
#define NBODIES 5
#define STEPS 1000
#define N 24
#define CN 4096

struct body {
  double x, y, z, vx, vy, vz, mass;
};

static const struct body init[NBODIES] = {
    {0, 0, 0, 0, 0, 0, SOLAR_MASS},
    {4.84143144246472090e+00, -1.16032004402742839e+00, -1.03622044471123109e-01,
     1.66007664274403694e-03 * DAYS_PER_YEAR, 7.69901118419740425e-03 * DAYS_PER_YEAR,
     -6.90460016972063023e-05 * DAYS_PER_YEAR, 9.54791938424326609e-04 * SOLAR_MASS},
    {8.34336671824457987e+00, 4.12479856412430479e+00, -4.03523417114321381e-01,
     -2.76742510726862411e-03 * DAYS_PER_YEAR, 4.99852801234917238e-03 * DAYS_PER_YEAR,
     2.30417297573763929e-05 * DAYS_PER_YEAR, 2.85885980666130812e-04 * SOLAR_MASS},
    {1.28943695621391310e+01, -1.51111514016986312e+01, -2.23307578892655734e-01,
     2.96460137564761618e-03 * DAYS_PER_YEAR, 2.37847173959480950e-03 * DAYS_PER_YEAR,
     -2.96589568540237556e-05 * DAYS_PER_YEAR, 4.36624404335156298e-05 * SOLAR_MASS},
    {1.53796971148509165e+01, -2.59193146099879641e+01, 1.79258772950371181e-01,
     2.68067772490389322e-03 * DAYS_PER_YEAR, 1.62824170038242295e-03 * DAYS_PER_YEAR,
     -9.51592254519715870e-05 * DAYS_PER_YEAR, 5.15138902046611451e-05 * SOLAR_MASS},
};

/* Read once per unit, so the compiler cannot hoist the work out of the unit loop. */
static volatile double vzero_d = 0.0;
static volatile float vone_f = 1.0f;
static volatile int vzero_i = 0;

static void advance(struct body *b, double dt) {
  for (int i = 0; i < NBODIES; i++) {
    for (int j = i + 1; j < NBODIES; j++) {
      double dx = b[i].x - b[j].x, dy = b[i].y - b[j].y, dz = b[i].z - b[j].z;
      double d2 = dx * dx + dy * dy + dz * dz;
      double mag = dt / (d2 * sqrt(d2));
      b[i].vx -= dx * b[j].mass * mag;
      b[i].vy -= dy * b[j].mass * mag;
      b[i].vz -= dz * b[j].mass * mag;
      b[j].vx += dx * b[i].mass * mag;
      b[j].vy += dy * b[i].mass * mag;
      b[j].vz += dz * b[i].mass * mag;
    }
  }
  for (int i = 0; i < NBODIES; i++) {
    b[i].x += dt * b[i].vx;
    b[i].y += dt * b[i].vy;
    b[i].z += dt * b[i].vz;
  }
}

static double energy(const struct body *b) {
  double e = 0.0;
  for (int i = 0; i < NBODIES; i++) {
    e += 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
    for (int j = i + 1; j < NBODIES; j++) {
      double dx = b[i].x - b[j].x, dy = b[i].y - b[j].y, dz = b[i].z - b[j].z;
      e -= b[i].mass * b[j].mass / sqrt(dx * dx + dy * dy + dz * dz);
    }
  }
  return e;
}

static void nbody(double *e0, double *e1) {
  struct body b[NBODIES];
  double px = 0, py = 0, pz = 0;
  for (int i = 0; i < NBODIES; i++) {
    b[i] = init[i];
    b[i].x += vzero_d;
    px += b[i].vx * b[i].mass;
    py += b[i].vy * b[i].mass;
    pz += b[i].vz * b[i].mass;
  }
  b[0].vx = -px / SOLAR_MASS;
  b[0].vy = -py / SOLAR_MASS;
  b[0].vz = -pz / SOLAR_MASS;
  *e0 = energy(b);
  for (int s = 0; s < STEPS; s++) advance(b, 0.01);
  *e1 = energy(b);
}

static int a_int(int i, int k) { return (i * 7 + k * 3) % 17 - 8; }
static int b_int(int k, int j) { return (k * 5 + j * 11) % 13 - 6; }

static float A[N][N], B[N][N], C[N][N];
static long Cref[N][N];
static double norm_ref;

/* Returns the number of mismatching elements; *norms = sum of row norms. */
static int sgemm(float *norms) {
  float one = vone_f;
  for (int i = 0; i < N; i++)
    for (int k = 0; k < N; k++) {
      A[i][k] = (float)a_int(i, k) * 0.125f * one;
      B[i][k] = (float)b_int(i, k) * 0.25f;
    }
  for (int i = 0; i < N; i++)
    for (int j = 0; j < N; j++) {
      float s = 0.0f;
      for (int k = 0; k < N; k++) s += A[i][k] * B[k][j];
      C[i][j] = s;
    }
  int bad = 0;
  float total = 0.0f;
  for (int i = 0; i < N; i++) {
    float sq = 0.0f;
    for (int j = 0; j < N; j++) {
      bad += C[i][j] != (float)Cref[i][j] * 0.03125f;
      sq += C[i][j] * C[i][j];
    }
    total += sqrtf(sq);
  }
  *norms = total;
  return bad;
}

static long conv_s1, conv_s2, conv_s3;

static void convert(void) {
  long s1 = 0, s2 = 0, s3 = 0;
  int z = vzero_i;
  for (int i = 0; i < CN; i++) {
    double x = (double)(i + z) * 0.5 + 0.25;
    s1 += (long)x;
    float f = (float)(i + z);
    s2 += (int)(f * 3.0f);
    s3 += llrint((double)(i + z) + 0.5);
  }
  conv_s1 = s1, conv_s2 = s2, conv_s3 = s3;
}

static double now(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec + ts.tv_nsec * 1e-9;
}

int main(int argc, char **argv) {
  long units = argc > 1 ? atol(argv[1]) : 100;
  if (units < 1) units = 1;

  /* Integer references (independent of the FP code under test). */
  norm_ref = 0.0;
  for (int i = 0; i < N; i++) {
    double sq = 0.0;
    for (int j = 0; j < N; j++) {
      long s = 0;
      for (int k = 0; k < N; k++) s += (long)a_int(i, k) * b_int(k, j);
      Cref[i][j] = s;
      sq += (double)s * s / 1024.0;
    }
    norm_ref += sqrt(sq);
  }
  long r1 = 0, r2 = 0, r3 = 0;
  for (long i = 0; i < CN; i++) {
    r1 += i / 2;
    r2 += 3 * i;
    r3 += (i % 2 == 0) ? i : i + 1; /* i + 0.5 rounds to the even neighbour */
  }

  long failed = 0;
  double e0 = 0, e1 = 0;
  float norms = 0;
  double t0 = now();
  for (long u = 0; u < units; u++) {
    nbody(&e0, &e1);
    int bad = sgemm(&norms);
    convert();
    int ok = fabs(e0 - -0.169075164) < 1e-9 && fabs(e1 - -0.169087605) < 1e-9 && bad == 0 &&
             fabs(norms - norm_ref) <= 1e-5 * norm_ref && conv_s1 == r1 && conv_s2 == r2 &&
             conv_s3 == r3;
    failed += !ok;
  }
  double secs = now() - t0;

  printf("fpbench: %ld units\n", units);
  printf("nbody energy: %.9f -> %.9f (want -0.169075164 -> -0.169087605)\n", e0, e1);
  printf("sgemm row norms: %.6f (want %.6f)\n", norms, norm_ref);
  printf("convert sums: %ld %ld %ld (want %ld %ld %ld)\n", conv_s1, conv_s2, conv_s3, r1, r2, r3);
  printf("failed units: %ld\n", failed);
  printf("Total time (secs): %.6f\n", secs);
  printf("FP units/s: %.3f\n", secs > 0 ? units / secs : 0.0);
  if (failed == 0) printf("FP validated\n");
  return failed != 0;
}
