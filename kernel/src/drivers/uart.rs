use core::fmt;

// QEMU virt の 16550 互換 UART に予約された固定 MMIO ベースアドレスである。
const UART_BASE: usize = 0x1000_0000;
const LINE_STATUS_OFFSET: usize = 5;
// Task 6 で UART 受信を接続するまでの計画済み API 用ビットなので、この項目だけ未使用を許容する。
#[allow(dead_code)]
const RECEIVE_READY: u8 = 1 << 0;
const TRANSMIT_READY: u8 = 1 << 5;

pub struct Uart {
    base: *mut u8,
}

impl Uart {
    pub const fn qemu_virt() -> Self {
        // Safety: UART_BASE は QEMU virt の仕様で UART レジスタを指す固定アドレスであり、
        // この型は RISC-V QEMU カーネルだけで使うため、この生ポインタは MMIO に限定される。
        Self {
            base: UART_BASE as *mut u8,
        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        while self.line_status() & TRANSMIT_READY == 0 {
            core::hint::spin_loop();
        }
        // Safety: base は qemu_virt が作る 16550 MMIO 領域であり、offset 0 は送信保持
        // レジスタである。volatile によりコンパイラがデバイス書き込みを省略しない。
        unsafe { core::ptr::write_volatile(self.base, byte) };
    }

    // Task 6 で入力ループから呼ぶ公開 API を先に保つため、このマイルストーン間だけ未使用を許容する。
    #[allow(dead_code)]
    pub fn read_byte(&mut self) -> u8 {
        while !self.has_byte() {
            core::hint::spin_loop();
        }
        // Safety: base は qemu_virt が作る 16550 MMIO 領域であり、offset 0 は受信保持
        // レジスタである。volatile によりデバイス読み出しをキャッシュしない。
        unsafe { core::ptr::read_volatile(self.base) }
    }

    // Task 6 で非ブロッキング受信に使う公開 API なので、この項目だけ一時的に未使用を許容する。
    #[allow(dead_code)]
    pub fn has_byte(&self) -> bool {
        self.line_status() & RECEIVE_READY != 0
    }

    fn line_status(&self) -> u8 {
        // Safety: base + 5 は 16550 Line Status Register であり、この固定オフセットは
        // QEMU virt の UART 仕様に従う。volatile 読み出しで状態ビットを毎回取得する。
        unsafe { core::ptr::read_volatile(self.base.add(LINE_STATUS_OFFSET)) }
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for byte in value.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}
