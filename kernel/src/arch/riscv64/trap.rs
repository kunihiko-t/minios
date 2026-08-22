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
    // `trap.S` が 4-byte 整列で公開する Direct-mode トラップ入口だけを参照する。
    unsafe extern "C" {
        fn __trap_entry();
    }

    let direct_entry = __trap_entry as *const () as usize;
    // Safety: kernel_main は OpenSBI から S-mode で呼ばれ、__trap_entry は下位 2 bit が
    // 0 になる 4-byte 整列済み BASE であり、Direct mode の全トラップを保存フレームで受ける。
    unsafe { super::csr::write_stvec(direct_entry) };
}

#[cfg(target_arch = "riscv64")]
// trap.S が生シンボル名と C ABI で call するため、この名前と ABI を変えてはならない。
#[unsafe(no_mangle)]
pub extern "C" fn rust_trap_handler() {
    let scause = super::csr::read_scause();
    let sepc = super::csr::read_sepc();
    let stval = super::csr::read_stval();
    let cause = decode_scause(scause);

    // supervisor timer interrupt の code 5 だけを通常復帰可能な割り込みとして扱う。
    // SBI が再設定を拒否した場合は次回 deadline を保証できないため、診断して失敗停止する。
    if cause == TrapCause::Interrupt(5) {
        if let Err(error) = crate::time::handle_interrupt() {
            crate::fatal_timer_error("interrupt rearm", error);
        }
        return;
    }

    #[cfg(feature = "qemu-test-trap")]
    // RISC-V 特権仕様で breakpoint 同期例外の cause code は 3 に固定されるため、テスト成功分岐はこの値だけを受ける。
    if cause == TrapCause::Exception(3) {
        // 同期例外は通常出力の途中に発生し得るため、共有ロックを使わない
        // 局所 UART の緊急経路で marker を出す。
        crate::console::emergency_print(format_args!("[MINIOS_TEST] trap: ok\r\n"));
        super::sbi::system_reset(
            super::sbi::ResetType::Shutdown,
            super::sbi::ResetReason::NoReason,
        );
    }

    // 予期しないトラップでは通常フォーマッタが保有中かもしれないため、
    // 直接 UART 経路で原因と 3 CSR を一度に記録する。
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
