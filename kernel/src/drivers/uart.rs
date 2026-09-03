use core::fmt;

// QEMU virtの16550互換UARTに予約された、固定のMMIOベースアドレスである。
const UART_BASE: usize = 0x1000_0000;
const LINE_STATUS_OFFSET: usize = 5;
const RECEIVE_READY: u8 = 1 << 0;
const TRANSMIT_READY: u8 = 1 << 5;

pub struct Uart {
    base: *mut u8,
}

impl Uart {
    pub const fn qemu_virt() -> Self {
        // Safety: `UART_BASE`はQEMU virtの仕様でUARTレジスターを指す固定アドレスである。
        // この型はRISC-V向けQEMUカーネルだけで使うため、生ポインターの参照先はMMIO領域に限られる。
        Self {
            base: UART_BASE as *mut u8,
        }
    }

    pub const fn for_target() -> Self {
        Self::qemu_virt()
    }

    pub fn write_byte(&mut self, byte: u8) {
        while self.line_status() & TRANSMIT_READY == 0 {
            core::hint::spin_loop();
        }
        // Safety: `base`は`qemu_virt`が作る16550のMMIO領域であり、オフセット0は送信保持レジスターである。
        // volatileな書き込みにより、コンパイラーが機器への操作を省略しない。
        unsafe { core::ptr::write_volatile(self.base, byte) };
    }

    pub fn read_byte(&mut self) -> u8 {
        while !self.has_byte() {
            core::hint::spin_loop();
        }
        // Safety: `base`は`qemu_virt`が作る16550のMMIO領域であり、オフセット0は受信保持レジスターである。
        // volatileな読み取りにより、コンパイラーが前回の値を再利用しない。
        unsafe { core::ptr::read_volatile(self.base) }
    }

    pub fn has_byte(&self) -> bool {
        self.line_status() & RECEIVE_READY != 0
    }

    fn line_status(&self) -> u8 {
        // Safety: `base + 5`は16550のLine Status Registerであり、この固定オフセットはQEMU virtのUART仕様に従う。
        // volatileな読み取りで状態ビットを毎回取得する。
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
