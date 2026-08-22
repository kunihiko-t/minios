#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCause {
    Interrupt(usize),
    Exception(usize),
}

pub fn decode_scause(value: usize) -> TrapCause {
    let interrupt_bit = 1usize << (usize::BITS - 1);
    let code = value & !interrupt_bit;
    if value & interrupt_bit == 0 {
        TrapCause::Exception(code)
    } else {
        TrapCause::Interrupt(code)
    }
}

#[cfg(target_arch = "riscv64")]
pub fn init() {
    // Safety: RISC-Vビルドでは、`trap.S`が4バイト境界にそろえたC ABIシンボルを必ず公開する。
    // この処理はアドレスを作るだけで、入口コードのメモリーを読み書きしない。
    unsafe extern "C" {
        fn __trap_entry();
    }

    let direct_entry = __trap_entry as *const () as usize;
    // Safety: `kernel_main`はOpenSBIからS-modeで呼ばれている。
    // `__trap_entry`は下位2ビットが0になる4バイト境界のBASEであり、Directモードの全トラップを保存フレームで受ける。
    unsafe { super::csr::write_stvec(direct_entry) };
}

#[cfg(target_arch = "riscv64")]
// `trap.S`がシンボル名とC ABIを直接指定して呼ぶため、この名前とABIを変えてはならない。
#[unsafe(no_mangle)]
pub extern "C" fn rust_trap_handler() {
    let scause = super::csr::read_scause();
    let sepc = super::csr::read_sepc();
    let stval = super::csr::read_stval();
    let cause = decode_scause(scause);

    // Supervisorタイマー割り込みの原因コード5だけを、通常実行へ戻れる割り込みとして扱う。
    // SBIが再設定を拒否すると次の発火時刻を保証できないため、診断を出して異常終了する。
    if cause == TrapCause::Interrupt(5) {
        if let Err(error) = crate::time::handle_interrupt() {
            crate::fatal_timer_error("interrupt rearm", error);
        }
        return;
    }

    #[cfg(feature = "qemu-test-trap")]
    // RISC-V特権仕様では、同期ブレークポイント例外の原因コードが3に固定されているため、テスト成功分岐はこの値だけを受ける。
    if cause == TrapCause::Exception(3) {
        // 同期例外は通常出力の途中でも起こり得るため、共有ロックを使わない局所UARTの緊急経路でマーカーを出す。
        crate::console::emergency_print(format_args!("[MINIOS_TEST] trap: ok\r\n"));
        super::sbi::system_reset(
            super::sbi::ResetType::Shutdown,
            super::sbi::ResetReason::NoReason,
        );
    }

    // 予期しないトラップでは通常の書式処理がロックを保持している可能性がある。
    // UARTへ直接書く経路を使い、原因と三つのCSRを一度に記録する。
    crate::console::emergency_print(format_args!(
        "MiniOS trap: cause={cause:?} scause={scause:#018x} sepc={sepc:#018x} stval={stval:#018x}\r\n"
    ));
    super::sbi::system_reset(
        super::sbi::ResetType::Shutdown,
        super::sbi::ResetReason::SystemFailure,
    );
}

#[cfg(test)]
mod tests {
    use super::{TrapCause, decode_scause};

    #[test]
    fn decodes_supervisor_timer_interrupt() {
        let value = (1usize << (usize::BITS - 1)) | 5;
        assert_eq!(decode_scause(value), TrapCause::Interrupt(5));
    }

    #[test]
    fn decodes_breakpoint_exception() {
        assert_eq!(decode_scause(3), TrapCause::Exception(3));
    }
}
