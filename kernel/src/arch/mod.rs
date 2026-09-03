// ホストテストでは純粋な`TrapCause`解読を検証するため、`test`でも組み込む。
#[cfg(target_arch = "riscv32")]
pub mod riscv32;
#[cfg(any(test, target_arch = "riscv64"))]
pub mod riscv64;
