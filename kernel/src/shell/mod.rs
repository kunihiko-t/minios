pub mod command;
pub mod line;

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
use command::{Command, parse_command};
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
use line::{LineBuffer, LineError};
#[cfg(target_arch = "riscv64")]
use minios_kernel::memory::frame::FrameSource;

#[cfg(target_arch = "riscv64")]
const INPUT_CAPACITY: usize = 128;

#[cfg(target_arch = "riscv32")]
const INPUT_CAPACITY: usize = 128;

#[cfg(target_arch = "riscv32")]
// `linker_neorv32.ld`が必ず定義するIMEM/DMEM境界のC ABIシンボルである。
// `addr_of!`でアドレスだけを得て、外部staticの内容は読み書きしない。
unsafe extern "C" {
    static __data_load: u8;
    static __data_start: u8;
    static __data_end: u8;
    static __kernel_end: u8;
}

// `linker_neorv32.ld`の`MEMORY`宣言と一致させたNEORV32の内蔵メモリー容量である。
#[cfg(target_arch = "riscv32")]
const IMEM_LEN: usize = 32_768;
#[cfg(target_arch = "riscv32")]
const DMEM_LEN: usize = 16_192;
#[cfg(target_arch = "riscv32")]
const DMEM_BASE: usize = 0x8000_0000;

#[cfg(target_arch = "riscv32")]
fn imem_used() -> usize {
    // IMEMの占有末尾は、`.text`に続いて配置される`.data`ロードイメージの終端である。
    let data_len =
        core::ptr::addr_of!(__data_end) as usize - core::ptr::addr_of!(__data_start) as usize;
    core::ptr::addr_of!(__data_load) as usize + data_len
}

#[cfg(target_arch = "riscv32")]
fn dmem_used() -> usize {
    // `__kernel_end`は`.bss`末尾の、カーネルが占有するDMEMの境界である。
    core::ptr::addr_of!(__kernel_end) as usize - DMEM_BASE
}

/// RV32 shellが保持する単一のSD/FAT32 session。初回の`ls`/`cat`でmountし、
/// 以降は作り直さず使い回す。heapもcacheも持たない。
#[cfg(target_arch = "riscv32")]
type Rv32Storage = crate::storage::fat32::Fat32<
    crate::storage::sd::SdCard<crate::drivers::neorv32_sd::Neorv32SdBus>,
>;

/// RV64 shellが保持する単一のvirtio-blk/FAT32 session。初回の`ls`/`cat`で
/// FDTのslotをprobeしてmountし、以降は同じsessionを使い回す。
#[cfg(target_arch = "riscv64")]
pub(crate) type Rv64Storage = crate::storage::fat32::Fat32<
    crate::storage::virtio_blk::VirtioBlk<
        crate::drivers::virtio_mmio::MmioRegs,
        crate::VirtioRegionPage,
    >,
>;

/// RV64の`mount_storage`が返す失敗。device未到達とFAT32側の失敗を分け、
/// どちらもshellを止めない診断messageへ写像する。
#[cfg(target_arch = "riscv64")]
pub(crate) enum Rv64StorageError {
    /// 全slotをprobeしたがblock deviceが見つからなかった。
    NoDevice,
    /// queue/request領域のframeを確保できなかった。
    NoFrames,
    /// block deviceは見つかったが初期化を完了できなかった。
    Init(crate::storage::virtio_blk::VirtioError),
    /// mount以降のFAT32経路の失敗。
    Fat(crate::storage::fat32::FatError<crate::storage::virtio_blk::VirtioError>),
}

