use super::SectorReader;

#[derive(Debug, PartialEq, Eq)]
pub enum FatError<E> {
    Read(E),
    Unsupported,
    InvalidFilesystem,
}

// Task 3 consumes the retained reader and geometry for FAT traversal.
#[allow(dead_code)]
pub struct Fat32<R> {
    reader: R,
    partition_start: u32,
    volume_sectors: u32,
    fat_start: u32,
    fat_sectors: u32,
    data_start: u32,
    sectors_per_cluster: u8,
    root_cluster: u32,
    data_cluster_count: u32,
}

struct Geometry {
    partition_start: u32,
    volume_sectors: u32,
    fat_start: u32,
    fat_sectors: u32,
    data_start: u32,
    sectors_per_cluster: u8,
    root_cluster: u32,
    data_cluster_count: u32,
}

#[derive(Clone, Copy)]
enum ParseError {
    Unsupported,
    InvalidFilesystem,
}

impl<R: SectorReader> Fat32<R> {
    pub fn mount(mut reader: R) -> Result<Self, FatError<R::Error>> {
        let mut sector = [0; 512];
        reader.read_sector(0, &mut sector).map_err(FatError::Read)?;

        let first_parse = parse_boot_sector(&sector, 0, None);
        if let Ok(geometry) = first_parse {
            return Ok(Self::from_geometry(reader, geometry));
        }
        let first_error = first_parse.err().unwrap();

        if sector[510..512] != [0x55, 0xaa] {
            return Err(first_error.into_fat_error());
        }

        let mut selected = None;
        let mut saw_nonempty = false;
        for index in 0..4 {
            let offset = 446 + 16 * index;
            let partition_type = sector[offset + 4];
            if partition_type == 0 {
                continue;
            }
            saw_nonempty = true;
            if partition_type == 0xee || is_extended_partition(partition_type) {
                return Err(FatError::Unsupported);
            }
            if partition_type != 0x0b && partition_type != 0x0c {
                continue;
            }

            let start = read_u32(&sector, offset + 8);
            let length = read_u32(&sector, offset + 12);
            if start == 0 || length == 0 {
                return Err(FatError::InvalidFilesystem);
            }
            start
                .checked_add(length)
                .ok_or(FatError::InvalidFilesystem)?;
            selected = Some((start, length));
            break;
        }

        let (partition_start, partition_sectors) = match selected {
            Some(selected) => selected,
            None if saw_nonempty => return Err(FatError::Unsupported),
            None => return Err(first_error.into_fat_error()),
        };

        reader
            .read_sector(partition_start, &mut sector)
            .map_err(FatError::Read)?;
        let geometry = parse_boot_sector(&sector, partition_start, Some(partition_sectors))
            .map_err(ParseError::into_fat_error)?;
        Ok(Self::from_geometry(reader, geometry))
    }

    fn from_geometry(reader: R, geometry: Geometry) -> Self {
        Self {
            reader,
            partition_start: geometry.partition_start,
            volume_sectors: geometry.volume_sectors,
            fat_start: geometry.fat_start,
            fat_sectors: geometry.fat_sectors,
            data_start: geometry.data_start,
            sectors_per_cluster: geometry.sectors_per_cluster,
            root_cluster: geometry.root_cluster,
            data_cluster_count: geometry.data_cluster_count,
        }
    }
}

impl ParseError {
    fn into_fat_error<E>(self) -> FatError<E> {
        match self {
            Self::Unsupported => FatError::Unsupported,
            Self::InvalidFilesystem => FatError::InvalidFilesystem,
        }
    }
}

fn is_extended_partition(partition_type: u8) -> bool {
    matches!(partition_type, 0x05 | 0x0f | 0x85)
}

fn read_u16(sector: &[u8; 512], offset: usize) -> u16 {
    u16::from_le_bytes([sector[offset], sector[offset + 1]])
}

fn read_u32(sector: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([
        sector[offset],
        sector[offset + 1],
        sector[offset + 2],
        sector[offset + 3],
    ])
}

