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

#[cfg(target_arch = "riscv32")]
type Rv32Storage = crate::storage::fat32::Fat32<
    crate::storage::sd::SdCard<crate::drivers::neorv32_sd::Neorv32SdBus>,
>;

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
    let mut storage: Option<Rv32Storage> = None;

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
                        Ok(input) => execute32(input, hart_id, &mut storage),
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
                        Ok(input) => execute32(input, hart_id, &mut storage),
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
fn execute32(input: &str, hart_id: usize, storage: &mut Option<Rv32Storage>) {
    match parse_command(input) {
        Command::Empty => {}
        Command::Help => {
            crate::println!("help      Show available commands");
            crate::println!("info      Show system information");
            crate::println!("echo      Echo text");
            crate::println!("ls        List root directory");
            crate::println!("cat       Read a root file");
        }
        Command::Info => {
            crate::println!("MiniOS 0.1.0 on RV32 (NEORV32)");
            crate::println!("hart id: {hart_id}");
        }
        Command::Echo(payload) => {
            crate::println!("{payload}");
        }
        Command::Ls => list_root(storage),
        Command::Cat("") => {
            crate::println!("sd: usage: cat NAME.EXT");
        }
        Command::Cat(name) => read_root_file(storage, name),
        Command::Uptime | Command::Memory | Command::Clear | Command::Shutdown => {
            crate::println!("command unavailable on RV32");
        }
        Command::Unknown(input) => {
            crate::println!("unknown command: {input}; try 'help'");
        }
    }
}

#[cfg(target_arch = "riscv32")]
fn mount_storage(
    storage: &mut Option<Rv32Storage>,
) -> Result<&mut Rv32Storage, crate::storage::fat32::FatError<crate::storage::sd::SdError>> {
    if storage.is_none() {
        let card =
            crate::storage::sd::SdCard::init(crate::drivers::neorv32_sd::Neorv32SdBus::new())
                .map_err(crate::storage::fat32::FatError::Read)?;
        *storage = Some(crate::storage::fat32::Fat32::mount(card)?);
    }
    storage
        .as_mut()
        .ok_or(crate::storage::fat32::FatError::InvalidFilesystem)
}

#[cfg(target_arch = "riscv32")]
fn list_root(storage: &mut Option<Rv32Storage>) {
    let session = match mount_storage(storage) {
        Ok(session) => session,
        Err(error) => {
            print_storage_error(error);
            return;
        }
    };
    let result = session.for_each_root_entry(|entry| {
        if entry.is_directory() {
            crate::println!("<DIR> {}", entry.name());
        } else {
            crate::println!("{:>10} {}", entry.size(), entry.name());
        }
    });
    if let Err(error) = result {
        print_storage_error(error);
    }
}

#[cfg(target_arch = "riscv32")]
fn read_root_file(storage: &mut Option<Rv32Storage>, name: &str) {
    let session = match mount_storage(storage) {
        Ok(session) => session,
        Err(error) => {
            print_storage_error(error);
            return;
        }
    };
    let mut last_byte = None;
    let result = session.read_root_file(name, |bytes| {
        for &byte in bytes {
            crate::console::write_byte(byte);
            last_byte = Some(byte);
        }
    });
    match finish_cat_output(result, last_byte, crate::console::write_byte) {
        Ok(()) => {}
        Err(error) => print_storage_error(error),
    }
}

#[cfg(any(test, target_arch = "riscv32"))]
fn finish_cat_output<E>(
    result: Result<(), E>,
    last_byte: Option<u8>,
    mut write: impl FnMut(u8),
) -> Result<(), E> {
    if let Some(byte) = last_byte
        && byte != b'\n'
    {
        write(b'\n');
    }
    result
}

#[cfg(target_arch = "riscv32")]
fn print_storage_error(error: crate::storage::fat32::FatError<crate::storage::sd::SdError>) {
    match error {
        crate::storage::fat32::FatError::Read(_) => {
            crate::println!("sd: I/O error");
        }
        crate::storage::fat32::FatError::Unsupported
        | crate::storage::fat32::FatError::InvalidFilesystem => {
            crate::println!("sd: unsupported or invalid FAT32");
        }
        crate::storage::fat32::FatError::NotFound => {
            crate::println!("sd: file not found");
        }
        crate::storage::fat32::FatError::IsDirectory => {
            crate::println!("sd: is a directory");
        }
        crate::storage::fat32::FatError::InvalidName => {
            crate::println!("sd: invalid 8.3 name");
        }
        crate::storage::fat32::FatError::CorruptChain => {
            crate::println!("sd: corrupt cluster chain");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::finish_cat_output;

    #[test]
    fn cat_adds_a_newline_after_partial_output_before_an_error() {
        let mut output = [0; 1];
        let mut length = 0;
        let result = finish_cat_output(Err(()), Some(b'x'), |byte| {
            output[length] = byte;
            length += 1;
        });
        assert!(result.is_err());
        assert_eq!(&output[..length], b"\n");
    }

    #[test]
    fn cat_does_not_duplicate_a_final_newline() {
        let mut called = false;
        let result = finish_cat_output(Ok::<(), ()>(()), Some(b'\n'), |_| called = true);
        assert!(result.is_ok());
        assert!(!called);
    }
}
