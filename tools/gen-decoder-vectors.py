#!/usr/bin/env python3
"""Generate golden decoder test vectors (P1.4/P1.5) with llvm-mc.

For every generated assembly line, llvm-mc is run twice:
  * without the C extension  -> 32-bit encoding + canonical text (-M no-aliases)
  * with the C extension     -> the compressed encoding, if llvm-mc chose one
Output: tests/data/rv64_vectors.txt, one line per instruction:
  <enc32:8 hex> <encC:4 hex | ---- > <canonical text>
The Rust test decodes enc32 and compares the disassembly with the text, and checks that the
compressed encoding expands to the same instruction.
"""
import pathlib
import random
import re
import subprocess
import sys

random.seed(0xB41D6E)
ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "tests/data/rv64_vectors.txt"

X = ["zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3",
     "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
     "t3", "t4", "t5", "t6"]
F = ["ft0", "ft1", "ft2", "ft3", "ft4", "ft5", "ft6", "ft7", "fs0", "fs1", "fa0", "fa1",
     "fa2", "fa3", "fa4", "fa5", "fa6", "fa7", "fs2", "fs3", "fs4", "fs5", "fs6", "fs7", "fs8",
     "fs9", "fs10", "fs11", "ft8", "ft9", "ft10", "ft11"]
XC = X[8:16]  # compressible x8..x15
FC = F[8:16]

_rot = {"x": 0, "f": 0}


def xr():
    """Rotate through all integer registers so every one appears in every field."""
    _rot["x"] = (_rot["x"] + 7) % 32
    return X[_rot["x"]]


def fr():
    _rot["f"] = (_rot["f"] + 5) % 32
    return F[_rot["f"]]


IMM12 = [-2048, -1, 0, 1, 5, 31, 2047, -1234]
RMS = ["rne", "rtz", "rdn", "rup", "rmm", "dyn"]
lines = []
emit = lines.append

# ---- RV64I ----
for m in ["addi", "slti", "sltiu", "xori", "ori", "andi"]:
    for imm in IMM12:
        emit(f"{m} {xr()}, {xr()}, {imm}")
for m in ["slli", "srli", "srai"]:
    for sh in [0, 1, 31, 32, 63]:
        emit(f"{m} {xr()}, {xr()}, {sh}")
for m in ["slliw", "srliw", "sraiw"]:
    for sh in [0, 1, 17, 31]:
        emit(f"{m} {xr()}, {xr()}, {sh}")
for imm in IMM12:
    emit(f"addiw {xr()}, {xr()}, {imm}")
for m in ["add", "sub", "sll", "slt", "sltu", "xor", "srl", "sra", "or", "and",
          "addw", "subw", "sllw", "srlw", "sraw",
          "mul", "mulh", "mulhsu", "mulhu", "div", "divu", "rem", "remu",
          "mulw", "divw", "divuw", "remw", "remuw"]:
    for _ in range(6):
        emit(f"{m} {xr()}, {xr()}, {xr()}")
for m in ["lui", "auipc"]:
    for imm in [0, 1, 31, 0x7ffff, 0x80000, 0xfffff, 0x12345, 0xfffe0]:
        emit(f"{m} {xr()}, {imm}")
for off in [-1048576, -2048, -2, 0, 2, 2046, 1048574, 123456]:
    emit(f"jal {xr()}, {off}")
for imm in [-2048, 0, 1, 2047]:
    emit(f"jalr {xr()}, {imm}({xr()})")
for m in ["beq", "bne", "blt", "bge", "bltu", "bgeu"]:
    for off in [-4096, -256, -2, 0, 2, 254, 4094]:
        emit(f"{m} {xr()}, {xr()}, {off}")
for m in ["lb", "lh", "lw", "ld", "lbu", "lhu", "lwu"]:
    for off in [-2048, -8, 0, 8, 2047]:
        emit(f"{m} {xr()}, {off}({xr()})")
for m in ["sb", "sh", "sw", "sd"]:
    for off in [-2048, -8, 0, 8, 2047]:
        emit(f"{m} {xr()}, {off}({xr()})")
for p, s in [("iorw", "iorw"), ("r", "w"), ("rw", "rw"), ("i", "o"), ("w", "r"), ("ow", "ir")]:
    emit(f"fence {p}, {s}")
emit("fence.tso")
emit("fence.i")
for m in ["ecall", "ebreak", "mret", "sret", "wfi"]:
    emit(m)
