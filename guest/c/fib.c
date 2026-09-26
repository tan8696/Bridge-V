/* JALR-heavy test (P3.4): naive recursion makes every call return through `ret`
 * (jalr x0, 0(ra)), exercising the jump cache. An optional argument raises the upper bound
 * (benchmark runs); the reference output uses the default. */
#include <stdio.h>
#include <stdlib.h>

static long fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

int main(int argc, char **argv) {
    int hi = argc > 1 ? atoi(argv[1]) : 27;
    for (int i = 20; i <= hi; i++)
        printf("fib(%d) = %ld\n", i, fib(i));
    return 0;
}
