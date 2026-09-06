use minios_kernel::storage::sd::{Bus, SdError};

const GPIO_BASE: usize = 0xfffc_0000;
const SYSTEM_CLOCK_HZ: u32 = 96_000_000;
const CYCLES_PER_MILLISECOND: u32 = SYSTEM_CLOCK_HZ / 1_000;
const GPIO_SCK: u32 = 1 << 0;
const GPIO_MOSI: u32 = 1 << 1;
const GPIO_SELECT: u32 = 1 << 2;
const GPIO_MISO: u32 = 1 << 3;
const HALF_CYCLE: u32 = 128;

pub struct Neorv32SdBus {
    output: u32,
}

impl Neorv32SdBus {
    pub fn new() -> Self {
        let bus = Self { output: GPIO_MOSI };
        // Drive the deselected, idle SPI state before SD initialization's
        // first delay, rather than leaving GPIO state undefined during it.
        bus.write();
        bus
    }

    fn write(&self) {
        // Safety: NEORV32 GPIO output is the word at GPIO_BASE + 4. The fixed
        // address is valid only for the RV32 NEORV32 target where this module
        // is compiled.
        unsafe { core::ptr::write_volatile((GPIO_BASE + 4) as *mut u32, self.output) };
    }
}

impl Bus for Neorv32SdBus {
    fn select(&mut self, active: bool) -> Result<(), SdError> {
        self.output &= !GPIO_SCK;
        self.output |= GPIO_MOSI;
        if active {
            self.output |= GPIO_SELECT;
        } else {
            self.output &= !GPIO_SELECT;
        }
        self.write();
        Ok(())
    }

    fn transfer(&mut self, tx: u8) -> Result<u8, SdError> {
        let mut rx = 0u8;
        for bit in (0..8).rev() {
            self.output &= !GPIO_SCK;
            if tx & (1 << bit) != 0 {
                self.output |= GPIO_MOSI;
            } else {
                self.output &= !GPIO_MOSI;
            }
            self.write();
            delay_cycles(HALF_CYCLE);
            self.output |= GPIO_SCK;
            self.write();
            delay_cycles(HALF_CYCLE);
            // Safety: GPIO_BASE is the NEORV32 GPIO input register on RV32.
            let input = unsafe { core::ptr::read_volatile(GPIO_BASE as *const u32) };
            rx = (rx << 1) | ((input & GPIO_MISO != 0) as u8);
        }
        self.output &= !GPIO_SCK;
        self.write();
        Ok(rx)
    }

    fn delay_ms(&mut self, milliseconds: u32) {
        for _ in 0..milliseconds {
            delay_cycles(CYCLES_PER_MILLISECOND);
        }
    }
}

fn cycles() -> u32 {
    let value;
    // Safety: `rdcycle` is available in the NEORV32 RV32IM execution mode and
    // does not access memory or the stack.
    unsafe { core::arch::asm!("rdcycle {0}", out(reg) value, options(nomem, nostack)) };
    value
}

fn delay_cycles(count: u32) {
    let start = cycles();
    while cycles().wrapping_sub(start) < count {
        core::hint::spin_loop();
    }
}
