use core::fmt::{self, Write};

#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
use core::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::uart::Uart;

/// Ready frame送信後に立てる。このmodeではguest UARTにplain textを混在させず、
/// 通常出力も緊急出力もcontrol frameへ載せ替える。
#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
static CONTROL_MODE: AtomicBool = AtomicBool::new(false);

/// Ready frameの送信が完了した後にcontrol.rsから呼ぶ。
#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
pub fn enter_control_mode() {
    CONTROL_MODE.store(true, Ordering::Relaxed);
}

#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
fn control_mode() -> bool {
    CONTROL_MODE.load(Ordering::Relaxed)
}

// UART control writerを持たないbuildでは、Ready送信経路が存在しないため
// control modeへ移行しない。
#[cfg(not(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit")))]
fn control_mode() -> bool {
    false
}

// control frameを載せる経路は、UART control writerを持つbuildだけに存在する。
#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
fn control_diagnostic(text: &[u8]) {
    crate::control::send_diagnostic(text);
}

#[cfg(not(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit")))]
fn control_diagnostic(_text: &[u8]) {}

#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
fn control_guest_error(text: &[u8]) {
    crate::control::send_guest_error(text);
}

#[cfg(not(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit")))]
fn control_guest_error(_text: &[u8]) {}

/// control frameのheaderやpayloadなど、整形しないbyte列をUARTへそのまま書く。
#[cfg(any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit"))]
pub fn write_bytes(bytes: &[u8]) {
    let mut uart = Uart::qemu_virt();
    for byte in bytes {
        uart.write_byte(*byte);
    }
}

/// control mode中の通常出力を載せるDiagnostic frameの一次buffer。
struct DiagnosticBuffer {
    bytes: [u8; 256],
    len: usize,
    overflowed: bool,
}

impl DiagnosticBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; 256],
            len: 0,
            overflowed: false,
        }
    }

    fn text(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Write for DiagnosticBuffer {
    fn write_str(&mut self, fragment: &str) -> fmt::Result {
        let rest = &mut self.bytes[self.len..];
        if fragment.len() > rest.len() {
            // 溢れた分は切り詰める。診断の後半より、frameとして届く前半を優先する。
            rest.copy_from_slice(&fragment.as_bytes()[..rest.len()]);
            self.len = self.bytes.len();
            self.overflowed = true;
            return Err(fmt::Error);
        }
        rest[..fragment.len()].copy_from_slice(fragment.as_bytes());
        self.len += fragment.len();
        Ok(())
    }
}

fn format_into_buffer(arguments: fmt::Arguments<'_>) -> DiagnosticBuffer {
    let mut buffer = DiagnosticBuffer::new();
    let _ = buffer.write_fmt(arguments);
    buffer
}

pub fn _print(arguments: fmt::Arguments<'_>) {
    // Ready後の通常出力はDiagnostic frameだけをUARTへ流す。
    if control_mode() {
        let buffer = format_into_buffer(arguments);
        control_diagnostic(buffer.text());
        return;
    }
    let mut uart = Uart::qemu_virt();
    let _ = uart.write_fmt(arguments);
}

pub fn emergency_print(arguments: fmt::Arguments<'_>) {
    // Ready後のpanicやfatal trapはplain consoleへ書かず、GuestError frameで届ける。
    if control_mode() {
        let buffer = format_into_buffer(arguments);
        control_guest_error(buffer.text());
        return;
    }
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
