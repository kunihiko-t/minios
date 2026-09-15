pub mod fat32;
pub mod sd;
pub mod virtio_blk;

/// 512 byte固定のsector読み取り境界。FAT32 parserをMMIOから切り離し、
/// host testではfixture readerを差し込むための最小抽象である。
pub trait SectorReader {
    type Error;

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), Self::Error>;
}
