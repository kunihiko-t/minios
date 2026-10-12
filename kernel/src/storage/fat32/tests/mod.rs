extern crate std;

use super::parse::{lfn_checksum, read_u16, read_u32, to_short_name};
use super::{DirEntry, Fat32, FatError};
use crate::storage::{SectorReader, SectorWriter};
use std::{string::String, vec::Vec};

mod dir;
mod read;
mod rename;
mod write;

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

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), Self::Error> {
        for &(sector_lba, ref sector) in &self.sectors {
            if sector_lba == lba {
                destination.copy_from_slice(sector);
                return Ok(());
            }
        }
        Err(ReadError::MissingSector(lba))
    }
}

/// fixtureに無いlbaへのwriteは`MissingSector`で失敗させ、volume外の
/// 書き込みをテストで検出できるようにする。
impl<const N: usize> SectorWriter for MemoryReader<N> {
    fn write_sector(&mut self, lba: u32, source: &[u8; 512]) -> Result<(), Self::Error> {
        for &mut (sector_lba, ref mut sector) in &mut self.sectors {
            if sector_lba == lba {
                sector.copy_from_slice(source);
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

fn write_partition(mbr: &mut [u8; 512], index: usize, partition_type: u8, start: u32, length: u32) {
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

/// write検証用の最小volume。`fat_count=1`、`fat_sectors=1`なので
/// data領域はlba 33から始まり、cluster `n`はlba `31 + n`。
/// fatはentry 0..=127だけを記述し、2(root)/4(HELLO)/5(SUBDIR)を
/// 使用済みとしてマークする。
fn writable_boot_sector() -> [u8; 512] {
    let mut boot = valid_boot_sector();
    write_u16(&mut boot, 14, 32);
    boot[16] = 1;
    write_u32(&mut boot, 36, 1);
    write_u32(&mut boot, 32, 66_000);
    boot
}

fn writable_fat() -> [u8; 512] {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);
    write_fat_entry(&mut fat, 4, 0x0fff_ffff);
    write_fat_entry(&mut fat, 5, 0x0fff_ffff);
    fat
}

/// root: [0]=HELLO.TXT(cluster 4, 11B), [1]=SUBDIR(cluster 5),
/// [2]=終端。cluster 3と6以降はfree。
fn writable_sectors() -> [(u32, [u8; 512]); 10] {
    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, 11);
    write_directory_entry(&mut root, 1, b"SUBDIR     ", 0x10, 5, 0);

    let mut hello = [0; 512];
    hello[..11].copy_from_slice(b"hello world");

    let mut subdir = [0; 512];
    write_directory_entry(&mut subdir, 0, b".          ", 0x10, 5, 0);
    write_directory_entry(&mut subdir, 1, b"..         ", 0x10, 2, 0);

    [
        (0, writable_boot_sector()),
        (32, writable_fat()),
        (33, root),
        (34, [0; 512]),
        (35, hello),
        (36, subdir),
        (37, [0; 512]),
        (38, [0; 512]),
        (39, [0; 512]),
        (40, [0; 512]),
    ]
}

fn mounted_writable_fixture() -> Fat32<MemoryReader<10>> {
    Fat32::mount(MemoryReader::with_sectors(writable_sectors())).unwrap()
}

/// FSInfo sector。`unknown`ならfree/nextを規定の「不明」値、違えば
/// 正しい形だが内容の古い値を持たせる。
fn fsinfo_sector(unknown: bool) -> [u8; 512] {
    let mut sector = [0; 512];
    write_u32(&mut sector, 0, 0x4161_5252);
    write_u32(&mut sector, 484, 0x6141_7272);
    write_u32(&mut sector, 488, if unknown { 0xffff_ffff } else { 61 });
    write_u32(&mut sector, 492, if unknown { 0xffff_ffff } else { 3 });
    write_u32(&mut sector, 508, 0xaa55_0000);
    sector
}

/// BPBがsector 1をFSInfoとして指すwritable fixture。
fn mounted_fsinfo_fixture(fsinfo: [u8; 512]) -> Fat32<MemoryReader<11>> {
    let base = writable_sectors();
    let mut boot = base[0].1;
    write_u16(&mut boot, 48, 1);
    let mut sectors = [(0, [0; 512]); 11];
    sectors[0] = (0, boot);
    sectors[1] = (1, fsinfo);
    sectors[2..].copy_from_slice(&base[1..]);
    Fat32::mount(MemoryReader::with_sectors(sectors)).unwrap()
}

fn sector_of<const N: usize>(fs: &Fat32<MemoryReader<N>>, lba: u32) -> &[u8; 512] {
    &fs.reader
        .sectors
        .iter()
        .find(|(sector_lba, _)| *sector_lba == lba)
        .unwrap()
        .1
}

/// rootに`SUBDIR`（cluster 5）を持ち、その中に`NOTE.TXT`（cluster 6）を
/// 持つfixture。`.`と`..`は列挙から除外されることを確認するために入れる。
fn mounted_nested_fixture() -> Fat32<MemoryReader<5>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);
    write_fat_entry(&mut fat, 5, 0x0fff_ffff);
    write_fat_entry(&mut fat, 6, 0x0fff_ffff);

    let mut root_cluster = [0; 512];
    write_directory_entry(&mut root_cluster, 0, b"SUBDIR     ", 0x10, 5, 0);
    write_directory_entry(&mut root_cluster, 1, b"HELLO   TXT", 0x20, 4, 0);

    let mut subdir = [0; 512];
    write_directory_entry(&mut subdir, 0, b".          ", 0x10, 5, 0);
    write_directory_entry(&mut subdir, 1, b"..         ", 0x10, 2, 0);
    write_directory_entry(&mut subdir, 2, b"NOTE    TXT", 0x20, 6, 11);

    let mut data = [0; 512];
    data[..11].copy_from_slice(b"nested note");

    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root_cluster),
        (291, subdir),
        (292, data),
    ]))
    .unwrap()
}

