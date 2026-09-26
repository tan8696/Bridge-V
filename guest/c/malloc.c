/* Heap exercise: small (brk) and large (mmap) allocations, realloc, free. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    unsigned long sum = 0;
    char *ptrs[256];
    for (int i = 0; i < 256; i++) {
        size_t n = (size_t)(i * 37 % 1000) + 1;
        ptrs[i] = malloc(n);
        memset(ptrs[i], i & 0xff, n);
        sum += (unsigned char)ptrs[i][n - 1] * n;
    }
    for (int i = 0; i < 256; i += 2) free(ptrs[i]);
    char *big = malloc(4 << 20);  /* above the mmap threshold */
    for (int i = 0; i < (4 << 20); i += 4096) big[i] = (char)i;
    big = realloc(big, 8 << 20);
    sum += (unsigned char)big[4096 * 3];
    free(big);
    for (int i = 1; i < 256; i += 2) free(ptrs[i]);
    printf("sum=%lu\n", sum);
    return 0;
}
