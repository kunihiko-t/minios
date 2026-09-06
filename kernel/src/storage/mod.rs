pub mod fat32;
pub mod sd;

pub trait SectorReader {
    type Error;

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), Self::Error>;
}
