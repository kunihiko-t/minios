//! 8.3へ正規化できない名前でのentry作成（VFAT long file name）。
//!
//! 8.3へ正規化できる名前はこれまでどおり8.3 entryだけを作る。それ以外の
//! 名前は、同じdirectory内で重複しない8.3 alias（`BASIS~N.EXT`）を
//! 生成し、LFN record列とalias entryをdirectory末尾の連続slotへ書く。
//! LFN record列は名前を13文字ずつUCS-2で持ち、末尾の断片から順に並べ、
//! 各recordがaliasのchecksumを持つ。読み出し側（`PendingLfn`）は
//! このchecksumで列とentryの対応を確かめる。
//!
//! 連続slotが要るため、削除跡（`0xe5`）の再利用はせず、終端slotから
//! 先へ追記する。書き込みの前に必要なslot数をclusterの追加まで含めて
//! 確保し、確保後の書き込みはcluster chainを伸ばさない。renameが
//! sourceを消した後で追記に失敗してentryを失う順序にならないように
//! するためである。

use alloc::vec::Vec;

use crate::storage::{SectorReader, SectorWriter};

use super::parse::{lfn_checksum, to_short_name};
use super::{DirEntry, Fat32, FatError};

/// LFN recordのattribute（read-only・hidden・system・volume labelの合成）。
const LFN_ATTR: u8 = 0x0f;
/// LFN recordの最終断片（列の先頭に置くrecord）を示すsequence flag。
const LFN_LAST: u8 = 0x40;
/// 1 recordが持つ名前の文字数。
const LFN_CHARS: usize = 13;
/// 1 record内でUCS-2文字を置くbyte offset。
const LFN_CHAR_OFFSETS: [usize; LFN_CHARS] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
/// aliasの数字tailの上限。`~1`から`~99`までを試す。
const MAX_ALIAS_TAIL: u32 = 99;

/// `reserve_long_entry`が確保したaliasと、LFN record列を書き始めるslot。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LongSlot {
    alias: [u8; 11],
    start: (u32, u32),
}

/// 新しく作るentryの名前。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NewName<'a> {
    /// 8.3へ正規化できた名前。LFN recordは書かない。
    Short([u8; 11]),
    /// LFNで持つ名前。aliasは`Fat32::lfn_alias`が決める。
    Long(&'a str),
}

impl<'a> NewName<'a> {
    /// path要素`name`の作り方を決める。8.3へ正規化できればshort、
    /// できなければLFNとして受理できるかを確かめる。
    pub(super) fn classify<E>(name: &'a str) -> Result<Self, FatError<E>> {
        if let Some(short) = to_short_name(name) {
            return Ok(Self::Short(short));
        }
        if is_valid_long_name(name) {
            Ok(Self::Long(name))
        } else {
            Err(FatError::InvalidName)
        }
    }

    /// 既存の`entry`が、この名前で作ろうとするentryと衝突するか。
    /// shortは正規化済み8.3名で、longは表示名のASCII大小文字無視で比べる。
    pub(super) fn matches(&self, entry: &DirEntry) -> bool {
        match self {
            Self::Short(short) => entry.short_name == *short,
            Self::Long(name) => entry.name().eq_ignore_ascii_case(name),
        }
    }
}

/// LFNとして作成を受理する名前か。`is_valid_component`済みの要素に対し、
/// FATがLFNに禁じる文字と、末尾の空白・`.`を追加で拒否する。
fn is_valid_long_name(name: &str) -> bool {
    !name.ends_with([' ', '.'])
        && !name
            .bytes()
            .any(|byte| matches!(byte, b'"' | b'*' | b':' | b'<' | b'>' | b'?' | b'|'))
}

/// aliasの基になる`(base, ext)`。空白と先頭の`.`を除いて大文字化し、
/// 8.3で使えない文字を`_`へ置き換える。拡張子は最後の`.`以降の3文字。
fn alias_basis(name: &str) -> ([u8; 8], usize, [u8; 3], usize) {
    let trimmed = name.trim_start_matches('.');
    let (base, ext) = match trimmed.rfind('.') {
        Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
        None => (trimmed, ""),
    };
    // 1文字の名前として8.3へ正規化できる文字だけを残し、それ以外（非ASCIIの
    // 文字を含む）は1文字を`_`1個にする。
    let convert = |ch: char| {
        let mut probe = [0u8; 4];
        let upper = ch.to_ascii_uppercase();
        if upper.is_ascii() && to_short_name(upper.encode_utf8(&mut probe)).is_some() {
            upper as u8
        } else {
            b'_'
        }
    };
    let mut base_out = [b' '; 8];
    let mut base_len = 0;
    for ch in base.chars().filter(|ch| !matches!(ch, ' ' | '.')) {
        if base_len == base_out.len() {
            break;
        }
        base_out[base_len] = convert(ch);
        base_len += 1;
    }
    if base_len == 0 {
        base_out[0] = b'_';
        base_len = 1;
    }
    let mut ext_out = [b' '; 3];
    let mut ext_len = 0;
    for ch in ext.chars().filter(|ch| *ch != ' ') {
        if ext_len == ext_out.len() {
            break;
        }
        ext_out[ext_len] = convert(ch);
        ext_len += 1;
    }
    (base_out, base_len, ext_out, ext_len)
}

