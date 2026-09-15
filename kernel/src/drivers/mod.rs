#[cfg(target_arch = "riscv32")]
pub mod neorv32_sd;
#[cfg(target_arch = "riscv32")]
pub mod neorv32_uart;
#[cfg(target_arch = "riscv64")]
pub mod uart;
#[cfg(target_arch = "riscv64")]
pub mod virtio_mmio;
