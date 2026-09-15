//! OpenSBIが`a1`へ渡すDTBからmachine記述を発見し、起動中ずっと共有する。
//! RV64のQEMU経路だけが使う。RV32 (NEORV32) の実機経路にはDTBが存在しない。

use minios_kernel::fdt::{FDT_RESERVED_LEN, FdtError, MachineSpec};

/// headerだけで`totalsize`を確かめるために先読みするFDT先頭の長さ。
const FDT_HEADER_LEN: usize = 40;

// Safety: `kernel_main`が`discover`で一度だけ書き込み、その後は不変である。
// 読み手は単一の起動ハートだけなので、書き込み前の読み出しは既定値
// (QEMU `virt` -m 128Mの参照値) を返す。FDT解析に失敗した場合の緊急診断が
// UARTの既定ベースを必要とするため、既定値は正当な値として残す。
static mut MACHINE: MachineSpec = MachineSpec {
    ram: 0x8000_0000..0x8800_0000,
    uart_base: 0x1000_0000,
    timebase_hz: 10_000_000,
    virtio_mmio: [0; minios_kernel::fdt::VIRTIO_MMIO_MAX],
    virtio_mmio_count: 0,
};

/// `kernel_main`が一度だけ呼ぶ。OpenSBIの`a1`が指すDTBを解析し、
/// machine記述を共有staticとUART driverへ登録する。
///
/// 失敗した場合は`Err`を返し、呼び出し側がpanic経路で停止する。
/// その場合もstaticの既定値はQEMU `virt`を指すため、緊急UART出力は動く。
///
/// # Safety
///
/// `dtb`はbare mode (VA==PA) で読める物理addressでなければならない。
/// QEMU `virt`の仕様どおり、FDTはRAM最後の2 MiB予約領域内にあることを
/// 確認する。そこに無いmachineでは、payloadがFDTを上書きし得るため拒否する。
pub fn discover(dtb: usize) -> Result<&'static MachineSpec, FdtError> {
    if dtb == 0 || !dtb.is_multiple_of(8) {
        return Err(FdtError::BadStructure);
    }
    // Safety: `dtb`はOpenSBIが渡す有効なDRAM addressであり、bare modeでは
    // 物理addressとして読める。先頭40 byteだけでtotalsizeを確かめる。
    let header = unsafe { core::slice::from_raw_parts(dtb as *const u8, FDT_HEADER_LEN) };
    let total_len = declared_len(header)?;
    if total_len > FDT_RESERVED_LEN {
        return Err(FdtError::Truncated);
    }
    // Safety: `totalsize`はheaderが保証したFDT全体の長さであり、
    // QEMUの配置契約によりFDTは連続したDRAM内にある。
    let bytes = unsafe { core::slice::from_raw_parts(dtb as *const u8, total_len) };
    let spec = MachineSpec::from_dtb(bytes)?;
    if !spec.fdt_region().contains(&dtb) {
        return Err(FdtError::UnsupportedMachine);
    }
    crate::drivers::uart::Uart::set_base(spec.uart_base);
    // Safety: この関数は`kernel_main`から一度だけ呼ばれ、以後の書き込みはない。
    unsafe { MACHINE = spec }
    // Safety: 書き込み済みのstaticを不変参照として返す。以後の変更はない。
    Ok(unsafe { &*core::ptr::addr_of!(MACHINE) })
}

/// 発見済みのmachine記述を返す。`discover`前はQEMU `virt`の参照値である。
/// 起動経路は`discover`の戻り値を直接使い、shellのvirtio-blk probeも
/// この記述からMMIO slotを得る。
pub fn spec() -> &'static MachineSpec {
    // Safety: `discover`は書き込み後にMACHINEを変更しない。単一ハートのみ読む。
    unsafe { &*core::ptr::addr_of!(MACHINE) }
}

/// FDT headerが宣言するblob全体の長さを返す。
fn declared_len(header: &[u8]) -> Result<usize, FdtError> {
    if header.len() < FDT_HEADER_LEN || header[0..4] != 0xd00d_feed_u32.to_be_bytes() {
        return Err(FdtError::BadMagic);
    }
    Ok(u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize)
}
