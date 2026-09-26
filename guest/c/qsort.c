/* Sort 10,000 pseudo-random ints (LCG), print a checksum: calls through a function pointer. */
#include <stdio.h>
#include <stdlib.h>

static int cmp(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

int main(void) {
    enum { N = 10000 };
    static int v[N];
    unsigned int s = 12345;
    for (int i = 0; i < N; i++) {
        s = s * 1103515245u + 12345u;
        v[i] = (int)(s >> 8) - (1 << 22);
    }
    qsort(v, N, sizeof v[0], cmp);
    long check = 0;
    for (int i = 0; i < N; i++) {
        if (i && v[i - 1] > v[i]) { puts("NOT SORTED"); return 1; }
        check = check * 31 + v[i];
    }
    printf("min=%d max=%d check=%ld\n", v[0], v[N - 1], check);
    return 0;
}