/// `name`から数字tail `tail`付きのalias候補を組む。baseはtailが
/// 収まるよう切り詰める（`LONGFI~1`、`LONGF~10`）。
pub(super) fn alias_candidate(name: &str, tail: u32) -> [u8; 11] {
    let (base, base_len, ext, ext_len) = alias_basis(name);
    let mut digits = [0u8; 10];
    let mut digit_len = 0;
    let mut value = tail;
    loop {
        digits[digit_len] = b'0' + (value % 10) as u8;
        digit_len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let keep = base_len.min(8 - 1 - digit_len);
    let mut alias = [b' '; 11];
    alias[..keep].copy_from_slice(&base[..keep]);
    alias[keep] = b'~';
    for (index, digit) in digits[..digit_len].iter().rev().enumerate() {
        alias[keep + 1 + index] = *digit;
    }
    alias[8..8 + ext_len].copy_from_slice(&ext[..ext_len]);
    alias
}

/// `name`のLFN record列をdisk上の並び順（最終断片が先頭）で返す。
/// 各recordは`checksum`でaliasへ結び付く。名前の後ろは0x0000を1文字置き、
/// 残りを0xffffで埋める（13文字ちょうどなら終端文字は置かない）。
pub(super) fn lfn_records(name: &str, checksum: u8) -> Vec<[u8; 32]> {
    // 名前はUCS-2で持つ。`is_valid_component`がBMPの外の文字を拒否するため、
    // UTF-16の1 unitが1文字に対応する。
    let units: Vec<u16> = name.encode_utf16().collect();
    let count = units.len().div_ceil(LFN_CHARS);
    let mut records = Vec::with_capacity(count);
    for seq in (1..=count).rev() {
        let mut record = [0u8; 32];
        record[0] = seq as u8 | if seq == count { LFN_LAST } else { 0 };
        record[11] = LFN_ATTR;
        record[13] = checksum;
        let start = (seq - 1) * LFN_CHARS;
        for (index, &offset) in LFN_CHAR_OFFSETS.iter().enumerate() {
            let position = start + index;
            let ch: u16 = match position.cmp(&units.len()) {
                core::cmp::Ordering::Less => units[position],
                core::cmp::Ordering::Equal => 0x0000,
                core::cmp::Ordering::Greater => 0xffff,
            };
            record[offset..offset + 2].copy_from_slice(&ch.to_le_bytes());
        }
        records.push(record);
    }
    records
}

impl<R: SectorReader + SectorWriter> Fat32<R> {
    /// `dir_cluster`内で既存entryの8.3名と衝突しない`name`のaliasを返す。
    /// `~1`から`~99`まで埋まっていれば`NoSpace`。
    pub(super) fn lfn_alias(
        &mut self,
        dir_cluster: u32,
        name: &str,
        scratch: &mut [u8; 512],
    ) -> Result<[u8; 11], FatError<R::Error>> {
        let mut taken = Vec::new();
        self.walk_dir(dir_cluster, scratch, |entry, _loc| {
            taken.push(entry.short_name);
            false
        })?;
        (1..=MAX_ALIAS_TAIL)
            .map(|tail| alias_candidate(name, tail))
            .find(|alias| !taken.contains(alias))
            .ok_or(FatError::NoSpace)
    }

    /// `dir_cluster`へ`name`をLFNで置くためのaliasとslotを確保する。
    /// 書き込みはまだ行わず、続く`write_long_entry`が確保したslotへ書く。
    /// 間にdirectoryの既存recordを削除跡へ変えてもよいが、chainの末尾は
    /// 変えてはならない。
    pub(super) fn reserve_long_entry(
        &mut self,
        dir_cluster: u32,
        name: &str,
        scratch: &mut [u8; 512],
    ) -> Result<LongSlot, FatError<R::Error>> {
        let alias = self.lfn_alias(dir_cluster, name, scratch)?;
        let count = name.encode_utf16().count().div_ceil(LFN_CHARS) + 1;
        let start = self.reserve_dir_tail(dir_cluster, count, scratch)?;
        Ok(LongSlot { alias, start })
    }

    /// `slot`へ`name`のLFN record列と、`init`で埋めたalias entryを書き、
    /// alias entryの位置`(cluster, index)`を返す。entryの名前byteは
    /// `init`の後でaliasに置き換える。
    pub(super) fn write_long_entry(
        &mut self,
        slot: &LongSlot,
        name: &str,
        scratch: &mut [u8; 512],
        init: impl FnOnce(&mut [u8; 32]),
    ) -> Result<(u32, u32), FatError<R::Error>> {
        let mut records = lfn_records(name, lfn_checksum(&slot.alias));
        let mut entry = [0u8; 32];
        init(&mut entry);
        entry[..11].copy_from_slice(&slot.alias);
        records.push(entry);
        self.write_dir_records(slot.start, &records, scratch)
    }

    /// directoryの終端slotから`count`個のslotが続くよう、足りなければ
    /// chain末尾へzero-fill済みのclusterを繋ぎ、終端slotの位置を返す。
    /// 繋いだclusterは空のdirectory領域として読めるため、後の書き込みが
    /// 失敗しても破損にはならない。
    fn reserve_dir_tail(
        &mut self,
        dir_cluster: u32,
        count: usize,
        scratch: &mut [u8; 512],
    ) -> Result<(u32, u32), FatError<R::Error>> {
        let slots_per_cluster = self.sectors_per_cluster as u32 * 16;
        let mut cluster = dir_cluster;
        let mut clusters_read = 0;
        let mut start = None;
        let mut available = 0usize;
        loop {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }
            if start.is_none() {
                'scan: for sector in 0..self.sectors_per_cluster as u32 {
                    let lba = self.data_sector_lba(cluster, sector)?;
                    self.reader
                        .read_sector(lba, scratch)
                        .map_err(FatError::Read)?;
                    for record in 0..16u32 {
                        if scratch[record as usize * 32] == 0x00 {
                            let index = sector * 16 + record;
                            start = Some((cluster, index));
                            available = (slots_per_cluster - index) as usize;
                            break 'scan;
                        }
                    }
                }
            } else {
                available += slots_per_cluster as usize;
            }
            if available >= count {
                break;
            }
            clusters_read += 1;
            match self.next_cluster(cluster, scratch)? {
                Some(next) => cluster = next,
                None => {
                    let new = self.alloc_cluster(scratch)?;
                    self.set_fat_entry(cluster, new, scratch)?;
                    if start.is_none() {
                        start = Some((new, 0));
                        available = slots_per_cluster as usize;
                    } else {
                        available += slots_per_cluster as usize;
                    }
                    cluster = new;
                    if available >= count {
                        break;
                    }
                }
            }
        }
        Ok(start.expect("a terminator or a new cluster was found"))
    }

    /// `start`から`records`を連続slotへ書き、最後のrecordの位置を返す。
    /// slotは`reserve_dir_tail`が確保済みで、ここではchainを伸ばさない。
    /// 最後のrecordの次slotがsector内なら終端印へzero化し、次のsectorや
    /// 次のclusterへ移るならその領域をzero-fillして終端を保つ。
    fn write_dir_records(
        &mut self,
        start: (u32, u32),
        records: &[[u8; 32]],
        scratch: &mut [u8; 512],
    ) -> Result<(u32, u32), FatError<R::Error>> {
        let slots_per_cluster = self.sectors_per_cluster as u32 * 16;
        let (mut cluster, mut index) = start;
        let mut sector = [0u8; 512];
        let mut loaded = None;
        let mut last = start;
        for record in records {
            if index == slots_per_cluster {
                // 確保済みの次clusterへ移る。
                cluster = self
                    .next_cluster(cluster, scratch)?
                    .ok_or(FatError::CorruptChain)?;
                index = 0;
            }
            let lba = self.data_sector_lba(cluster, index / 16)?;
            if loaded != Some(lba) {
                if let Some(previous) = loaded {
                    self.reader
                        .write_sector(previous, &sector)
                        .map_err(FatError::Read)?;
                }
                self.reader
                    .read_sector(lba, &mut sector)
                    .map_err(FatError::Read)?;
                loaded = Some(lba);
            }
            let offset = (index % 16) as usize * 32;
            sector[offset..offset + 32].copy_from_slice(record);
            last = (cluster, index);
            index += 1;
        }
        // 終端印の後始末。`index`は最後のrecordの次slotを指す。
        if index % 16 != 0 && index < slots_per_cluster {
            let offset = (index % 16) as usize * 32;
            sector[offset..offset + 32].fill(0);
        }
        if let Some(lba) = loaded {
            self.reader
                .write_sector(lba, &sector)
                .map_err(FatError::Read)?;
        }
        if index % 16 == 0 {
            if index < slots_per_cluster {
                let next_lba = self.data_sector_lba(cluster, index / 16)?;
                self.reader
                    .write_sector(next_lba, &[0; 512])
                    .map_err(FatError::Read)?;
            } else if let Some(next) = self.next_cluster(cluster, scratch)? {
                self.zero_cluster(next)?;
            }
        }
        Ok(last)
    }
}
