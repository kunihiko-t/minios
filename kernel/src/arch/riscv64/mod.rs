#[cfg(target_arch = "riscv64")]
// このアセンブリは RISC-V 64 のみで有効であり、ホストテストへは混入しない。
core::arch::global_asm!(include_str!("entry.S"));
#[cfg(target_arch = "riscv64")]
// `__trap_entry` と Rust ハンドラの ABI 契約は RISC-V 64 専用である。
core::arch::global_asm!(include_str!("trap.S"));

#[cfg(target_arch = "riscv64")]
pub mod csr;
#[cfg(target_arch = "riscv64")]
pub mod sbi;
pub mod trap;
