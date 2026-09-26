//! Compiles Berkeley SoftFloat 3e with the RISC-V specialization (D13): canonical NaNs,
//! tininess after rounding and RISC-V's saturating conversions, i.e. bit-exact with Spike.
//! The object lists are taken from SoftFloat's own Linux-x86_64-GCC Makefile, so the build
//! matches the reference configuration exactly.

use std::path::Path;

fn objs(makefile: &str, var: &str) -> Vec<String> {
    let start = makefile
        .find(&format!("{var} = \\"))
        .unwrap_or_else(|| panic!("{var} not found in SoftFloat Makefile"));
    let mut out = Vec::new();
    for line in makefile[start..].lines().skip(1) {
        let name = line.trim().trim_end_matches('\\').trim();
        if let Some(stem) = name.strip_suffix("$(OBJ)") {
            out.push(stem.to_string());
        }
        if !line.trim_end().ends_with('\\') {
            break;
        }
    }
    out
}

fn main() {
    let sf = Path::new("third_party/berkeley-softfloat-3");
    let mk_path = sf.join("build/Linux-x86_64-GCC/Makefile");
    let mk = std::fs::read_to_string(&mk_path)
        .expect("SoftFloat submodule missing: run `git submodule update --init --recursive`");
    let mut b = cc::Build::new();
    b.include(sf.join("build/Linux-x86_64-GCC"))
        .include(sf.join("source/RISCV"))
        .include(sf.join("source/include"))
        .define("SOFTFLOAT_FAST_INT64", None)
        .define("SOFTFLOAT_ROUND_ODD", None)
        .define("INLINE_LEVEL", "5")
        .define("SOFTFLOAT_FAST_DIV32TO16", None)
        .define("SOFTFLOAT_FAST_DIV64TO32", None)
        // Rounding mode and exception flags are per thread (cargo test runs tests in parallel).
        .define("THREAD_LOCAL", "_Thread_local")
        .opt_level(2)
        .warnings(false);
    for o in objs(&mk, "OBJS_PRIMITIVES")
        .into_iter()
        .chain(objs(&mk, "OBJS_OTHERS"))
    {
        b.file(sf.join("source").join(format!("{o}.c")));
    }
    for o in objs(&mk, "OBJS_SPECIALIZE") {
        b.file(sf.join("source/RISCV").join(format!("{o}.c")));
    }
    b.file("src/cpu/softfloat_shim.c");
    b.compile("softfloat");
    println!("cargo:rerun-if-changed=src/cpu/softfloat_shim.c");
    println!("cargo:rerun-if-changed={}", mk_path.display());
}
