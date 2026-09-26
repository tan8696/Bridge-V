/*
 * Bridge-V Linux shim for riscv-tests' bare-metal Dhrystone (P5.1).
 *
 * third_party/riscv-tests/benchmarks/dhrystone/dhrystone_main.c includes "util.h" right after
 * "dhrystone.h"; with this directory on the include path it gets this file instead of the
 * riscv-tests bare-metal one. The benchmark sources stay unmodified. This shim:
 *   - takes the number of runs from argv[1] (see shim.c) instead of the fixed 500,
 *   - times the measured loop with clock_gettime(CLOCK_MONOTONIC) in microseconds on every
 *     target (the riscv-tests default reads mcycle, which Linux user mode cannot),
 *   - makes setStats() a no-op; debug_printf() in dhrystone_main.c becomes a real printf
 *     (bridgev_dhry_printf in shim.c, renamed on the compiler command line).
 */
#ifndef BRIDGEV_DHRY_UTIL_H
#define BRIDGEV_DHRY_UTIL_H

#include <stdio.h>
#include <string.h>

extern int bridgev_dhry_runs;
long bridgev_dhry_now_us(void);

#undef NUMBER_OF_RUNS
#define NUMBER_OF_RUNS bridgev_dhry_runs

#undef HZ
#define HZ 1000000L /* long: HZ * Number_Of_Runs must not overflow int */
#undef Too_Small_Time
#define Too_Small_Time 1
#undef CLOCK_TYPE
#define CLOCK_TYPE "clock_gettime(CLOCK_MONOTONIC)"
#undef Start_Timer
#define Start_Timer() (Begin_Time = bridgev_dhry_now_us())
#undef Stop_Timer
#define Stop_Timer() (End_Time = bridgev_dhry_now_us())

#define setStats(on) ((void)(on))

#endif
