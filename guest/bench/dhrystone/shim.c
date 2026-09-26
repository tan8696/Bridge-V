/* Bridge-V Linux shim for riscv-tests' Dhrystone: run count, timer, debug_printf (see util.h). */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

int bridgev_dhry_runs = 500;

/* glibc passes (argc, argv, envp) to .init_array functions, static binaries included. */
static void __attribute__((constructor)) bridgev_dhry_args(int argc, char **argv, char **envp)
{
    (void)envp;
    if (argc > 1)
        bridgev_dhry_runs = atoi(argv[1]);
}

long bridgev_dhry_now_us(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000000L + t.tv_nsec / 1000;
}

/* dhrystone_main.c is compiled with -Ddebug_printf=bridgev_dhry_printf: riscv-tests' own
 * debug_printf (in dhrystone.c) is empty, which would hide the final-value check. */
void bridgev_dhry_printf(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
}
