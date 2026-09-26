/* struct stat translation (P1.15): the riscv64 layout (128 bytes) differs from x86-64's.
 * Only machine-independent facts are printed: a file this program creates itself, "/", and
 * a missing path. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char path[] = "/tmp/bridgev-stat-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 1;
    char buf[1234];
    memset(buf, 'x', sizeof buf);
    if (write(fd, buf, sizeof buf) != (ssize_t)sizeof buf) return 2;
    struct stat a, b;
    if (fstat(fd, &a) != 0) return 3;
    printf("fstat: size=%lld reg=%d nlink=%lu mode=%o\n", (long long)a.st_size,
           S_ISREG(a.st_mode), (unsigned long)a.st_nlink, (unsigned)(a.st_mode & 0777));
    if (stat(path, &b) != 0) return 4;
    printf("stat: same_ino=%d same_dev=%d size=%lld blksize_ok=%d mtime_ok=%d\n",
           a.st_ino == b.st_ino, a.st_dev == b.st_dev, (long long)b.st_size, b.st_blksize > 0,
           b.st_mtime > 1600000000);
    if (stat("/", &b) != 0) return 5;
    printf("root_is_dir=%d\n", S_ISDIR(b.st_mode));
    int r = stat("/nonexistent/bridgev", &b);
    printf("missing=%d enoent=%d\n", r, errno == ENOENT);
    close(fd);
    unlink(path);
    return 0;
}
