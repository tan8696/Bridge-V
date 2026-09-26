# Precise-fault test (P2.7): print a line, then, inside one basic block, update registers and
# store into the read-only text segment. The store must fault with every engine at the same
# pc, with the registers written before it visible and nothing after it executed: exit status
# 139 (SIGSEGV) like qemu-riscv64.
    .section .text
    .globl _start
_start:
    li      a0, 1               # fd = stdout
    lla     a1, msg
    li      a2, 12
    li      a7, 64              # __NR_write
    ecall
    li      t1, 7               # same block as the faulting store
    addi    t1, t1, 35
    lla     t0, _start          # text: mapped read-only (R|X)
    sd      t1, 8(t0)           # store access fault → SIGSEGV
    li      t1, 99              # never executed
    li      a0, 1
    li      a7, 93              # __NR_exit
    ecall

    .section .rodata
msg:
    .ascii  "before fault"
