use super::{DirEntry, FatError, Geometry, ParseError};

impl ParseError {
    pub(super) fn into_fat_error<E>(self) -> FatError<E> {
        match self {
            Self::Unsupported => FatError::Unsupported,
            Self::InvalidFilesystem => FatError::InvalidFilesystem,
        }
    }
}

pub(super) fn uppercase_ascii(byte: u8) -> u8 {
    if byte.is_ascii_lowercase() {
        byte - b'a' + b'A'
    } else {
        byte
    }
}

pub(super) fn normalize_input_name(name: &str) -> Option<[u8; 11]> {
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

pub(super) fn parse_dir_entry(record: &[u8]) -> Option<DirEntry> {
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
    for (index, &byte) in record[..8].iter().enumerate() {
        if matches!(byte, b'.' | b'/' | b'\\')
            || (byte == b' ' && index < base_len)
            || !(0x21..=0x7e).contains(&byte) && byte != b' '
        {
            return None;
        }
    }
    for (index, &byte) in record[8..11].iter().enumerate() {
        if matches!(byte, b'.' | b'/' | b'\\')
            || (byte == b' ' && index < extension_len)
            || !(0x21..=0x7e).contains(&byte) && byte != b' '
        {
            return None;
        }
    }

    let mut name = [0; 255];
    let mut name_len = 0;
    for &byte in &record[..base_len] {
        name[name_len] = uppercase_ascii(byte);
        name_len += 1;
    }
    if extension_len != 0 {
        name[name_len] = b'.';
        name_len += 1;
        for &byte in &record[8..8 + extension_len] {
            name[name_len] = uppercase_ascii(byte);
            name_len += 1;
        }
    }

    let high = u16::from_le_bytes([record[20], record[21]]) as u32;
    let low = u16::from_le_bytes([record[26], record[27]]) as u32;
    Some(DirEntry {
        name,
        name_len: name_len as u8,
        short_name: record[..11].try_into().expect("11-byte FAT name field"),
        first_cluster: (high << 16) | low,
        size: u32::from_le_bytes([record[28], record[29], record[30], record[31]]),
        directory: attributes & 0x10 != 0,
    })
}

/// path要素がentryへ一致するか。LFN表示名のASCII大小文字無視比較と、
/// 8.3 aliasへの正規化比較の両方を試す。
pub(super) fn matches_component(entry: &DirEntry, part: &str) -> bool {
    part.eq_ignore_ascii_case(entry.name())
        || normalize_input_name(part).is_some_and(|wanted| wanted == entry.short_name)
}

/// path要素として受理する文字種。LFNは空白を含む印字可能ASCIIを許すが、
/// 区切りと衝突する`/`と`\`、`.`と`..`そのものは拒否する。
#[cfg(not(target_arch = "riscv32"))]
pub(super) fn is_valid_component(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part.len() <= 255
        && part
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'\\')
}

/// raw 8.3名に対するLFN checksum。LFN record列がどのshort entryへ
/// 属するかを照合するために使う。
#[cfg(not(target_arch = "riscv32"))]
pub(super) fn lfn_checksum(short_name: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &byte in short_name {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte);
    }
    sum
}

/// walk中に集めたLFN chunk列。RV32ではZSTのno-opとして実装し、
/// IMEMへ機構を載せない。
#[cfg(not(target_arch = "riscv32"))]
pub(super) struct PendingLfn {
    /// chunk番号順にdecodeしたASCII byte。chunk `s`は`(s-1)*13`から始まる。
    name: [u8; 255],
    /// 連続性を検査する次に来るべきseq。0は未収集または中断。
    expected_seq: u8,
    /// 最終chunk（disk上では先頭）が持つchunk総数。
    chunk_count: u8,
    checksum: u8,
    /// seq 1まで揃ったか。
    complete: bool,
    /// 非ASCIIやbuf超過など、decode不能にした印。
    invalid: bool,
}

