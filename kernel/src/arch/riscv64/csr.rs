use core::arch::asm;

pub fn read_scause() -> usize {
    let value: usize;
    // Safety: S-modeのトラップハンドラー内で`scause`を読むだけで、メモリーとスタックを変更しない。
    unsafe { asm!("csrr {value}, scause", value = out(reg) value, options(nomem, nostack)) };
    value
}

pub fn read_sepc() -> usize {
    let value: usize;
    // Safety: S-modeのトラップハンドラーが`sepc`を読むだけで、実行再開位置は変更しない。
    unsafe { asm!("csrr {value}, sepc", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `sepc`に、S-modeで実行を再開できる命令アドレスだけを書きます。
///
/// # Safety
///
/// 呼び出し側はS-modeであり、`value`が実装のIALIGNを満たし、`sret`後に
/// 現在のアドレス空間で安全に実行できることを保証します。
// 回復可能な例外を扱う次の段階で必要になるAPIなので、この関数だけ一時的に未使用を許す。
#[allow(dead_code)]
pub unsafe fn write_sepc(value: usize) {
    // Safety: 上記のS-mode、IALIGN、再開先の不変条件を呼び出し側が満たす場合に限る。
    unsafe { asm!("csrw sepc, {value}", value = in(reg) value, options(nomem, nostack)) };
}

pub fn read_stval() -> usize {
    let value: usize;
    // Safety: S-modeのトラップ情報`stval`を読むだけで、CSRとメモリーを書き換えない。
    unsafe { asm!("csrr {value}, stval", value = out(reg) value, options(nomem, nostack)) };
    value
}

pub fn read_time() -> u64 {
    let value: u64;
    // Safety: `time`は読み取り専用カウンターである。
    // S-modeから読むだけではCSR、メモリー、スタックの状態を変更しない。
    unsafe { asm!("csrr {value}, time", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `stvec`に、S-modeのトラップ入口とモードビットを書きます。
///
/// # Safety
///
/// 呼び出し側はS-modeであり、BASEが規定の境界にそろい、下位2ビットが
/// Direct（0）または実装済みのモードであり、入口が全トラップを安全に受けることを保証します。
pub unsafe fn write_stvec(value: usize) {
    // Safety: 上記の特権モード、BASEのアラインメント、モードビット、入口ABIを呼び出し側が保証する。
    unsafe { asm!("csrw stvec, {value}", value = in(reg) value, options(nomem, nostack)) };
}

pub fn read_sstatus() -> usize {
    let value: usize;
    // Safety: S-modeで`sstatus`の現在値を読むだけで、特権状態とメモリーを変更しない。
    unsafe { asm!("csrr {value}, sstatus", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `sstatus`のうち、S-modeから書き込めるビットだけを書きます。
///
/// # Safety
///
/// 呼び出し側はS-modeであり、予約ビットとWARLビットに無効値を入れず、SIE、SPIE、SPP
/// などの変更が現在のトラップフレームと `sret` の不変条件を壊さないことを保証します。
pub unsafe fn write_sstatus(value: usize) {
    // Safety: 上記のS-mode、WARLビットと予約ビット、トラップ復帰状態の不変条件に従う。
    unsafe { asm!("csrw sstatus, {value}", value = in(reg) value, options(nomem, nostack)) };
}

pub fn read_sie() -> usize {
    let value: usize;
    // Safety: S-modeで`sie`の許可ビットを読むだけで、割り込み状態は変更しない。
    unsafe { asm!("csrr {value}, sie", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// `sie`のうち、実装済みのS-mode割り込み許可ビットだけを書きます。
///
/// # Safety
///
/// 呼び出し側はS-modeであり、予約ビットとWARLビットを保ち、許可する全割り込みについて
/// 初期化済みのハンドラーと共有状態が存在することを保証します。
pub unsafe fn write_sie(value: usize) {
    // Safety: 上記のS-mode、WARLビットと予約ビット、ハンドラー初期化の不変条件に従う。
    unsafe { asm!("csrw sie, {value}", value = in(reg) value, options(nomem, nostack)) };
}
