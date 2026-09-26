# Minimal RV64 Linux program: write(1, "hello\n", 6); exit(0).
# No libc, no stack use: the first program Bridge-V must run (Phase 1).
    .section .text
    .globl _start
_start:
    li      a0, 1               # fd = stdout
    lla     a1, msg             # buf (lla: PC-relative; `la` would load from the GOT under PIC)
    li      a2, 6               # len
    li      a7, 64              # __NR_write
    ecall
    li      a0, 0               # status
    li      a7, 93              # __NR_exit
    ecall

    .section .rodata
msg:
    .ascii  "hello\n"
