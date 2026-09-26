/* Exercises F/D arithmetic, conversions and glibc's float formatting. */
#include <math.h>
#include <stdio.h>

int main(void) {
    volatile double a = 1.0, b = 3.0;  /* volatile: keep the FP ops at runtime */
    volatile float f = 2.5f;
    printf("%.17g\n", a / b);
    printf("%f %e %g\n", a / b, 6.02214076e23, 1e-300 * 1e-10);
    printf("%.9g\n", (double)(f * f + 0.1f));
    printf("%.17g\n", sqrt(b));
    printf("%ld %d\n", (long)(-7.9), (int)(2.5f * 3.0f));
    printf("%f %f\n", 0.0 / (a - a) == 0.0 ? 1.0 : -1.0, -0.0);
    printf("%a\n", a / b);
    return 0;
}
