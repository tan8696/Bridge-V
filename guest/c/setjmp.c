/* Non-local control flow: setjmp/longjmp saves and restores callee-saved and FP registers. */
#include <setjmp.h>
#include <stdio.h>

static jmp_buf env;

static int deep(int n) {
    if (n < 0) return 0;  /* never taken; gives GCC a normal return path */
    if (n == 0) longjmp(env, 42);
    return deep(n - 1) + 1;
}

int main(void) {
    volatile int round = 0;
    volatile double d = 1.5;
    int r = setjmp(env);
    printf("setjmp returned %d (round %d, d=%g)\n", r, round, d);
    if (round++ < 3) { d *= 2; printf("unreachable %d\n", deep(10 * round)); }
    return 7;  /* exit status is checked too */
}
