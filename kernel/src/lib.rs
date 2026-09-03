#![no_std]

//! MiniOSのうち、ホストでテストできる純粋なロジックを置くクレートです。
//!
//! RISC-V固有の起動、CSR、SBI、MMIOは`main.rs`以下で
//! `target_arch = "riscv64"`に限定し、ホスト上のライブラリーテストから分離します。

// ホストテストでは、純粋なトラップ原因の解読を`arch::riscv64`から検証する。
// RISC-Vバイナリー側は`main.rs`の`arch`を使い、起動シンボルの二重定義を避ける。
// RV32ターゲット追加後は`not(riscv64)`がRV32のlibビルドにも一致してしまうため、
// ホストテスト用モジュールは`test`に限定する。RISC-Vのlib側には置かない。
#[cfg(test)]
pub mod arch;
#[cfg(any(test, target_arch = "riscv64"))]
pub mod boot_payload;
#[cfg(any(test, target_arch = "riscv64"))]
pub mod elf;
// `frame.rs`のビットマップとID採番はu64前提のため、RV32のlibビルドから外す。
// RV32用アロケーターは幅汎化と合わせて別途対応する。ホストテストでは検証を続ける。
#[cfg(any(test, target_arch = "riscv64"))]
pub mod memory;
pub mod sbi;
#[cfg(test)]
pub mod shell;
#[cfg(test)]
pub mod time;
#[cfg(any(test, target_arch = "riscv64"))]
pub mod user;
#[cfg(any(test, target_arch = "riscv64"))]
pub mod vm;

#[cfg(test)]
mod tests {
    use super::memory;
    use super::sbi::{SbiError, SbiRet};

    #[test]
    fn managed_memory_stops_before_the_boot_payload_window() {
        assert_eq!(memory::PHYSICAL_MEMORY_END, 0x8780_0000);
        assert_eq!(memory::BOOT_PAYLOAD_START, memory::PHYSICAL_MEMORY_END);
        assert_eq!(memory::BOOT_PAYLOAD_END, 0x8800_0000);
    }

    #[test]
    fn kernel_sections_require_page_aligned_non_overlapping_ranges() {
        let sections = memory::KernelSections::new(
            0x8020_0000..0x8020_2000,
            0x8020_2000..0x8020_3000,
            0x8020_3000..0x8020_5000,
            0x8020_5000..0x8021_5000,
            0x8021_5000,
        )
        .unwrap();

        assert_eq!(sections.kernel_end(), 0x8021_5000);
    }

    #[test]
    fn kernel_sections_reject_unaligned_ranges() {
        assert!(matches!(
            memory::KernelSections::new(
                0x8020_0001..0x8020_2000,
                0x8020_2000..0x8020_3000,
                0x8020_3000..0x8020_5000,
                0x8020_5000..0x8021_5000,
                0x8021_5000,
            ),
            Err(memory::LayoutError::Unaligned)
        ));
    }

    #[test]
    fn kernel_sections_reject_empty_ranges() {
        assert!(matches!(
            memory::KernelSections::new(
                0x8020_0000..0x8020_0000,
                0x8020_2000..0x8020_3000,
                0x8020_3000..0x8020_5000,
                0x8020_5000..0x8021_5000,
                0x8021_5000,
            ),
            Err(memory::LayoutError::Empty)
        ));
    }

    #[test]
    fn kernel_sections_reject_overlapping_ranges() {
        assert!(matches!(
            memory::KernelSections::new(
                0x8020_0000..0x8020_2000,
                0x8020_1000..0x8020_3000,
                0x8020_3000..0x8020_5000,
                0x8020_5000..0x8021_5000,
                0x8021_5000,
            ),
            Err(memory::LayoutError::Overlap)
        ));
    }

    #[test]
    fn kernel_sections_require_boot_stack_to_end_at_kernel_end() {
        assert!(matches!(
            memory::KernelSections::new(
                0x8020_0000..0x8020_2000,
                0x8020_2000..0x8020_3000,
                0x8020_3000..0x8020_5000,
                0x8020_5000..0x8021_5000,
                0x8021_6000,
            ),
            Err(memory::LayoutError::EndMismatch)
        ));
    }

    #[test]
    fn sbi_return_value_is_available_on_success() {
        assert_eq!(
            SbiRet {
                error: 0,
                value: 42,
            }
            .into_result(),
            Ok(42)
        );
    }

    #[test]
    fn sbi_error_is_preserved_on_failure() {
        assert_eq!(
            SbiRet {
                error: -2,
                value: 42,
            }
            .into_result(),
            Err(SbiError(-2))
        );
    }
}
