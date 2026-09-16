pub mod fat32;
pub mod sd;
pub mod virtio_blk;

/// 512 byte固定のsector読み取り境界。FAT32 parserをMMIOから切り離し、
/// host testではfixture readerを差し込むための最小抽象である。
pub trait SectorReader {
    type Error;

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), Self::Error>;
}

/// 512 byte固定のsector書き込み境界。`Fat32`のwrite APIは
/// `SectorReader + SectorWriter`の両方を要求し、SDのような
/// read-only backendを型で排除する。
#[cfg(not(target_arch = "riscv32"))]
pub trait SectorWriter: SectorReader {
    fn write_sector(&mut self, lba: u32, source: &[u8; 512]) -> Result<(), Self::Error>;
}
