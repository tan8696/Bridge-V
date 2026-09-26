/* String and memory routines (glibc's optimized versions) plus snprintf. */
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    char buf[128];
    const char *s = "The quick brown fox jumps over the lazy dog";
    printf("len=%zu\n", strlen(s));
    printf("cmp=%d %d\n", strcmp("abc", "abd") < 0, strncmp(s, "The", 3) == 0);
    printf("strstr=%s\n", strstr(s, "fox"));
    memcpy(buf, s, 20); buf[20] = 0;
    memmove(buf + 4, buf, 10);
    printf("buf=%s\n", buf);
    int n = snprintf(buf, sizeof buf, "%s|%5d|%-4s|%08.3f|%x", "fmt", 42, "ab", 3.14159, 0xbeefu);
    printf("snprintf=%d:%s\n", n, buf);
    printf("argc=%d", argc);
    for (int i = 1; i < argc; i++) printf(" [%s]", argv[i]);
    printf("\n");
    return 0;
}
