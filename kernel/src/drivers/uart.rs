use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

// QEMU virtの16550互換UARTに予約されたMMIOベースアドレスの既定値である。
// `kernel_main`はFDTから得た値で上書きする。FDT解析に失敗した場合の
// 緊急診断だけが、この既定値を頼りにする。
const QEMU_VIRT_UART_BASE: usize = 0x1000_0000;
static UART_BASE: AtomicUsize = AtomicUsize::new(QEMU_VIRT_UART_BASE);
const LINE_STATUS_OFFSET: usize = 5;
const RECEIVE_READY: u8 = 1 << 0;
const TRANSMIT_READY: u8 = 1 << 5;

pub struct Uart {
    base: *mut u8,
}

impl Uart {
    /// FDTの`ns16550a`nodeから発見したUARTベースを登録する。
    /// `kernel_main`が一度だけ呼ぶ。
    pub fn set_base(base: usize) {
        UART_BASE.store(base, Ordering::Relaxed);
    }

    /// 登録済みのUARTベースでdriverを作る。`set_base`前はQEMU `virt`の既定値を使う。
    pub fn for_target() -> Self {
        Self {
            base: UART_BASE.load(Ordering::Relaxed) as *mut u8,
        }
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
