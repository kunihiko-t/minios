#[cfg(target_arch = "riscv64")]
// Safety: entry.S は RV64 の OpenSBI entry ABI (a0=hart ID, a1=DTB) と linker.ld の
// stack/BSS symbolを前提にする。RISC-V targetだけへ組み込み、Rustを呼ぶ前にその不変条件を確立する。
core::arch::global_asm!(include_str!("entry.S"));
#[cfg(target_arch = "riscv64")]
// Safety: trap.S はRV64 register幅と256-byte frame、rust_trap_handlerのC ABIを前提にし、
// 全保存registerを復元してからsretする。hostや異なるarchitectureへは組み込まない。
core::arch::global_asm!(include_str!("trap.S"));

#[cfg(target_arch = "riscv64")]
pub mod csr;
#[cfg(target_arch = "riscv64")]
pub mod sbi;
pub mod trap;
