# Tight counted loop: 100,000,000 iterations of `addi` + `bne` (2e8 guest instructions), then
# exit with ((count >> 8) & 0xff) = 0xE1 = 225 so the result is checked. Used for MIPS numbers.
    .section .text
    .globl _start
_start:
    li      t0, 100000000
    li      t1, 0
1:
    addi    t1, t1, 1
    bne     t1, t0, 1b
    srli    a0, t1, 8
    andi    a0, a0, 0xff
    li      a7, 93              # __NR_exit
    ecall
