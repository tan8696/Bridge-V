// Accessors for SoftFloat's thread-local state, which Rust cannot reference directly.
#include <stdint.h>
#include "softfloat.h"

void bv_sf_set_rounding_mode(uint8_t rm) { softfloat_roundingMode = rm; }

// Return the accrued exception flags (same bit layout as RISC-V fflags) and clear them.
uint8_t bv_sf_take_flags(void) {
    uint8_t f = softfloat_exceptionFlags;
    softfloat_exceptionFlags = 0;
    return f;
}