for a, b in [("zero", "zero"), ("a0", "zero"), ("zero", "a1"), ("t6", "s11")]:
    emit(f"sfence.vma {a}, {b}")

# ---- Zicsr ----
CSRS = ["fflags", "frm", "fcsr", "cycle", "time", "instret", "sstatus", "sie", "stvec",
        "sscratch", "sepc", "scause", "stval", "sip", "satp", "mstatus", "misa", "medeleg",
        "mideleg", "mie", "mtvec", "mcounteren", "mscratch", "mepc", "mcause", "mtval", "mip",
        "pmpcfg0", "pmpaddr0", "mcycle", "minstret", "mhartid", "mvendorid", "0x7c0"]
for csr in CSRS:
    emit(f"csrrw {xr()}, {csr}, {xr()}")
    emit(f"csrrs {xr()}, {csr}, {xr()}")
    emit(f"csrrc {xr()}, {csr}, {xr()}")
for m in ["csrrwi", "csrrsi", "csrrci"]:
    for u in [0, 1, 31]:
        emit(f"{m} {xr()}, mstatus, {u}")

# ---- A ----
for w in ["w", "d"]:
    for o in ["", ".aq", ".rl", ".aqrl"]:
        emit(f"lr.{w}{o} {xr()}, ({xr()})")
        emit(f"sc.{w}{o} {xr()}, {xr()}, ({xr()})")
        for m in ["amoswap", "amoadd", "amoxor", "amoand", "amoor", "amomin", "amomax",
                  "amominu", "amomaxu"]:
            emit(f"{m}.{w}{o} {xr()}, {xr()}, ({xr()})")

# ---- F / D ----
for t in ["s", "d"]:
    ld, st = ("flw", "fsw") if t == "s" else ("fld", "fsd")
    for off in [-2048, -8, 0, 8, 2047]:
        emit(f"{ld} {fr()}, {off}({xr()})")
        emit(f"{st} {fr()}, {off}({xr()})")
    for m in ["fmadd", "fmsub", "fnmsub", "fnmadd"]:
        for rm in RMS:
            emit(f"{m}.{t} {fr()}, {fr()}, {fr()}, {fr()}, {rm}")
    for m in ["fadd", "fsub", "fmul", "fdiv"]:
        for rm in RMS:
            emit(f"{m}.{t} {fr()}, {fr()}, {fr()}, {rm}")
    for rm in RMS:
        emit(f"fsqrt.{t} {fr()}, {fr()}, {rm}")
    for m in ["fsgnj", "fsgnjn", "fsgnjx", "fmin", "fmax"]:
        for _ in range(3):
            emit(f"{m}.{t} {fr()}, {fr()}, {fr()}")
    for m in ["feq", "flt", "fle"]:
        for _ in range(3):
            emit(f"{m}.{t} {xr()}, {fr()}, {fr()}")
    emit(f"fclass.{t} {xr()}, {fr()}")
    for it in ["w", "wu", "l", "lu"]:
        for rm in RMS:
            emit(f"fcvt.{it}.{t} {xr()}, {fr()}, {rm}")
            # int -> double for 32-bit sources is exact: LLVM takes no rounding mode
            if t == "d" and it in ("w", "wu"):
                emit(f"fcvt.{t}.{it} {fr()}, {xr()}")
            else:
                emit(f"fcvt.{t}.{it} {fr()}, {xr()}, {rm}")
    w = "w" if t == "s" else "d"
    emit(f"fmv.x.{w} {xr()}, {fr()}")
    emit(f"fmv.{w}.x {fr()}, {xr()}")
for rm in RMS:
    emit(f"fcvt.s.d {fr()}, {fr()}, {rm}")
emit(f"fcvt.d.s {fr()}, {fr()}")

# ---- Shapes that the C extension can compress (exercise every RVC format) ----
for r in XC:
    emit(f"addi {r}, sp, {random.choice([4, 8, 16, 1020])}")      # c.addi4spn
for r in X[1:]:
    emit(f"addi {r}, {r}, {random.choice([-32, -1, 1, 31])}")     # c.addi
    emit(f"addi {r}, zero, {random.choice([-32, 0, 7, 31])}")     # c.li
    emit(f"addiw {r}, {r}, {random.choice([-32, 0, 1, 31])}")     # c.addiw
    emit(f"slli {r}, {r}, {random.choice([1, 31, 32, 63])}")      # c.slli
    emit(f"add {r}, zero, {random.choice(X[1:])}")                # c.mv
    emit(f"add {r}, {r}, {random.choice(X[1:])}")                 # c.add
    emit(f"jalr zero, 0({r})")                                    # c.jr
    emit(f"jalr ra, 0({r})")                                      # c.jalr
    emit(f"lw {r}, {random.choice([0, 4, 252])}(sp)")             # c.lwsp
    emit(f"ld {r}, {random.choice([0, 8, 504])}(sp)")             # c.ldsp
    emit(f"sw {r}, {random.choice([0, 4, 252])}(sp)")             # c.swsp
    emit(f"sd {r}, {random.choice([0, 8, 504])}(sp)")             # c.sdsp
