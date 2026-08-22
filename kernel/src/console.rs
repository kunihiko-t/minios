use core::fmt::{self, Write};

use crate::drivers::uart::Uart;

pub fn _print(arguments: fmt::Arguments<'_>) {
    let mut uart = Uart::qemu_virt();
    let _ = uart.write_fmt(arguments);
}

pub fn emergency_print(arguments: fmt::Arguments<'_>) {
    // パニックやトラップでは、通常出力の書式処理が終わるのを待てない。
    // 局所的なUARTからMMIOへ直接書き、共有の書式処理やグローバル状態に依存しない。
    let mut uart = Uart::qemu_virt();
    let _ = uart.write_fmt(arguments);
}

pub fn emergency_sbi_error(error: isize) {
    // SBIリセットが失敗した時点では通常出力の状態を信用できないため、`emergency_print`の局所UART経路だけを使う。
    emergency_print(format_args!("MiniOS: SBI reset error {error}\r\n"));
}

pub fn read_byte() -> u8 {
    // UARTの状態レジスターを繰り返し読み、受信を待つ。
    // 割り込みは有効なため、待機中もSupervisorタイマーのトラップを処理できる。
    Uart::qemu_virt().read_byte()
}

pub fn write_byte(byte: u8) {
    // シェルの1文字エコーも、通常出力と同じUARTのMMIO経路へ順に送る。
    Uart::qemu_virt().write_byte(byte);
}

#[macro_export]
macro_rules! print {
    ($($argument:tt)*) => {
        $crate::console::_print(core::format_args!($($argument)*));
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n");
    };
    ($($argument:tt)*) => {
        $crate::print!("{}\n", core::format_args!($($argument)*));
    };
}
