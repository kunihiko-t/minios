#[cfg(target_arch = "riscv64")]
// Safety: `entry.S`はRV64のOpenSBI起動ABI（`a0`はハートID、`a1`はDTB）と、
// `linker.ld`が定義するスタックおよびBSSシンボルを前提にする。
// RISC-Vターゲットだけへ組み込み、Rustを呼ぶ前に必要な不変条件を確立する。
core::arch::global_asm!(include_str!("entry.S"));
#[cfg(target_arch = "riscv64")]
// Safety: `trap.S`はRV64のレジスター幅、256バイトのフレーム、`rust_trap_handler`のC ABIを前提にする。
// 保存した全レジスターを復元してから`sret`するため、ホストや異なるアーキテクチャーへは組み込まない。
core::arch::global_asm!(include_str!("trap.S"));
#[cfg(target_arch = "riscv64")]
// Safety: `user.S`はRV64のレジスター幅、34語の`UserContext` C ABI layout、
// 144バイトのkernel trap stack固定header、`rust_user_trap_handler`のC ABIを前提にする。
// `sret`でU-modeへ降りてsatpを交換するため、ホストや異なるアーキテクチャーへは組み込まない。
core::arch::global_asm!(include_str!("user.S"));

pub mod csr;
#[cfg(target_arch = "riscv64")]
pub mod sbi;
pub mod trap;