/// `short_name`に対するLFN checksum record列を`sector`の`index`以降へ
/// 書く。disk上は最終chunkから順に並ぶ。
fn write_lfn_records(sector: &mut [u8; 512], index: usize, name: &str, short_name: &[u8; 11]) {
    let chars: Vec<u16> = name.encode_utf16().collect();
    let chunks = chars.len().div_ceil(13);
    let checksum = lfn_checksum(short_name);
    for chunk in 0..chunks {
        // disk上は末尾chunk（seq=N, `0x40`flag付き）から並ぶ。seq `s`の
        // recordはnameの`(s-1)*13`文字目からの13文字を保持する。
        let seq = (chunks - chunk) as u8;
        let record = &mut sector[(index + chunk) * 32..(index + chunk) * 32 + 32];
        record[0] = if chunk == 0 { seq | 0x40 } else { seq };
        record[11] = 0x0f;
        record[13] = checksum;
        for position in 0..13 {
            let ch_index = (seq as usize - 1) * 13 + position;
            // name直後の`0x0000`終端、その後は`0xffff`で埋める。
            let ch = if ch_index == chars.len() {
                0x0000
            } else {
                chars.get(ch_index).copied().unwrap_or(0xffff)
            };
            let offset = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30][position];
            record[offset..offset + 2].copy_from_slice(&ch.to_le_bytes());
        }
    }
}

/// rootに`Long File Name.txt`（chain 4→5）を持つwritable fixture。
/// LFN record 2個とshort entryがindex 0..=2に並び、index 3は終端。
/// cluster 6以降はfree。
fn mounted_unlink_lfn_fixture() -> Fat32<MemoryReader<7>> {
    let mut fat = writable_fat();
    write_fat_entry(&mut fat, 4, 5);

    let mut root = [0; 512];
    write_lfn_records(&mut root, 0, "Long File Name.txt", b"LONGFI~1TXT");
    write_directory_entry(&mut root, 2, b"LONGFI~1TXT", 0x20, 4, 13);

    let mut data_a = [0; 512];
    data_a[..7].copy_from_slice(b"long da");
    let mut data_b = [0; 512];
    data_b[..6].copy_from_slice(b"ta end");

    Fat32::mount(MemoryReader::with_sectors([
        (0, writable_boot_sector()),
        (32, fat),
        (33, root),
        (34, [0; 512]),
        (35, data_a),
        (36, data_b),
        (37, [0; 512]),
    ]))
    .unwrap()
}
