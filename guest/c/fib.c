/* JALR-heavy test (P3.4): naive recursion makes every call return through `ret`
 * (jalr x0, 0(ra)), exercising the jump cache. */
#include <stdio.h>

static long fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

int main(void) {
    for (int i = 20; i <= 27; i++)
        printf("fib(%d) = %ld\n", i, fib(i));
    return 0;
}
