use core::fmt::{self, Write};

use crate::drivers::uart::Uart;

pub fn _print(arguments: fmt::Arguments<'_>) {
    let mut uart = Uart::qemu_virt();
    let _ = uart.write_fmt(arguments);
}

pub fn emergency_print(arguments: fmt::Arguments<'_>) {
    // panic/トラップ経路では通常出力が整形中でも待たない。局所 UART から
    // MMIO へ直接書き、共有フォーマッタロックやグローバル状態に依存しない。
    let mut uart = Uart::qemu_virt();
    let _ = uart.write_fmt(arguments);
}

pub fn emergency_sbi_error(error: isize) {
    // SBI reset 失敗時も通常出力の保有状態は信用できないため、
    // emergency_print の局所 UART 経路だけを使う。
    emergency_print(format_args!("MiniOS: SBI reset error {error}\r\n"));
}

pub fn read_byte() -> u8 {
    // 受信待ちは UART の状態レジスタを polling する。割り込み自体は有効なため、
    // 待機中も supervisor timer の trap は通常どおり処理される。
    Uart::qemu_virt().read_byte()
}

pub fn write_byte(byte: u8) {
    // shell の一文字 echo も通常出力と同じ UART MMIO 経路へ直列化して送る。
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
