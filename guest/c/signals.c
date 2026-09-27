// Guest signals (Phase 10): SIGSEGV handlers that recover with siglongjmp (si_code, si_addr),
// raise() with a handler that returns (rt_sigreturn restores every register), a blocked signal
// delivered when unblocked, sigaltstack + SA_ONSTACK, SA_RESETHAND, SIGILL, SIG_IGN, and
// abort()'s default action (exit status 134).
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static sigjmp_buf jb;
// An address in the unmapped first page, hidden from the compiler's bounds checks.
static volatile unsigned long null_page = 16;
static void *volatile fault_addr;
static volatile int fault_code;

static void on_segv(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    fault_addr = si->si_addr;
    fault_code = si->si_code;
    siglongjmp(jb, sig);
}

static volatile int usr1_count, usr1_info_ok;

static void on_usr1(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    usr1_count++;
    usr1_info_ok = si->si_signo == sig && si->si_code == SI_TKILL && si->si_pid == getpid();
}

static volatile int on_alt;
static char altstack[65536];

static void on_usr2(int sig) {
    char local;
    (void)sig;
    on_alt = &local >= altstack && &local < altstack + sizeof altstack;
}

static void on_ill(int sig) { siglongjmp(jb, sig); }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, 0);

    int r = sigsetjmp(jb, 1);
    if (r == 0) {
        (void)*(volatile int *)null_page;
        puts("no fault?");
    }
    printf("segv load: sig %d code %d addr %p\n", r, fault_code, fault_addr);
    r = sigsetjmp(jb, 1);
    if (r == 0) {
        *(volatile char *)(void *)main = 0;
        puts("no fault?");
    }
    printf("segv store to text: sig %d code %d addr is main %d\n", r, fault_code,
           fault_addr == (void *)main);

    sa.sa_sigaction = on_usr1;
    sigaction(SIGUSR1, &sa, 0);
    volatile long canary = 0x1234567890L;
    volatile double d = 2.5;
    raise(SIGUSR1);
    printf("usr1: count %d info %d canary %lx d %.2f\n", usr1_count, usr1_info_ok, canary,
           d * 2);

    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, 0);
    raise(SIGUSR1);
    printf("while blocked: count %d\n", usr1_count);
    sigprocmask(SIG_UNBLOCK, &set, 0);
    printf("after unblock: count %d\n", usr1_count);

    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
    sigaltstack(&ss, 0);
    struct sigaction sb;
    memset(&sb, 0, sizeof sb);
    sb.sa_handler = on_usr2;
    sb.sa_flags = SA_ONSTACK | SA_RESETHAND;
    sigaction(SIGUSR2, &sb, 0);
    raise(SIGUSR2);
    struct sigaction cur;
    sigaction(SIGUSR2, 0, &cur);
    printf("usr2: on altstack %d, reset to default %d\n", on_alt, cur.sa_handler == SIG_DFL);

    signal(SIGILL, on_ill);
    r = sigsetjmp(jb, 1);
    if (r == 0) {
        __asm__ volatile(".word 0");
        puts("no SIGILL?");
    }
    printf("sigill: sig %d\n", r);

    signal(SIGUSR1, SIG_IGN);
    raise(SIGUSR1);
    puts("ignored usr1");
    fflush(stdout);
    abort();
}
