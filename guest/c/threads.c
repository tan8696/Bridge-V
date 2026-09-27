// Guest threads (Phase 10, multithreaded user mode): pthread_create/join (clone with
// CLONE_SETTLS/PARENT_SETTID/CHILD_CLEARTID, futex wake on exit), mutexes and condition
// variables (futex wait/wake), C11 atomics (AMOs, LR/SC), thread-local storage, a busy-wait
// that only ends if the spinning thread is preempted, and sleeping threads. The output is
// deterministic.
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>
#include <sys/syscall.h>

#define N 4
#define ITERS 20000

static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static long locked_sum;
static atomic_long atomic_sum;
static long cas_sum;
static __thread int tls_id = -1;
static int tls_seen[N];
static long tids[N];

static void *worker(void *arg) {
    int id = (int)(long)arg;
    tls_id = id * 10;
    tids[id] = syscall(SYS_gettid);
    for (int i = 0; i < ITERS; i++) {
        pthread_mutex_lock(&mu);
        locked_sum += id + 1;
        pthread_mutex_unlock(&mu);
        atomic_fetch_add(&atomic_sum, 1);
        // A compare-and-swap loop (LR/SC on RISC-V).
        long old = __atomic_load_n(&cas_sum, __ATOMIC_RELAXED);
        while (!__atomic_compare_exchange_n(&cas_sum, &old, old + 2, 1, __ATOMIC_SEQ_CST,
                                            __ATOMIC_RELAXED)) {
        }
    }
    tls_seen[id] = tls_id;
    return (void *)(long)(id * id);
}

static int turn, rounds;

static void *pong(void *unused) {
    (void)unused;
    for (int i = 0; i < 1000; i++) {
        pthread_mutex_lock(&mu);
        while (turn != 1) pthread_cond_wait(&cv, &mu);
        turn = 0;
        rounds++;
        pthread_cond_signal(&cv);
        pthread_mutex_unlock(&mu);
    }
    return 0;
}

static atomic_int flag;

static void *setter(void *unused) {
    (void)unused;
    struct timespec ts = {0, 2000000};  // 2 ms: the main thread is spinning meanwhile
    nanosleep(&ts, 0);
    atomic_store(&flag, 1);
    return 0;
}

int main(void) {
    pthread_t t[N];
    for (long i = 0; i < N; i++) pthread_create(&t[i], 0, worker, (void *)i);
    long joined = 0;
    for (int i = 0; i < N; i++) {
        void *r;
        pthread_join(t[i], &r);
        joined += (long)r;
    }
    printf("locked_sum %ld atomic_sum %ld cas_sum %ld join %ld\n", locked_sum,
           (long)atomic_sum, cas_sum, joined);
    int distinct = 1;
    for (int i = 0; i < N; i++) {
        printf("tls[%d] = %d\n", i, tls_seen[i]);
        for (int j = 0; j < i; j++) distinct &= tids[i] != tids[j];
        distinct &= tids[i] != getpid();
    }
    printf("main tls %d, distinct tids %d\n", tls_id, distinct);

    pthread_t p;
    pthread_create(&p, 0, pong, 0);
    for (int i = 0; i < 1000; i++) {
        pthread_mutex_lock(&mu);
        while (turn != 0) pthread_cond_wait(&cv, &mu);
        turn = 1;
        pthread_cond_signal(&cv);
        pthread_mutex_unlock(&mu);
    }
    pthread_join(p, 0);
    printf("ping-pong rounds %d\n", rounds);

    pthread_t s;
    pthread_create(&s, 0, setter, 0);
    long spins = 0;
    while (!atomic_load(&flag)) spins++;
    pthread_join(s, 0);
    printf("spin-wait done (%s)\n", spins > 0 ? "spun" : "no spin");
    return 0;
}
