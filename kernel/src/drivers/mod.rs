#[cfg(target_arch = "riscv32")]
pub mod neorv32_uart;
#[cfg(target_arch = "riscv64")]
pub mod uart;