fn parse_boot_sector(
    sector: &[u8; 512],
    partition_start: u32,
    partition_sectors: Option<u32>,
) -> Result<Geometry, ParseError> {
    if sector[3..11] == *b"EXFAT   " {
        return Err(ParseError::Unsupported);
    }
    if sector[510..512] != [0x55, 0xaa] {
        return Err(ParseError::InvalidFilesystem);
    }

    let bytes_per_sector = read_u16(sector, 11);
    if bytes_per_sector == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    if bytes_per_sector != 512 {
        return Err(ParseError::Unsupported);
    }

    let sectors_per_cluster = sector[13];
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
        return Err(ParseError::InvalidFilesystem);
    }
    if sectors_per_cluster > 128 {
        return Err(ParseError::Unsupported);
    }

    let reserved_sectors = read_u16(sector, 14) as u32;
    if reserved_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }

    let fat_count = sector[16];
    if fat_count == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    if fat_count > 2 {
        return Err(ParseError::Unsupported);
    }

    let root_entries = read_u16(sector, 17);
    let fat_size_16 = read_u16(sector, 22);
    if root_entries != 0 || fat_size_16 != 0 {
        return Err(ParseError::Unsupported);
    }

    let volume_sectors_16 = read_u16(sector, 19) as u32;
    let volume_sectors_32 = read_u32(sector, 32);
    let volume_sectors = if volume_sectors_16 != 0 {
        volume_sectors_16
    } else {
        volume_sectors_32
    };
    if volume_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    if let Some(partition_sectors) = partition_sectors {
        if volume_sectors > partition_sectors {
            return Err(ParseError::InvalidFilesystem);
        }
    }

    let fat_sectors = read_u32(sector, 36);
    if fat_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    let root_cluster = read_u32(sector, 44);
    if root_cluster < 2 {
        return Err(ParseError::InvalidFilesystem);
    }

    let fat_total_sectors = (fat_count as u32)
        .checked_mul(fat_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_start_relative = reserved_sectors
        .checked_add(fat_total_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let fat_end_relative = reserved_sectors
        .checked_add(fat_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    if fat_end_relative > volume_sectors || data_start_relative > volume_sectors {
        return Err(ParseError::InvalidFilesystem);
    }

    let data_sectors = volume_sectors
        .checked_sub(data_start_relative)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_cluster_count = data_sectors / sectors_per_cluster as u32;
    if data_cluster_count < 65_525 {
        return Err(ParseError::Unsupported);
    }
    let data_span = data_cluster_count
        .checked_mul(sectors_per_cluster as u32)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_end_relative = data_start_relative
        .checked_add(data_span)
        .ok_or(ParseError::InvalidFilesystem)?;
    if data_end_relative > volume_sectors {
        return Err(ParseError::InvalidFilesystem);
    }
    let max_cluster = data_cluster_count
        .checked_add(1)
        .ok_or(ParseError::InvalidFilesystem)?;
    if root_cluster > max_cluster {
        return Err(ParseError::InvalidFilesystem);
    }

    let volume_end = partition_start
        .checked_add(volume_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let fat_start = partition_start
        .checked_add(reserved_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let fat_end = fat_start
        .checked_add(fat_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    if fat_end > volume_end {
        return Err(ParseError::InvalidFilesystem);
    }
    let data_start = partition_start
        .checked_add(data_start_relative)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_end = data_start
        .checked_add(data_span)
        .ok_or(ParseError::InvalidFilesystem)?;
    if data_end > volume_end {
        return Err(ParseError::InvalidFilesystem);
    }

    Ok(Geometry {
        partition_start,
        volume_sectors,
        fat_start,
        fat_sectors,
        data_start,
        sectors_per_cluster,
        root_cluster,
        data_cluster_count,
    })
}

#[cfg(test)]
impl<R> Fat32<R> {
    pub fn partition_start(&self) -> u32 {
        self.partition_start
    }

    pub fn root_cluster(&self) -> u32 {
        self.root_cluster
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{Fat32, FatError};
    use crate::storage::SectorReader;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReadError {
        MissingSector(u32),
    }

    struct MemoryReader<const N: usize> {
        sectors: [(u32, [u8; 512]); N],
    }

    impl<const N: usize> MemoryReader<N> {
        fn with_sectors(sectors: [(u32, [u8; 512]); N]) -> Self {
            Self { sectors }
        }
    }

    impl MemoryReader<1> {
        fn with_sector(lba: u32, sector: [u8; 512]) -> Self {
            Self::with_sectors([(lba, sector)])
        }
    }

    impl MemoryReader<2> {
        fn with_mbr_volume(start: u32, length: u32, boot: [u8; 512]) -> Self {
            let mut mbr = [0; 512];
            write_partition(&mut mbr, 0, 0x0c, start, length);
            mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
            Self::with_sectors([(0, mbr), (start, boot)])
        }
    }

    impl<const N: usize> SectorReader for MemoryReader<N> {
        type Error = ReadError;

        fn read_sector(
            &mut self,
            lba: u32,
            destination: &mut [u8; 512],
        ) -> Result<(), Self::Error> {
            for &(sector_lba, ref sector) in &self.sectors {
                if sector_lba == lba {
                    destination.copy_from_slice(sector);
                    return Ok(());
                }
            }
            Err(ReadError::MissingSector(lba))
        }
    }

    fn write_u16(sector: &mut [u8; 512], offset: usize, value: u16) {
        sector[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(sector: &mut [u8; 512], offset: usize, value: u32) {
        sector[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_partition(
        mbr: &mut [u8; 512],
        index: usize,
        partition_type: u8,
        start: u32,
        length: u32,
    ) {
        let offset = 446 + 16 * index;
        mbr[offset + 4] = partition_type;
        write_u32(mbr, offset + 8, start);
        write_u32(mbr, offset + 12, length);
    }

    fn valid_boot_sector() -> [u8; 512] {
        let mut boot = [0; 512];
        write_u16(&mut boot, 11, 512);
        boot[13] = 1;
        write_u16(&mut boot, 14, 32);
        boot[16] = 2;
        write_u16(&mut boot, 17, 0);
        write_u16(&mut boot, 19, 0);
        write_u16(&mut boot, 22, 0);
        write_u32(&mut boot, 32, 70_000);
        write_u32(&mut boot, 36, 128);
        write_u32(&mut boot, 44, 2);
        boot[510..512].copy_from_slice(&[0x55, 0xaa]);
        boot
    }

    #[test]
    fn mounts_a_valid_fat32_superfloppy_at_lba_zero() {
        let reader = MemoryReader::with_sector(0, valid_boot_sector());
        let volume = Fat32::mount(reader).unwrap();
        assert_eq!(volume.partition_start(), 0);
        assert_eq!(volume.root_cluster(), 2);
    }

    #[test]
    fn scans_primary_mbr_entries_and_mounts_the_first_fat32_partition() {
        let mut mbr = [0; 512];
        write_partition(&mut mbr, 0, 0x83, 1, 10);
        write_partition(&mut mbr, 1, 0x0c, 2048, 70_000);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        let reader = MemoryReader::with_sectors([(0, mbr), (2048, valid_boot_sector())]);
        assert_eq!(Fat32::mount(reader).unwrap().partition_start(), 2048);
    }

    #[test]
    fn mounts_the_first_fat32_partition_without_inspecting_later_entries() {
        let mut mbr = [0; 512];
        write_partition(&mut mbr, 0, 0x0c, 2048, 70_000);
        write_partition(&mut mbr, 1, 0xee, u32::MAX, u32::MAX);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        let reader = MemoryReader::with_sectors([(0, mbr), (2048, valid_boot_sector())]);
        assert_eq!(Fat32::mount(reader).unwrap().partition_start(), 2048);
    }

    #[test]
    fn rejects_a_bpb_whose_data_range_exceeds_the_partition() {
        let reader = MemoryReader::with_mbr_volume(2048, 100, valid_boot_sector());
        assert!(matches!(
            Fat32::mount(reader),
            Err(FatError::InvalidFilesystem)
        ));
    }

    #[test]
    fn rejects_malformed_or_unsupported_bpb_values() {
        let cases = [
            ("bytes per sector", 11, 1024u32),
            ("reserved sectors", 14, 0),
            ("root entries", 17, 1),
            ("fat16 size", 22, 1),
            ("fat32 size", 36, 0),
            ("root cluster", 44, 1),
        ];
        for &(name, offset, value) in &cases {
            let mut boot = valid_boot_sector();
            if offset == 44 || offset == 36 {
                write_u32(&mut boot, offset, value);
            } else {
                write_u16(&mut boot, offset, value as u16);
            }
            assert!(
                Fat32::mount(MemoryReader::with_sector(0, boot)).is_err(),
                "case {name} unexpectedly mounted"
            );
        }

        let mut boot = valid_boot_sector();
        boot[13] = 0;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
        let mut boot = valid_boot_sector();
        boot[13] = 3;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
        let mut boot = valid_boot_sector();
        boot[16] = 0;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
        let mut boot = valid_boot_sector();
        boot[16] = 3;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
    }

    #[test]
    fn rejects_checked_arithmetic_overflow() {
        let mut boot = valid_boot_sector();
        write_u32(&mut boot, 32, u32::MAX);
        write_u32(&mut boot, 36, u32::MAX);
        let reader = MemoryReader::with_mbr_volume(1, u32::MAX, boot);
        assert!(matches!(
            Fat32::mount(reader),
            Err(FatError::InvalidFilesystem)
        ));
    }

    #[test]
    fn rejects_gpt_protective_mbr() {
        let mut mbr = [0; 512];
        write_partition(&mut mbr, 0, 0xee, 1, u32::MAX);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        assert!(matches!(
            Fat32::mount(MemoryReader::with_sector(0, mbr)),
            Err(FatError::Unsupported)
        ));
    }

    #[test]
    fn rejects_missing_boot_signature() {
        let mut boot = valid_boot_sector();
        boot[510..512].copy_from_slice(&[0, 0]);
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
    }

    #[test]
    fn rejects_an_exfat_oem_signature_as_unsupported() {
        let mut boot = [0; 512];
        boot[3..11].copy_from_slice(b"EXFAT   ");
        assert!(matches!(
            Fat32::mount(MemoryReader::with_sector(0, boot)),
            Err(FatError::Unsupported)
        ));
    }
}
