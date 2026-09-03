use core::fmt;

// NEORV32のプライマリーUART(UART0)の固定MMIOベースアドレスである。
const UART_BASE: usize = 0xFFF5_0000;
const CTRL_OFFSET: usize = 0x00;
const DATA_OFFSET: usize = 0x04;
// NEORV32のデータシートが規定する96 MHz基準クロックと19,200 baudである。
const SYSTEM_CLOCK_HZ: u32 = 96_000_000;
const BAUD_RATE: u32 = 19_200;
// `neorv32_uart.vhd`のCTRLビット定義である。
const TX_FULL: u32 = 1 << 21;
const RX_NOT_EMPTY: u32 = 1 << 16;

pub struct Neorv32Uart {
    ctrl: *mut u32,
    data: *mut u32,
}

impl Neorv32Uart {
    pub const fn neorv32() -> Self {
        // Safety: `UART_BASE`はNEORV32の仕様でUART0レジスターを指す固定アドレスである。
        // この型はRV32向けNEORV32カーネルだけで使うため、生ポインターの参照先はMMIO領域に限られる。
        Self {
            ctrl: (UART_BASE + CTRL_OFFSET) as *mut u32,
            data: (UART_BASE + DATA_OFFSET) as *mut u32,
        }
    }

    pub const fn for_target() -> Self {
        Self::neorv32()
    }

    pub fn init(&mut self) {
        // Safety: 生成直後のCTRLへ一度だけボーレート設定を書く。
        // volatileな書き込みにより、コンパイラーが機器への操作を省略しない。
        unsafe { core::ptr::write_volatile(self.ctrl, uart_control(SYSTEM_CLOCK_HZ, BAUD_RATE)) };
    }

    pub fn write_byte(&mut self, byte: u8) {
        while self.control() & TX_FULL != 0 {
            core::hint::spin_loop();
        }
        // Safety: `data`はUART0の送受信レジスターであり、TX FIFOに空きがあることを確認済みである。
        unsafe { core::ptr::write_volatile(self.data, byte as u32) };
    }

    pub fn read_byte(&mut self) -> u8 {
        while !self.has_byte() {
            core::hint::spin_loop();
        }
        // Safety: `data`はUART0の送受信レジスターであり、RX FIFOに受信済みがあることを確認済みである。
        unsafe { core::ptr::read_volatile(self.data) as u8 }
    }

    pub fn has_byte(&self) -> bool {
        self.control() & RX_NOT_EMPTY != 0
    }

    fn control(&self) -> u32 {
        // Safety: `ctrl`はUART0の制御レジスターであり、状態ビットの読み取りに副作用はない。
        unsafe { core::ptr::read_volatile(self.ctrl) }
    }
}

const fn uart_control(clock_hz: u32, baud_rate: u32) -> u32 {
    // NEORV32の`neorv32_uart_setup`と同じ計算である。
    // 実機で受信確認済みの値を保つため、この式と期待値を変えてはならない。
    if clock_hz == 0 || baud_rate == 0 {
        return 0;
    }

    let mut prescaler = 0u32;
    let mut divisor = (clock_hz as u64) / (2 * baud_rate as u64);

    while divisor >= 0x3ff {
        divisor >>= if prescaler == 2 || prescaler == 4 {
            3
        } else {
            1
        };
        prescaler += 1;
    }

    1 | (prescaler << 3) | (((divisor as u32) - 1) << 6)
}

impl fmt::Write for Neorv32Uart {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for byte in value.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}