#[cfg(target_arch = "riscv64")]
pub fn run(hart_id: usize, frames: &mut dyn FrameSource) -> ! {
    let mut storage: Option<Rv64Storage> = None;
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
                        Ok(input) => execute(input, hart_id, frames, &mut storage),
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
fn execute(
    input: &str,
    hart_id: usize,
    frames: &mut dyn FrameSource,
    storage: &mut Option<Rv64Storage>,
) {
    match parse_command(input) {
        Command::Empty => {}
        Command::Help => {
            crate::println!("help      Show available commands");
            crate::println!("info      Show system information");
            crate::println!("uptime    Show elapsed time");
            crate::println!("memory    Show physical memory statistics");
            crate::println!("ls        List a directory");
            crate::println!("cat       Read a file");
            crate::println!("rm        Remove a file");
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
        Command::Ls(path) => list_dir(storage, frames, path),
        Command::Cat("") => {
            crate::println!("virtio: usage: cat NAME.EXT");
        }
        Command::Cat(name) => read_root_file(storage, name, frames),
        Command::Rm("") => {
            crate::println!("virtio: usage: rm NAME");
        }
        Command::Rm(path) => remove_file(storage, frames, path),
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
            crate::println!("uptime    Show elapsed time");
            crate::println!("memory    Show memory usage");
            crate::println!("echo      Echo text");
            crate::println!("ls        List a directory");
            crate::println!("cat       Read a file");
            crate::println!("clear     Clear the terminal");
            crate::println!("shutdown  Halt the CPU");
        }
        Command::Info => {
            crate::println!("MiniOS 0.1.0 on RV32 (NEORV32)");
            crate::println!("hart id: {hart_id}");
        }
        Command::Uptime => {
            let cycles = crate::arch::riscv32::cycles();
            let millis = cycles / (crate::arch::riscv32::SYSTEM_CLOCK_HZ as u64 / 1_000);
            crate::println!("uptime: {millis} ms");
            crate::println!("cycles: {cycles}");
        }
        Command::Memory => {
            crate::println!("imem: {} / {} bytes", imem_used(), IMEM_LEN);
            crate::println!("dmem: {} / {} bytes", dmem_used(), DMEM_LEN);
        }
        Command::Echo(payload) => {
            crate::println!("{payload}");
        }
        Command::Ls(path) => list_dir(storage, path),
        Command::Cat("") => {
            crate::println!("sd: usage: cat NAME.EXT");
        }
        Command::Cat(name) => read_root_file(storage, name),
        Command::Clear => {
            crate::print!("\x1b[2J\x1b[H");
        }
        Command::Shutdown => {
            crate::println!("shutting down");
            // NEORV32に電源切断はない。有効な割り込みを持たないため、`wfi`で恒久的に停止する。
            loop {
                crate::arch::riscv32::wfi();
            }
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

#[cfg(target_arch = "riscv64")]
fn mount_storage<'a>(
    storage: &'a mut Option<Rv64Storage>,
    frames: &mut dyn FrameSource,
) -> Result<&'a mut Rv64Storage, Rv64StorageError> {
    if storage.is_none() {
        *storage = Some(probe_and_mount(frames)?);
    }
    Ok(storage.as_mut().expect("mounted above"))
}

/// `MachineSpec::virtio_mmio`のslotを順にprobeし、最初に見つかった
/// block deviceをmountする。空slotや他deviceは読み飛ばし、確立に使った
/// deviceだけがsessionを所有する。失敗したprobeのregionはdropでframeを
/// poolへ返す。
#[cfg(target_arch = "riscv64")]
pub(crate) fn probe_and_mount(
    frames: &mut dyn FrameSource,
) -> Result<Rv64Storage, Rv64StorageError> {
    use crate::storage::virtio_blk::{VirtioBlk, VirtioError};

    let spec = crate::machine::spec();
    let mut last_init_error = None;
    for &base in &spec.virtio_mmio[..spec.virtio_mmio_count] {
        // Safety: `base`はFDTが報告したMMIO領域で、`with_device_pages`が
        // S-mode R+Wとしてmap済み。
        let mmio = unsafe { crate::drivers::virtio_mmio::MmioRegs::new(base) };
        let Some(frame) = frames.allocate() else {
            return Err(Rv64StorageError::NoFrames);
        };
        match VirtioBlk::init(mmio, crate::VirtioRegionPage::new(frame)) {
            Ok(blk) => {
                return crate::storage::fat32::Fat32::mount(blk).map_err(Rv64StorageError::Fat);
            }
            Err(
                VirtioError::BadMagic
                | VirtioError::NotBlockDevice(_)
                | VirtioError::UnsupportedVersion(_),
            ) => {}
            Err(error) => last_init_error = Some(error),
        }
    }
    match last_init_error {
        Some(error) => Err(Rv64StorageError::Init(error)),
        None => Err(Rv64StorageError::NoDevice),
    }
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn print_dir_entry(entry: &crate::storage::fat32::DirEntry) {
    if entry.is_directory() {
        crate::println!("<DIR> {}", entry.name());
    } else {
        crate::println!("{:>10} {}", entry.size(), entry.name());
    }
}

/// RV32のSD経路はflatな8.3名前空間だけを扱う。`/`を含む引数は
/// IMEMを使うpath機構を持たず、`InvalidName`として報告する。
#[cfg(target_arch = "riscv32")]
fn list_dir(storage: &mut Option<Rv32Storage>, path: &str) {
    if !path.is_empty() {
        crate::println!("sd: invalid 8.3 name");
        return;
    }
    let session = match mount_storage(storage) {
        Ok(session) => session,
        Err(error) => {
            print_fat_error("sd", error);
            return;
        }
    };
    if let Err(error) = session.for_each_root_entry(print_dir_entry) {
        print_fat_error("sd", error);
    }
}

#[cfg(target_arch = "riscv64")]
fn list_dir(storage: &mut Option<Rv64Storage>, frames: &mut dyn FrameSource, path: &str) {
    let session = match mount_storage(storage, frames) {
        Ok(session) => session,
        Err(error) => {
            print_virtio_error(error);
            return;
        }
    };
    if let Err(error) = session.for_each_entry(path, print_dir_entry) {
        print_fat_error("virtio", error);
    }
}

#[cfg(target_arch = "riscv32")]
fn print_root_file<R: crate::storage::SectorReader>(
    session: &mut crate::storage::fat32::Fat32<R>,
    name: &str,
) -> Result<(), crate::storage::fat32::FatError<R::Error>> {
    print_cat_stream(|write| session.read_root_file(name, write))
}

#[cfg(target_arch = "riscv64")]
fn print_root_file<R: crate::storage::SectorReader>(
    session: &mut crate::storage::fat32::Fat32<R>,
    path: &str,
) -> Result<(), crate::storage::fat32::FatError<R::Error>> {
    print_cat_stream(|write| session.read_file(path, write))
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn print_cat_stream<E>(
    read: impl FnOnce(&mut dyn FnMut(&[u8])) -> Result<(), crate::storage::fat32::FatError<E>>,
) -> Result<(), crate::storage::fat32::FatError<E>> {
    let mut last_byte = None;
    let result = read(&mut |bytes: &[u8]| {
        for &byte in bytes {
            crate::console::write_byte(byte);
            last_byte = Some(byte);
        }
    });
    finish_cat_output(result, last_byte, crate::console::write_byte)
}

#[cfg(target_arch = "riscv32")]
fn read_root_file(storage: &mut Option<Rv32Storage>, name: &str) {
    let session = match mount_storage(storage) {
        Ok(session) => session,
        Err(error) => {
            print_fat_error("sd", error);
            return;
        }
    };
    if let Err(error) = print_root_file(session, name) {
        print_fat_error("sd", error);
    }
}

#[cfg(target_arch = "riscv64")]
fn read_root_file(storage: &mut Option<Rv64Storage>, name: &str, frames: &mut dyn FrameSource) {
    let session = match mount_storage(storage, frames) {
        Ok(session) => session,
        Err(error) => {
            print_virtio_error(error);
            return;
        }
    };
    if let Err(error) = print_root_file(session, name) {
        print_fat_error("virtio", error);
    }
}

/// `rm <path>`を遅延mount済みのsessionへ委譲する。成功時は出力を出さず、
/// 失敗だけを診断messageへ写像する。
#[cfg(target_arch = "riscv64")]
fn remove_file(storage: &mut Option<Rv64Storage>, frames: &mut dyn FrameSource, path: &str) {
    let session = match mount_storage(storage, frames) {
        Ok(session) => session,
        Err(error) => {
            print_virtio_error(error);
            return;
        }
    };
    if let Err(error) = session.unlink_file(path) {
        print_fat_error("virtio", error);
    }
}

#[cfg(any(test, target_arch = "riscv32", target_arch = "riscv64"))]
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

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn print_fat_error<E>(prefix: &str, error: crate::storage::fat32::FatError<E>) {
    match error {
        crate::storage::fat32::FatError::Read(_) => {
            crate::println!("{prefix}: I/O error");
        }
        crate::storage::fat32::FatError::Unsupported
        | crate::storage::fat32::FatError::InvalidFilesystem => {
            crate::println!("{prefix}: unsupported or invalid FAT32");
        }
        crate::storage::fat32::FatError::NotFound => {
            crate::println!("{prefix}: file not found");
        }
        crate::storage::fat32::FatError::IsDirectory => {
            crate::println!("{prefix}: is a directory");
        }
        crate::storage::fat32::FatError::NotDirectory => {
            crate::println!("{prefix}: not a directory");
        }
        crate::storage::fat32::FatError::InvalidName => {
            crate::println!("{prefix}: invalid 8.3 name");
        }
        crate::storage::fat32::FatError::CorruptChain => {
            crate::println!("{prefix}: corrupt cluster chain");
        }
        crate::storage::fat32::FatError::NoSpace => {
            crate::println!("{prefix}: no space left");
        }
        crate::storage::fat32::FatError::InvalidOffset => {
            crate::println!("{prefix}: invalid offset");
        }
    }
}

#[cfg(target_arch = "riscv64")]
fn print_virtio_error(error: Rv64StorageError) {
    match error {
        Rv64StorageError::NoDevice => {
            crate::println!("virtio: no block device found");
        }
        Rv64StorageError::NoFrames => {
            crate::println!("virtio: out of frames");
        }
        Rv64StorageError::Init(error) => {
            crate::println!("virtio: device init failed: {error:?}");
        }
        Rv64StorageError::Fat(error) => print_fat_error("virtio", error),
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
