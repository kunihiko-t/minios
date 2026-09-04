pub mod command;
pub mod line;

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
use command::{Command, parse_command};
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
use line::{LineBuffer, LineError};
#[cfg(target_arch = "riscv64")]
use minios_kernel::memory::frame::FrameAllocator;

#[cfg(target_arch = "riscv64")]
const INPUT_CAPACITY: usize = 128;

#[cfg(target_arch = "riscv32")]
const INPUT_CAPACITY: usize = 128;

#[cfg(target_arch = "riscv64")]
pub fn run(hart_id: usize, frames: &mut FrameAllocator<512>) -> ! {
    let mut line = LineBuffer::<INPUT_CAPACITY>::new();
    loop {
        crate::print!("minios> ");
        line.clear();

        loop {
            let byte = crate::console::read_byte();
            match byte {
                b'\r' | b'\n' => {
                    crate::println!();
                    match line.finish() {
                        Ok(input) => execute(input, hart_id, frames),
                        Err(LineError::Full) => {
                            crate::println!("error: input exceeds 128 bytes");
                        }
                        Err(LineError::NonPrintable) => {}
                    }
                    break;
                }
                0x08 | 0x7f => {
                    if line.backspace().is_some() {
                        crate::print!("\x08 \x08");
                    }
                }
                b' '..=b'~' if line.push(byte).is_ok() => {
                    crate::console::write_byte(byte);
                }
                _ => {}
            }
        }
    }
}

#[cfg(target_arch = "riscv64")]
fn execute(input: &str, hart_id: usize, frames: &mut FrameAllocator<512>) {
    match parse_command(input) {
        Command::Empty => {}
        Command::Help => {
            crate::println!("help      Show available commands");
            crate::println!("info      Show system information");
            crate::println!("uptime    Show elapsed time");
            crate::println!("memory    Show physical memory statistics");
            crate::println!("clear     Clear the terminal");
            crate::println!("shutdown  Shut down MiniOS");
        }
        Command::Info => {
            crate::println!("MiniOS 0.1.0 on RISC-V 64");
            crate::println!("hart id: {hart_id}");
        }
        Command::Uptime => {
            let uptime_millis = crate::time::uptime_millis();
            let ticks = crate::time::ticks();
            crate::println!("uptime: {uptime_millis} ms");
            crate::println!("ticks: {ticks}");
        }
        Command::Memory => {
            let stats = frames.stats();
            crate::println!(
                "memory: total={} allocated={} free={} pages",
                stats.total,
                stats.allocated,
                stats.free
            );
        }
        Command::Clear => {
            crate::print!("\x1b[2J\x1b[H");
        }
        Command::Shutdown => {
            crate::println!("shutting down");
            crate::arch::riscv64::sbi::system_reset(
                crate::arch::riscv64::sbi::ResetType::Shutdown,
                crate::arch::riscv64::sbi::ResetReason::NoReason,
            );
        }
        Command::Unknown(input) => {
            crate::println!("unknown command: {input}; try 'help'");
        }
    }
}

#[cfg(target_arch = "riscv32")]
pub fn run32(hart_id: usize) -> ! {
    let mut line = LineBuffer::<INPUT_CAPACITY>::new();
    let mut suppress_lf = false;

    loop {
        crate::print!("minios> ");
        line.clear();

        loop {
            let byte = crate::console::read_byte();
            if suppress_lf {
                suppress_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    crate::println!();
                    match line.finish() {
                        Ok(input) => execute32(input, hart_id),
                        Err(LineError::Full) => {
                            crate::println!("error: input exceeds 128 bytes");
                        }
                        Err(LineError::NonPrintable) => {}
                    }
                    suppress_lf = true;
                    break;
                }
                b'\n' => {
                    crate::println!();
                    match line.finish() {
                        Ok(input) => execute32(input, hart_id),
                        Err(LineError::Full) => {
                            crate::println!("error: input exceeds 128 bytes");
                        }
                        Err(LineError::NonPrintable) => {}
                    }
                    break;
                }
                0x08 | 0x7f => {
                    if line.backspace().is_some() {
                        crate::print!("\x08 \x08");
                    }
                }
                b' '..=b'~' if line.push(byte).is_ok() => {
                    crate::console::write_byte(byte);
                }
                _ => {}
            }
        }
    }
}

#[cfg(target_arch = "riscv32")]
fn execute32(input: &str, hart_id: usize) {
    match parse_command(input) {
        Command::Empty => {}
        Command::Help => {
            crate::println!("help      Show available commands");
            crate::println!("info      Show system information");
            crate::println!("echo      Echo text");
        }
        Command::Info => {
            crate::println!("MiniOS 0.1.0 on RV32 (NEORV32)");
            crate::println!("hart id: {hart_id}");
        }
        Command::Echo(payload) => {
            crate::println!("{payload}");
        }
        Command::Uptime | Command::Memory | Command::Clear | Command::Shutdown => {
            crate::println!("command unavailable on RV32");
        }
        Command::Unknown(input) => {
            crate::println!("unknown command: {input}; try 'help'");
        }
    }
}
