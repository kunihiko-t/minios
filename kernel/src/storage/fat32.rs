use super::SectorReader;

#[derive(Debug, PartialEq, Eq)]
pub enum FatError<E> {
    Read(E),
    Unsupported,
    InvalidFilesystem,
    NotFound,
    IsDirectory,
    InvalidName,
    CorruptChain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    name: [u8; 12],
    name_len: u8,
    first_cluster: u32,
    size: u32,
    directory: bool,
}

impl DirEntry {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).expect("validated FAT name")
    }

    pub const fn size(&self) -> u32 {
        self.size
    }

    pub const fn is_directory(&self) -> bool {
        self.directory
    }
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

    pub fn for_each_root_entry(
        &mut self,
        mut visit: impl FnMut(&DirEntry),
    ) -> Result<(), FatError<R::Error>> {
        let mut scratch = [0; 512];
        self.walk_root(&mut scratch, |entry| {
            visit(entry);
            false
        })
    }

    pub fn read_root_file(
        &mut self,
        name: &str,
        mut write: impl FnMut(&[u8]),
    ) -> Result<(), FatError<R::Error>> {
        let wanted = normalize_input_name(name).ok_or(FatError::InvalidName)?;
        let mut scratch = [0; 512];
        let entry = self
            .find_root_entry(&wanted, &mut scratch)?
            .ok_or(FatError::NotFound)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        if entry.size == 0 {
            return Ok(());
        }
        if !self.is_data_cluster(entry.first_cluster) {
            return Err(FatError::CorruptChain);
        }

        let mut remaining = entry.size;
        let mut cluster = entry.first_cluster;
        let mut clusters_read = 0;
        while remaining != 0 {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }

            let mut sector_in_cluster = 0;
            while sector_in_cluster < self.sectors_per_cluster && remaining != 0 {
                let lba = self.data_sector_lba(cluster, sector_in_cluster as u32)?;
                self.reader
                    .read_sector(lba, &mut scratch)
                    .map_err(FatError::Read)?;
                let bytes = if remaining < 512 {
                    remaining as usize
                } else {
                    512
                };
                write(&scratch[..bytes]);
                remaining -= bytes as u32;
                sector_in_cluster += 1;
            }
            if remaining == 0 {
                return Ok(());
            }

            clusters_read += 1;
            cluster = match self.next_cluster(cluster, &mut scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
        }
        Ok(())
    }

    fn find_root_entry(
        &mut self,
        wanted: &[u8; 11],
        scratch: &mut [u8; 512],
    ) -> Result<Option<DirEntry>, FatError<R::Error>> {
        let mut found = None;
        self.walk_root(scratch, |entry| {
            if entry_short_name(entry) == *wanted {
                found = Some(*entry);
                true
            } else {
                false
            }
        })?;
        Ok(found)
    }

    fn walk_root<F>(
        &mut self,
        scratch: &mut [u8; 512],
        mut visit: F,
    ) -> Result<(), FatError<R::Error>>
    where
        F: FnMut(&DirEntry) -> bool,
    {
        let mut cluster = self.root_cluster;
        let mut clusters_read = 0;
        loop {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }

            let mut sector_in_cluster = 0;
            while sector_in_cluster < self.sectors_per_cluster {
                let lba = self.data_sector_lba(cluster, sector_in_cluster as u32)?;
                self.reader
                    .read_sector(lba, scratch)
                    .map_err(FatError::Read)?;
                for record in 0..16 {
                    let offset = record * 32;
                    if scratch[offset] == 0 {
                        return Ok(());
                    }
                    if let Some(entry) = parse_dir_entry(&scratch[offset..offset + 32]) {
                        if visit(&entry) {
                            return Ok(());
                        }
                    }
                }
                sector_in_cluster += 1;
            }

            clusters_read += 1;
            cluster = match self.next_cluster(cluster, scratch)? {
                Some(next) => next,
                None => return Ok(()),
            };
        }
    }

    fn is_data_cluster(&self, cluster: u32) -> bool {
        match self.data_cluster_count.checked_add(2) {
            Some(limit) => cluster >= 2 && cluster < limit,
            None => false,
        }
    }

    fn data_sector_lba(
        &self,
        cluster: u32,
        sector_in_cluster: u32,
    ) -> Result<u32, FatError<R::Error>> {
        if !self.is_data_cluster(cluster) || sector_in_cluster >= self.sectors_per_cluster as u32 {
            return Err(FatError::CorruptChain);
        }
        let cluster_offset = cluster
            .checked_sub(2)
            .and_then(|value| value.checked_mul(self.sectors_per_cluster as u32))
            .and_then(|value| value.checked_add(sector_in_cluster))
            .ok_or(FatError::InvalidFilesystem)?;
        let lba = self
            .data_start
            .checked_add(cluster_offset)
            .ok_or(FatError::InvalidFilesystem)?;
        let volume_end = self
            .partition_start
            .checked_add(self.volume_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        if lba >= volume_end {
            return Err(FatError::InvalidFilesystem);
        }
        Ok(lba)
    }

    fn next_cluster(
        &mut self,
        cluster: u32,
        scratch: &mut [u8; 512],
    ) -> Result<Option<u32>, FatError<R::Error>> {
        let entry_offset = cluster.checked_mul(4).ok_or(FatError::CorruptChain)?;
        let fat_sector_offset = entry_offset / 512;
        let byte_offset = (entry_offset % 512) as usize;
        let lba = self
            .fat_start
            .checked_add(fat_sector_offset)
            .ok_or(FatError::InvalidFilesystem)?;
        let fat_end = self
            .fat_start
            .checked_add(self.fat_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        let volume_end = self
            .partition_start
            .checked_add(self.volume_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        if lba >= fat_end || lba >= volume_end {
            return Err(FatError::InvalidFilesystem);
        }
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        let value = read_u32(scratch, byte_offset) & 0x0fff_ffff;
        if value >= 0x0fff_fff8 {
            return Ok(None);
        }
        if value < 2 || (0x0fff_fff0..0x0fff_fff8).contains(&value) {
            return Err(FatError::CorruptChain);
        }
        if !self.is_data_cluster(value) {
            return Err(FatError::CorruptChain);
        }
        Ok(Some(value))
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

fn uppercase_ascii(byte: u8) -> u8 {
    if byte.is_ascii_lowercase() {
        byte - b'a' + b'A'
    } else {
        byte
    }
}

fn normalize_input_name(name: &str) -> Option<[u8; 11]> {
    let bytes = name.as_bytes();
    let mut dot = None;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'.' {
            if dot.is_some() {
                return None;
            }
            dot = Some(index);
        } else if !(0x21..=0x7e).contains(&byte) || byte == b'/' || byte == b'\\' {
            return None;
        }
    }

    let base_len = dot.unwrap_or(bytes.len());
    let extension_len = dot.map_or(0, |index| bytes.len() - index - 1);
    if base_len == 0 || base_len > 8 || extension_len > 3 {
        return None;
    }

    let mut normalized = [b' '; 11];
    for index in 0..base_len {
        normalized[index] = uppercase_ascii(bytes[index]);
    }
    if let Some(dot) = dot {
        for index in 0..extension_len {
            normalized[8 + index] = uppercase_ascii(bytes[dot + 1 + index]);
        }
    }
    Some(normalized)
}

fn parse_dir_entry(record: &[u8]) -> Option<DirEntry> {
    if record[0] == 0 || record[0] == 0xe5 {
        return None;
    }
    let attributes = record[11];
    if attributes == 0x0f || attributes & 0x08 != 0 {
        return None;
    }

    let mut base_len = 8;
    while base_len > 0 && record[base_len - 1] == b' ' {
        base_len -= 1;
    }
    let mut extension_len = 3;
    while extension_len > 0 && record[8 + extension_len - 1] == b' ' {
        extension_len -= 1;
    }
    if base_len == 0 {
        return None;
    }
    for index in 0..8 {
        let byte = record[index];
        if (byte == b' ' && index < base_len) || !(0x21..=0x7e).contains(&byte) && byte != b' ' {
            return None;
        }
    }
    for index in 0..3 {
        let byte = record[8 + index];
        if (byte == b' ' && index < extension_len) || !(0x21..=0x7e).contains(&byte) && byte != b' '
        {
            return None;
        }
    }

    let mut name = [0; 12];
    let mut name_len = 0;
    for index in 0..base_len {
        name[name_len] = uppercase_ascii(record[index]);
        name_len += 1;
    }
    if extension_len != 0 {
        name[name_len] = b'.';
        name_len += 1;
        for index in 0..extension_len {
            name[name_len] = uppercase_ascii(record[8 + index]);
            name_len += 1;
        }
    }

    let high = u16::from_le_bytes([record[20], record[21]]) as u32;
    let low = u16::from_le_bytes([record[26], record[27]]) as u32;
    Some(DirEntry {
        name,
        name_len: name_len as u8,
        first_cluster: (high << 16) | low,
        size: u32::from_le_bytes([record[28], record[29], record[30], record[31]]),
        directory: attributes & 0x10 != 0,
    })
}

fn entry_short_name(entry: &DirEntry) -> [u8; 11] {
    let mut normalized = [b' '; 11];
    let mut source = 0;
    let mut destination = 0;
    while source < entry.name_len as usize && entry.name[source] != b'.' {
        normalized[destination] = entry.name[source];
        source += 1;
        destination += 1;
    }
    if source < entry.name_len as usize {
        source += 1;
        destination = 8;
        while source < entry.name_len as usize {
            normalized[destination] = entry.name[source];
            source += 1;
            destination += 1;
        }
    }
    normalized
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

    use super::{DirEntry, Fat32, FatError};
    use crate::storage::SectorReader;
    use std::{string::String, vec, vec::Vec};

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

    struct FixtureReader {
        sectors: Vec<(u32, [u8; 512])>,
    }

    impl SectorReader for FixtureReader {
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

    fn fixture_reader() -> FixtureReader {
        FixtureReader {
            sectors: vec![(0, valid_boot_sector())],
        }
    }

    fn push_sector(reader: &mut FixtureReader, lba: u32, sector: [u8; 512]) {
        reader.sectors.push((lba, sector));
    }

    fn write_fat_entry(sector: &mut [u8; 512], cluster: u32, value: u32) {
        write_u32(sector, (cluster * 4) as usize, value);
    }

    fn write_directory_entry(
        sector: &mut [u8; 512],
        index: usize,
        name: &[u8; 11],
        attributes: u8,
        first_cluster: u32,
        size: u32,
    ) {
        let offset = index * 32;
        sector[offset..offset + 11].copy_from_slice(name);
        sector[offset + 11] = attributes;
        write_u16(sector, offset + 20, (first_cluster >> 16) as u16);
        write_u16(sector, offset + 26, first_cluster as u16);
        write_u32(sector, offset + 28, size);
    }

    fn directory_fat() -> [u8; 512] {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 3);
        write_fat_entry(&mut fat, 3, 0x0fff_ffff);
        fat
    }

    fn mounted_directory_fixture() -> Fat32<FixtureReader> {
        let mut reader = fixture_reader();
        push_sector(&mut reader, 32, directory_fat());

        let mut first_directory_cluster = [0; 512];
        for index in 0..16 {
            first_directory_cluster[index * 32] = 0xe5;
        }
        push_sector(&mut reader, 288, first_directory_cluster);

        let mut second_directory_cluster = [0; 512];
        write_directory_entry(&mut second_directory_cluster, 0, b"HELLO   TXT", 0x20, 4, 0);
        write_directory_entry(&mut second_directory_cluster, 1, b"SUBDIR     ", 0x10, 5, 0);
        push_sector(&mut reader, 289, second_directory_cluster);
        Fat32::mount(reader).unwrap()
    }

    fn mounted_multicluster_file_fixture(content: &[u8]) -> Fat32<FixtureReader> {
        let mut reader = fixture_reader();
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, 5);
        write_fat_entry(&mut fat, 5, 0x0fff_ffff);
        push_sector(&mut reader, 32, fat);

        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, content.len() as u32);
        push_sector(&mut reader, 288, root);

        assert!(content.len() <= 1024);
        let first_len = if content.len() < 512 {
            content.len()
        } else {
            512
        };
        let mut data = [0; 512];
        data[..first_len].copy_from_slice(&content[..first_len]);
        push_sector(&mut reader, 290, data);
        let mut next_data = [0xa5; 512];
        if content.len() > first_len {
            next_data[..content.len() - first_len].copy_from_slice(&content[first_len..]);
        }
        push_sector(&mut reader, 291, next_data);
        Fat32::mount(reader).unwrap()
    }

    fn mounted_empty_file_fixture() -> Fat32<FixtureReader> {
        let mut reader = fixture_reader();
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        push_sector(&mut reader, 32, fat);

        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"EMPTY   TXT", 0x20, 0, 0);
        push_sector(&mut reader, 288, root);
        Fat32::mount(reader).unwrap()
    }

    fn mounted_cyclic_file_fixture() -> Fat32<FixtureReader> {
        let mut reader = fixture_reader();
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, 4);
        push_sector(&mut reader, 32, fat);

        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"LOOP    BIN", 0x20, 4, u32::MAX);
        push_sector(&mut reader, 288, root);
        push_sector(&mut reader, 290, [0x5a; 512]);
        Fat32::mount(reader).unwrap()
    }

    #[test]
    fn root_listing_filters_metadata_and_crosses_a_cluster_boundary() {
        let mut volume = mounted_directory_fixture();
        let mut names = Vec::new();
        volume
            .for_each_root_entry(|entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["HELLO.TXT", "SUBDIR"]);
    }

    #[test]
    fn file_lookup_is_ascii_case_insensitive_and_streams_exact_size() {
        let mut volume = mounted_multicluster_file_fixture(b"hello from sd\n");
        let mut output = Vec::new();
        volume
            .read_root_file("hello.txt", |bytes| output.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(output, b"hello from sd\n");
    }

    #[test]
    fn empty_file_reads_no_data_cluster() {
        let mut volume = mounted_empty_file_fixture();
        let mut called = false;
        volume
            .read_root_file("EMPTY.TXT", |_| called = true)
            .unwrap();
        assert!(!called);
    }

    #[test]
    fn cyclic_chain_is_rejected_before_the_traversal_bound_is_exceeded() {
        let mut volume = mounted_cyclic_file_fixture();
        assert_eq!(
            volume.read_root_file("LOOP.BIN", |_| {}),
            Err(FatError::CorruptChain)
        );
    }

    #[test]
    fn root_listing_stops_at_the_directory_end_marker() {
        let mut volume = mounted_multicluster_file_fixture(b"contents");
        let mut entries = Vec::<DirEntry>::new();
        volume
            .for_each_root_entry(|entry| entries.push(*entry))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), "HELLO.TXT");
    }

    #[test]
    fn file_lookup_rejects_missing_directory_and_invalid_names() {
        let mut volume = mounted_directory_fixture();
        assert_eq!(
            volume.read_root_file("MISSING.TXT", |_| {}),
            Err(FatError::NotFound)
        );
        assert_eq!(
            volume.read_root_file("SUBDIR", |_| {}),
            Err(FatError::IsDirectory)
        );
        for name in [
            "",
            ".TXT",
            "TOO-LONG9.TXT",
            "A/B.TXT",
            "A\\\\B.TXT",
            "A..TXT",
        ] {
            assert_eq!(
                volume.read_root_file(name, |_| {}),
                Err(FatError::InvalidName),
                "{name} unexpectedly accepted"
            );
        }
    }

    #[test]
    fn file_streaming_emits_only_the_declared_final_partial_sector() {
        let mut volume = mounted_multicluster_file_fixture(&[0x11; 513]);
        let mut output = Vec::new();
        volume
            .read_root_file("HELLO.TXT", |bytes| output.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(output.len(), 513);
        assert_eq!(output[512], 0x11);
    }

    #[test]
    fn file_streaming_rejects_free_bad_reserved_and_out_of_range_clusters() {
        for value in [0, 1, 0x0fff_fff0, 0x0fff_fff7, 0x0fff_fff8, 69_714] {
            let mut reader = fixture_reader();
            let mut fat = [0; 512];
            write_fat_entry(&mut fat, 2, 0x0fff_ffff);
            write_fat_entry(&mut fat, 4, value);
            push_sector(&mut reader, 32, fat);
            let mut root = [0; 512];
            write_directory_entry(&mut root, 0, b"BAD     BIN", 0x20, 4, 513);
            push_sector(&mut reader, 288, root);
            push_sector(&mut reader, 290, [0x22; 512]);
            let mut volume = Fat32::mount(reader).unwrap();
            assert_eq!(
                volume.read_root_file("BAD.BIN", |_| {}),
                Err(FatError::CorruptChain),
                "FAT value {value:#x} unexpectedly accepted"
            );
        }
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
