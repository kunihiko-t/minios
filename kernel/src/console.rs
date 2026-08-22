use core::fmt::{self, Write};

use crate::drivers::uart::Uart;

pub fn _print(arguments: fmt::Arguments<'_>) {
    let mut uart = Uart::qemu_virt();
    let _ = uart.write_fmt(arguments);
}

pub fn emergency_sbi_error(error: isize) {
    let mut uart = Uart::qemu_virt();
    let _ = write!(uart, "MiniOS: SBI reset error {error}\r\n");
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
