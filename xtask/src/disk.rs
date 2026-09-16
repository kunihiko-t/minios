//! QEMU virtio-blk検査用の決定的なFAT32 disk image生成。
//!
//! kernel側`storage::fat32`が要求するgeometry制約
//! (`bytes_per_sector=512`、`data_cluster_count >= 65525`等) を満たす
//! superfloppy (パーティション無し) imageを組み立てる。
//! root directoryには`HELLO.TXT` 1件だけを置き、内容は[`HELLO_TXT`]。
//!
//! imageはsparse fileとして書き出す: 総サイズ約34 MiBだが、実際に書くのは
//! boot sector・FAT 2面・root directory・file dataの数sectorだけである。

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// QEMU検査で`run_virtio_test`が照合する`HELLO.TXT`の内容。
pub const HELLO_TXT: &[u8] = b"hello from virtio\n";

/// `DOCS`subdirectory内の`NOTE.TXT`の内容。shellの`cat DOCS/NOTE.TXT`が
/// path解決とsubdirectory読み出しをend-to-endで検査する。
pub const NOTE_TXT: &[u8] = b"note inside docs\n";

/// rootの`Long File Name.txt`（8.3 alias `LONGFI~1.TXT`）の内容。
/// `ls`の長い名前表示と`cat`のLFN解決を検査する。
pub const LFN_TXT: &[u8] = b"long file contents\n";

const SECTOR: usize = 512;
const SECTORS_PER_CLUSTER: u8 = 1;
const RESERVED_SECTORS: u16 = 32;
const FAT_COUNT: u8 = 2;
const ROOT_CLUSTER: u32 = 2;
const FILE_CLUSTER: u32 = 3;
const DOCS_CLUSTER: u32 = 4;
const NOTE_CLUSTER: u32 = 5;
const LFN_CLUSTER: u32 = 6;

/// `data_cluster_count >= 65_525` (FAT32の最小cluster数) を余裕を持って
/// 満たすvolume sector数。fat_sectorsは下記で固定点反復して求める。
const VOLUME_SECTORS: u32 = 70_000;

/// FAT領域に必要なsector数。cluster数がFAT容量を左右するため、
/// `data_sectors / spc`が必要entry数を下回らない点へ反復収束させる。
fn fat_sectors() -> u32 {
    // 必要sector数はfatの非増加関数なので、上方からの反復で整合点へ
    // 近づけた後、条件を満たす最小値まで縮める。
    let needed_for = |fat: u32| {
        let clusters = (VOLUME_SECTORS - u32::from(RESERVED_SECTORS) - FAT_COUNT as u32 * fat)
            / u32::from(SECTORS_PER_CLUSTER);
        ((u64::from(clusters) + 2) * 4).div_ceil(SECTOR as u64) as u32
    };
    let mut fat = 1u32;
    loop {
        let needed = needed_for(fat);
        if needed > fat {
            fat = needed;
            continue;
        }
        if fat > 1 && needed_for(fat - 1) < fat {
            fat -= 1;
            continue;
        }
        return fat;
    }
}

/// `lba`番目のsectorへ`bytes` (512 byteちょうど) を書き込む。
fn write_sector(file: &mut std::fs::File, lba: u32, bytes: &[u8]) -> Result<(), std::io::Error> {
    debug_assert_eq!(bytes.len(), SECTOR);
    file.seek(SeekFrom::Start(u64::from(lba) * SECTOR as u64))?;
    file.write_all(bytes)
}

/// raw 8.3名に対するLFN checksum。kernel側`storage::fat32`の同名関数と
/// 同じ式で、`0x40`flag付き末尾chunkへ書き込む値を求める。
fn lfn_checksum(short_name: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &byte in short_name {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte);
    }
    sum
}

/// `name`を表すLFN record列を`dir`の`index`以降へ書く。disk上は
/// `0x40`flag付きの末尾chunkから順に並び、seq `s`はnameの
/// `(s-1)*13`文字目からの13文字を保持する。
fn write_lfn_entries(dir: &mut [u8], index: usize, name: &str, short_name: &[u8; 11]) {
    const CHAR_OFFSETS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let chars: Vec<u16> = name.encode_utf16().collect();
    let chunks = chars.len().div_ceil(13);
    let checksum = lfn_checksum(short_name);
    for chunk in 0..chunks {
        let seq = (chunks - chunk) as u8;
        let record = &mut dir[(index + chunk) * 32..(index + chunk) * 32 + 32];
        record[0] = if chunk == 0 { seq | 0x40 } else { seq };
        record[11] = 0x0f;
        record[13] = checksum;
        for (position, &offset) in CHAR_OFFSETS.iter().enumerate() {
            let ch_index = (seq as usize - 1) * 13 + position;
            let ch = if ch_index == chars.len() {
                0x0000
            } else {
                chars.get(ch_index).copied().unwrap_or(0xffff)
            };
            record[offset..offset + 2].copy_from_slice(&ch.to_le_bytes());
        }
    }
}

