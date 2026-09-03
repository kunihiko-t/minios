#[cfg(target_arch = "riscv32")]
// Safety: `entry.S`はNEORV32のMモード起動(`pc=0`、スタック未設定、BSS未初期化)を前提にする。
// Rustを呼ぶ前にスタック・BSS・`mtvec`の不変条件を確立するため、異なるアーキテクチャーへは組み込まない。
core::arch::global_asm!(include_str!("entry.S"));
