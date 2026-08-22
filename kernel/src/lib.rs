#![no_std]

//! MiniOS のホストテスト可能な純粋ロジックを置く crate です。
//!
//! RISC-V 固有の起動、CSR、SBI、MMIO は `main.rs` 以下で
//! `target_arch = "riscv64"` に限定し、ホストのライブラリテストを保ちます。

// ホストテストでは純粋なトラップ解読を `arch::riscv64` から検証する。RISC-V
// バイナリ側は `main.rs` の arch を使い、起動シンボルの二重定義を避ける。
#[cfg(not(target_arch = "riscv64"))]
pub mod arch;
pub mod memory;
pub mod sbi;
#[cfg(not(target_arch = "riscv64"))]
pub mod time;

#[cfg(test)]
mod tests {
    use super::sbi::{SbiError, SbiRet};

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
