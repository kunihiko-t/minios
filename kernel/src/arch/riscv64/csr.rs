use core::arch::asm;

pub fn read_scause() -> usize {
    let value: usize;
    // Safety: S-mode トラップハンドラ内で scause を読むだけで、メモリやスタックを変更しない。
    unsafe { asm!("csrr {value}, scause", value = out(reg) value, options(nomem, nostack)) };
    value
}

pub fn read_sepc() -> usize {
    let value: usize;
    // Safety: S-mode トラップハンドラが sepc を読むだけで、実行再開位置は変更しない。
    unsafe { asm!("csrr {value}, sepc", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `sepc` に S-mode で実行再開可能な命令アドレスだけを書きます。
///
/// # Safety
///
/// 呼び出し側は S-mode であり、`value` が実装の IALIGN を満たし、`sret` 後に
/// 現在のアドレス空間で安全に実行できることを保証します。
// 回復可能な例外を扱う後続マイルストーン用の要求 API なので、この項目だけ一時的に未使用を許容する。
#[allow(dead_code)]
pub unsafe fn write_sepc(value: usize) {
    // Safety: 上記の S-mode、IALIGN、再開先の不変条件を呼び出し側が満たす場合に限る。
    unsafe { asm!("csrw sepc, {value}", value = in(reg) value, options(nomem, nostack)) };
}

pub fn read_stval() -> usize {
    let value: usize;
    // Safety: S-mode トラップ情報 stval の参照だけで CSR やメモリを書き換えない。
    unsafe { asm!("csrr {value}, stval", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `stvec` に S-mode トラップ入口と mode ビットを書きます。
///
/// # Safety
///
/// 呼び出し側は S-mode であり、BASE が規定境界に整列し、下位 2 bit が
/// Direct(0) または実装済みの mode で、入口が全トラップを安全に受けることを保証します。
pub unsafe fn write_stvec(value: usize) {
    // Safety: 上記の特権モード、BASE 整列、mode ビット、入口 ABI を呼び出し側が保証する。
    unsafe { asm!("csrw stvec, {value}", value = in(reg) value, options(nomem, nostack)) };
}

// 割り込み状態の保存を実装する後続マイルストーン用の要求 API なので、この項目だけ一時的に未使用を許容する。
#[allow(dead_code)]
pub fn read_sstatus() -> usize {
    let value: usize;
    // Safety: S-mode で sstatus の現在値を読むだけで、特権状態やメモリを変更しない。
    unsafe { asm!("csrr {value}, sstatus", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `sstatus` の S-mode から書き込み可能なビットだけを書きます。
///
/// # Safety
///
/// 呼び出し側は S-mode であり、予約/WARL ビットに無効値を入れず、SIE/SPIE/SPP
/// などの変更が現在のトラップフレームと `sret` の不変条件を壊さないことを保証します。
// 割り込み状態の復元を実装する後続マイルストーン用の要求 API なので、この項目だけ一時的に未使用を許容する。
#[allow(dead_code)]
pub unsafe fn write_sstatus(value: usize) {
    // Safety: 上記の S-mode、WARL/予約ビット、トラップ復帰状態の不変条件に従う。
    unsafe { asm!("csrw sstatus, {value}", value = in(reg) value, options(nomem, nostack)) };
}

// S-mode 割り込み許可の初期化を行う後続マイルストーン用の要求 API なので、この項目だけ一時的に未使用を許容する。
#[allow(dead_code)]
pub fn read_sie() -> usize {
    let value: usize;
    // Safety: S-mode で sie の許可ビットを読むだけで、割り込み状態は変更しない。
    unsafe { asm!("csrr {value}, sie", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `sie` の実装済み S-mode 割り込み許可ビットだけを書きます。
///
/// # Safety
///
/// 呼び出し側は S-mode であり、予約/WARL ビットを保存し、許可する全割り込みに
/// 対する初期化済みハンドラと共有状態が存在することを保証します。
// S-mode 割り込み許可を適用する後続マイルストーン用の要求 API なので、この項目だけ一時的に未使用を許容する。
#[allow(dead_code)]
pub unsafe fn write_sie(value: usize) {
    // Safety: 上記の S-mode、WARL/予約ビット、ハンドラ初期化の不変条件に従う。
    unsafe { asm!("csrw sie, {value}", value = in(reg) value, options(nomem, nostack)) };
}