#[cfg(not(target_arch = "riscv32"))]
impl PendingLfn {
    pub(super) const fn new() -> Self {
        Self {
            name: [0; 255],
            expected_seq: 0,
            chunk_count: 0,
            checksum: 0,
            complete: false,
            invalid: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.expected_seq = 0;
        self.complete = false;
    }

    /// 1 record分のLFN chunkをdecodeして蓄える。`0x40`flag付きは
    /// 新しい列の開始として状態を張り直す。
    pub(super) fn push(&mut self, record: &[u8]) {
        const CHAR_OFFSETS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        let seq = record[0] & 0x1f;
        if record[0] & 0x40 != 0 {
            self.reset();
            self.chunk_count = seq;
            self.checksum = record[13];
            self.expected_seq = seq;
            self.invalid = seq == 0 || seq > 20;
        }
        if self.invalid || seq == 0 || seq != self.expected_seq {
            self.reset();
            return;
        }
        let start = (seq as usize - 1) * 13;
        if start + 13 > self.name.len() {
            self.invalid = true;
            self.reset();
            return;
        }
        for (index, &offset) in CHAR_OFFSETS.iter().enumerate() {
            let ch = u16::from_le_bytes([record[offset], record[offset + 1]]);
            let decoded = match ch {
                0x0000 | 0xffff => 0,
                0x20..=0x7e => ch as u8,
                _ => {
                    self.invalid = true;
                    0
                }
            };
            self.name[start + index] = decoded;
        }
        self.expected_seq = seq - 1;
        if seq == 1 {
            self.complete = true;
        }
    }

    /// `record`が引き受けるLFN名を`entry`へ適用する。checksumまたは
    /// 収集条件が合わなければ8.3名のままにし、どちらでも状態をresetする。
    pub(super) fn apply(&mut self, entry: &mut DirEntry, record: &[u8]) {
        if self.complete
            && !self.invalid
            && self.checksum == lfn_checksum(record[..11].try_into().unwrap())
        {
            let end = self.chunk_count as usize * 13;
            let name_len = self.name[..end]
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(end);
            entry.name[..name_len].copy_from_slice(&self.name[..name_len]);
            entry.name_len = name_len as u8;
        }
        self.reset();
    }
}

/// RV32では状態を持たず、全ての操作をinlinedなno-opへ畳む。
#[cfg(target_arch = "riscv32")]
pub(super) struct PendingLfn;

#[cfg(target_arch = "riscv32")]
impl PendingLfn {
    pub(super) const fn new() -> Self {
        Self
    }

    pub(super) fn reset(&mut self) {}

    pub(super) fn push(&mut self, _record: &[u8]) {}

    pub(super) fn apply(&mut self, _entry: &mut DirEntry, _record: &[u8]) {}
}

pub(super) fn is_extended_partition(partition_type: u8) -> bool {
    matches!(partition_type, 0x05 | 0x0f | 0x85)
}

pub(super) fn read_u16(sector: &[u8; 512], offset: usize) -> u16 {
    u16::from_le_bytes([sector[offset], sector[offset + 1]])
}

#[cfg(not(target_arch = "riscv32"))]
pub(super) fn write_u16(sector: &mut [u8], offset: usize, value: u16) {
    sector[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[cfg(not(target_arch = "riscv32"))]
pub(super) fn write_u32(sector: &mut [u8], offset: usize, value: u32) {
    sector[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// `name`をFATのraw 8.3名へ正規化する。小文字ASCIIは大文字化し、
/// base 1..=8文字・ext 0..=3文字の`BASE.EXT`形だけを受理する。
/// 変換できない名前（空白・`.`重複・長すぎる要素など）は`None`。
#[cfg(not(target_arch = "riscv32"))]
pub(super) fn to_short_name(name: &str) -> Option<[u8; 11]> {
    pub(super) fn is_83_byte(byte: u8) -> bool {
        byte.is_ascii_uppercase()
            || byte.is_ascii_digit()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'-'
                    | b'@'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'{'
                    | b'}'
                    | b'~'
            )
    }

    let (base, ext) = match name.split_once('.') {
        Some((base, ext)) => (base, ext),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || ext.contains('.') {
        return None;
    }
    let mut short = [b' '; 11];
    for (index, byte) in base.bytes().enumerate() {
        let byte = byte.to_ascii_uppercase();
        if !is_83_byte(byte) {
            return None;
        }
        short[index] = byte;
    }
    for (index, byte) in ext.bytes().enumerate() {
        let byte = byte.to_ascii_uppercase();
        if !is_83_byte(byte) {
            return None;
        }
        short[8 + index] = byte;
    }
    Some(short)
}

pub(super) fn read_u32(sector: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([
        sector[offset],
        sector[offset + 1],
        sector[offset + 2],
        sector[offset + 3],
    ])
}

pub(super) fn parse_boot_sector(
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
    if let Some(partition_sectors) = partition_sectors
        && volume_sectors > partition_sectors
    {
        return Err(ParseError::InvalidFilesystem);
    }

    let fat_sectors = read_u32(sector, 36);
    if fat_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    let root_cluster = read_u32(sector, 44);
    if root_cluster < 2 {
        return Err(ParseError::InvalidFilesystem);
    }

    // FSInfo欄はadvisory。0または予約領域外を指す場合は「無し」(0)として
    // mountを拒否はしない。
    #[cfg(not(target_arch = "riscv32"))]
    let fsinfo_lba = {
        let fsinfo_sector = read_u16(sector, 48) as u32;
        if fsinfo_sector != 0 && fsinfo_sector < reserved_sectors {
            partition_start.checked_add(fsinfo_sector).unwrap_or(0)
        } else {
            0
        }
    };

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
        #[cfg(not(target_arch = "riscv32"))]
        fat_count: fat_count as u32,
        #[cfg(not(target_arch = "riscv32"))]
        fsinfo_lba,
        data_start,
        sectors_per_cluster,
        root_cluster,
        data_cluster_count,
    })
}
