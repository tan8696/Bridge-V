//! Runtime host feature detection (CLAUDE.md §4: the JIT may use BMI2/FMA3/POPCNT only after
//! checking `cpuid`). `std::is_x86_feature_detected!` executes `cpuid` (and checks OS XSAVE
//! support for AVX-class features) once and caches the result.

use std::sync::OnceLock;

/// Optional host ISA extensions the back end can use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostFeatures {
    pub bmi1: bool,
    pub bmi2: bool,
    pub fma: bool,
    pub popcnt: bool,
    pub lzcnt: bool,
    pub avx2: bool,
    pub sse41: bool,
}

impl HostFeatures {
    /// Query the running CPU.
    pub fn detect() -> HostFeatures {
        HostFeatures {
            bmi1: std::is_x86_feature_detected!("bmi1"),
            bmi2: std::is_x86_feature_detected!("bmi2"),
            fma: std::is_x86_feature_detected!("fma"),
            popcnt: std::is_x86_feature_detected!("popcnt"),
            lzcnt: std::is_x86_feature_detected!("lzcnt"),
            avx2: std::is_x86_feature_detected!("avx2"),
            sse41: std::is_x86_feature_detected!("sse4.1"),
        }
    }

    /// The x86-64 baseline (SSE2 only): what `--no-host-features` forces.
    pub fn baseline() -> HostFeatures {
        HostFeatures::default()
    }
}

static HOST: OnceLock<HostFeatures> = OnceLock::new();

/// Features of this host, detected once per process.
pub fn host() -> HostFeatures {
    *HOST.get_or_init(HostFeatures::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_is_stable_and_baseline_is_empty() {
        assert_eq!(host(), HostFeatures::detect());
        assert_eq!(HostFeatures::baseline(), HostFeatures::default());
        // Every AVX2 CPU also implements SSE4.1; a contradiction means detection is broken.
        let f = host();
        assert!(!f.avx2 || f.sse41);
    }
}