for r in X[1:2] + X[3:]:
    emit(f"lui {r}, {random.choice([1, 31, 0xfffe0, 0xfffff])}")  # c.lui (rd != sp)
for imm in [-512, -16, 16, 496]:
    emit(f"addi sp, sp, {imm}")                                   # c.addi16sp
for r in XC:
    s = random.choice(XC)
    emit(f"srli {r}, {r}, {random.choice([1, 31, 32, 63])}")      # c.srli
    emit(f"srai {r}, {r}, {random.choice([1, 31, 32, 63])}")      # c.srai
    emit(f"andi {r}, {r}, {random.choice([-32, -1, 0, 31])}")     # c.andi
    for m in ["sub", "xor", "or", "and", "subw", "addw"]:
        emit(f"{m} {r}, {r}, {s}")                                # CA format
    emit(f"lw {r}, {random.choice([0, 4, 124])}({s})")            # c.lw
    emit(f"ld {r}, {random.choice([0, 8, 248])}({s})")            # c.ld
    emit(f"sw {r}, {random.choice([0, 4, 124])}({s})")            # c.sw
    emit(f"sd {r}, {random.choice([0, 8, 248])}({s})")            # c.sd
    emit(f"beq {r}, zero, {random.choice([-256, -2, 2, 254])}")   # c.beqz
    emit(f"bne {r}, zero, {random.choice([-256, -2, 2, 254])}")   # c.bnez
for fdr in FC:
    s = random.choice(XC)
    emit(f"fld {fdr}, {random.choice([0, 8, 248])}({s})")         # c.fld
    emit(f"fsd {fdr}, {random.choice([0, 8, 248])}({s})")         # c.fsd
for fdr in F:
    emit(f"fld {fdr}, {random.choice([0, 8, 504])}(sp)")          # c.fldsp
    emit(f"fsd {fdr}, {random.choice([0, 8, 504])}(sp)")          # c.fsdsp
for off in [-2048, -2, 2, 2046]:
    emit(f"jal zero, {off}")                                      # c.j


def assemble(src, attrs):
    res = subprocess.run(
        ["llvm-mc", "-triple=riscv64", f"-mattr={attrs}", "-M", "no-aliases", "-show-encoding"],
        input=src, capture_output=True, text=True)
    if res.returncode != 0:
        sys.exit(f"llvm-mc failed:\n{res.stderr}")
    out = []
    for line in res.stdout.splitlines():
        m = re.match(r"\s*(.*?)\s*# encoding: \[(.*)\]", line)
        if m:
            text = re.sub(r"\s+", " ", m.group(1).strip())
            enc = bytes(int(b, 16) for b in m.group(2).split(","))
            out.append((text, int.from_bytes(enc, "little"), len(enc)))
    return out


seen = set()
uniq = [line for line in lines if not (line in seen or seen.add(line))]
src = "\n".join(uniq) + "\n"
full = assemble(src, "+m,+a,+f,+d")
comp = assemble(src, "+m,+a,+f,+d,+c")
assert len(full) == len(comp) == len(uniq), (len(full), len(comp), len(uniq))

n_c = 0
with open(OUT, "w") as f:
    f.write("# Generated by tools/gen-decoder-vectors.py with "
            f"{next(l.strip() for l in subprocess.run(['llvm-mc', '--version'], capture_output=True, text=True).stdout.splitlines() if 'version' in l)}\n")
    f.write("# <enc32> <encC|----> <canonical llvm-mc -M no-aliases text of enc32>\n")
    for (text, enc, n), (_, cenc, cn) in zip(full, comp):
        assert n == 4, text
        c = f"{cenc:04x}" if cn == 2 else "----"
        n_c += cn == 2
        f.write(f"{enc:08x} {c} {text}\n")
print(f"gen-decoder-vectors: {len(full)} vectors ({n_c} with a compressed form) -> {OUT.relative_to(ROOT)}")
