#![no_std]

//! MiniOS のホストテスト可能な純粋ロジックを置く crate です。
//!
//! RISC-V 固有の起動、CSR、SBI、MMIO は `main.rs` 以下で
//! `target_arch = "riscv64"` に限定し、ホストのライブラリテストを保ちます。
