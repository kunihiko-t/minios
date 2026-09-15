#[cfg(target_arch = "riscv32")]
pub mod neorv32_sd;
#[cfg(target_arch = "riscv32")]
pub mod neorv32_uart;
#[cfg(target_arch = "riscv64")]
pub mod uart;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-virtio"))]
pub mod virtio_mmio;
