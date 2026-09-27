//! Self-modifying code (CLAUDE.md §16, D11, D49). The mechanism is spread over the modules
//! that own the state it touches:
//!
//! * `mem::direct`: the per-page CODE mark (`DirectMem::mark_code`), host write protection of
//!   code pages, and `DirectMem::smc_pages`, filled by every write path that drops a mark;
//! * `mem::tlb::set_code_flag` and `mem::mmu::fill`: `TLB_CODE` on softmmu write tags;
//! * `interp`: `mark_block_code`, stopping a block after a store to a code page, dropping
//!   decoded blocks (`forget_smc_pages`);
//! * `jit::dispatch`: `page_tbs`, `drain_smc` (unlink + invalidate), the SMC exits and the
//!   direct-mode host-fault path (`smc_host_fault`), `fence_i`.