/// `build_fat32_image`が生成したimageの検査で使うメモリ上のreader。
/// kernelの`SectorReader`を満たすので`Fat32`にそのまま渡せる。
#[cfg(test)]
pub struct SliceReader<'a> {
    bytes: &'a [u8],
}

#[cfg(test)]
impl minios_kernel::storage::SectorReader for SliceReader<'_> {
    type Error = &'static str;

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), &'static str> {
        let start = lba as usize * SECTOR;
        let end = start + SECTOR;
        let slice = self.bytes.get(start..end).ok_or("short read")?;
        destination.copy_from_slice(slice);
        Ok(())
    }
}

/// 完全なdisk imageを`Vec`として組み立てる。テストでparserと直接照合する
/// ため、sparse書き出しとは別に全byteを返す経路も用意する。
fn image_bytes() -> Vec<u8> {
    let fat_sectors = fat_sectors();
    let data_start = u32::from(RESERVED_SECTORS) + FAT_COUNT as u32 * fat_sectors;
    let mut image = std::vec![0u8; VOLUME_SECTORS as usize * SECTOR];

    // --- boot sector (BPB) ---
    let boot = &mut image[..SECTOR];
    boot[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]); // jump
    boot[3..11].copy_from_slice(b"MSWIN4.1");
    boot[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    boot[13] = SECTORS_PER_CLUSTER;
    boot[14..16].copy_from_slice(&RESERVED_SECTORS.to_le_bytes());
    boot[16] = FAT_COUNT;
    boot[17..19].copy_from_slice(&0u16.to_le_bytes()); // root entries (FAT32)
    boot[19..21].copy_from_slice(&0u16.to_le_bytes()); // volume sectors 16
    boot[21] = 0xf8; // media
    boot[22..24].copy_from_slice(&0u16.to_le_bytes()); // fat size 16
    boot[32..36].copy_from_slice(&VOLUME_SECTORS.to_le_bytes());
    boot[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
    boot[44..48].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
    boot[48..50].copy_from_slice(&1u16.to_le_bytes()); // fsinfo sector
    boot[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
    boot[510..512].copy_from_slice(&[0x55, 0xaa]);

    // --- FAT (2面同一): [0]=media, [1]=EOC, [2]=EOC(root), [3]=EOC(file) ---
    let mut fat = std::vec![0u8; fat_sectors as usize * SECTOR];
    let set = |fat: &mut [u8], index: u32, value: u32| {
        let offset = index as usize * 4;
        fat[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    set(&mut fat, 0, 0x0fff_fff8);
    set(&mut fat, 1, 0x0fff_ffff);
    set(&mut fat, ROOT_CLUSTER, 0x0fff_ffff);
    set(&mut fat, FILE_CLUSTER, 0x0fff_ffff);
    set(&mut fat, DOCS_CLUSTER, 0x0fff_ffff);
    set(&mut fat, NOTE_CLUSTER, 0x0fff_ffff);
    set(&mut fat, LFN_CLUSTER, 0x0fff_ffff);
    for copy in 0..FAT_COUNT {
        let start = (u32::from(RESERVED_SECTORS) + u32::from(copy) * fat_sectors) as usize * SECTOR;
        image[start..start + fat.len()].copy_from_slice(&fat);
    }

    // --- root directory (cluster 2): HELLO.TXT + DOCS + 終端0x00 record ---
    let root_start = data_start as usize * SECTOR;
    let dir_entry =
        |image: &mut [u8], index: usize, name: &[u8; 11], attr: u8, cluster: u32, size: u32| {
            let record = &mut image[index * 32..index * 32 + 32];
            record[..11].copy_from_slice(name);
            record[11] = attr;
            record[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
            record[26..28].copy_from_slice(&((cluster & 0xffff) as u16).to_le_bytes());
            record[28..32].copy_from_slice(&size.to_le_bytes());
        };
    {
        let root = &mut image[root_start..root_start + SECTOR];
        dir_entry(
            root,
            0,
            b"HELLO   TXT",
            0x20,
            FILE_CLUSTER,
            HELLO_TXT.len() as u32,
        );
        dir_entry(root, 1, b"DOCS       ", 0x10, DOCS_CLUSTER, 0);
        write_lfn_entries(root, 2, "Long File Name.txt", b"LONGFI~1TXT");
        dir_entry(
            root,
            4,
            b"LONGFI~1TXT",
            0x20,
            LFN_CLUSTER,
            LFN_TXT.len() as u32,
        );
    }

    // --- DOCS directory (cluster 4): `.`/`..` + NOTE.TXT ---
    {
        let docs_start = (data_start + (DOCS_CLUSTER - 2)) as usize * SECTOR;
        let docs = &mut image[docs_start..docs_start + SECTOR];
        dir_entry(docs, 0, b".          ", 0x10, DOCS_CLUSTER, 0);
        dir_entry(docs, 1, b"..         ", 0x10, ROOT_CLUSTER, 0);
        dir_entry(
            docs,
            2,
            b"NOTE    TXT",
            0x20,
            NOTE_CLUSTER,
            NOTE_TXT.len() as u32,
        );
    }

    // --- file data (cluster 3 = HELLO.TXT, cluster 5 = NOTE.TXT) ---
    let file_start = (data_start + (FILE_CLUSTER - 2)) as usize * SECTOR;
    image[file_start..file_start + HELLO_TXT.len()].copy_from_slice(HELLO_TXT);
    let note_start = (data_start + (NOTE_CLUSTER - 2)) as usize * SECTOR;
    image[note_start..note_start + NOTE_TXT.len()].copy_from_slice(NOTE_TXT);
    let lfn_start = (data_start + (LFN_CLUSTER - 2)) as usize * SECTOR;
    image[lfn_start..lfn_start + LFN_TXT.len()].copy_from_slice(LFN_TXT);

    image
}

/// 使用中のimage pathと実バイト列の受け渡し用。QEMU起動時だけfileへ
/// materializeし、終了後は呼び出し側が削除する。
pub struct DiskImage {
    path: PathBuf,
}

impl DiskImage {
    /// FAT32 imageを組み立て、使用sectorだけを書いたsparse fileとして
    /// temp dirへ配置する。
    pub fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "minios-disk-{}-{}.img",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        let image = image_bytes();
        let mut file = std::fs::File::create(&path)
            .map_err(|error| format!("could not create {}: {error}", path.display()))?;
        // 非0 byteを含む領域だけをseek+writeし、残りはholeのままにする。
        let mut lba = 0u32;
        while (lba as usize) < VOLUME_SECTORS as usize {
            let start = lba as usize * SECTOR;
            let sector = &image[start..start + SECTOR];
            if sector.iter().any(|byte| *byte != 0) {
                write_sector(&mut file, lba, sector)
                    .map_err(|error| format!("could not write sector {lba}: {error}"))?;
            }
            lba += 1;
        }
        // 末尾を宣言して読み出し側がshort readしないようにする。
        file.set_len(u64::from(VOLUME_SECTORS) * SECTOR as u64)
            .map_err(|error| format!("could not size {}: {error}", path.display()))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::{HELLO_TXT, LFN_TXT, NOTE_TXT, SliceReader, fat_sectors, image_bytes};
    use minios_kernel::storage::fat32::Fat32;
    use std::vec::Vec;

    // Catches BPB fields the parser rejects, FAT links it flags as corrupt,
    // or a directory record it cannot match.
    #[test]
    fn generated_image_mounts_and_reads_hello_txt() {
        let image = image_bytes();
        let reader = SliceReader { bytes: &image };
        let mut fs = Fat32::mount(reader).expect("generated image must mount");

        let mut names = Vec::new();
        fs.for_each_root_entry(|entry| names.push(entry.name().to_owned()))
            .expect("root listing must succeed");
        assert_eq!(names, ["HELLO.TXT", "DOCS", "Long File Name.txt"]);

        let mut content = Vec::new();
        fs.read_root_file("HELLO.TXT", |chunk| content.extend_from_slice(chunk))
            .expect("HELLO.TXT must be readable");
        assert_eq!(content, HELLO_TXT);

        // subdirectoryは`.`/`..`を列挙せず、path経由でNOTE.TXTを読める。
        let mut docs = Vec::new();
        fs.for_each_entry("DOCS", |entry| docs.push(entry.name().to_owned()))
            .expect("DOCS listing must succeed");
        assert_eq!(docs, ["NOTE.TXT"]);
        let mut note = Vec::new();
        fs.read_file("DOCS/NOTE.TXT", |chunk| note.extend_from_slice(chunk))
            .expect("DOCS/NOTE.TXT must be readable");
        assert_eq!(note, NOTE_TXT);

        // LFN名でも8.3 aliasでも同じfileを読める。
        for name in ["Long File Name.txt", "LONGFI~1.TXT"] {
            let mut lfn = Vec::new();
            fs.read_file(name, |chunk| lfn.extend_from_slice(chunk))
                .expect("LFN file must be readable");
            assert_eq!(lfn, LFN_TXT);
        }
    }

    // Catches a fat_sectors estimate that leaves data_cluster_count under the
    // parser's FAT32 minimum or wastes space on an oversized FAT.
    #[test]
    fn fat_sectors_converges_to_a_tight_cover() {
        let fat = fat_sectors();
        let data = 70_000 - 32 - 2 * fat;
        let clusters = data;
        assert!(clusters >= 65_525);
        assert!(fat * 512 >= (clusters + 2) * 4);
        // 1 sector少ないFATではentryが収まらないことを確認する。
        assert!((fat - 1) * 512 < (clusters + 2) * 4);
    }
}
