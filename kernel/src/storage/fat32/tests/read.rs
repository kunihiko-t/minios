use super::*;
use crate::storage::fat32::{FileDesc, FileStat};

fn mounted_directory_fixture() -> Fat32<MemoryReader<4>> {
    let mut first_directory_cluster = [0; 512];
    for index in 0..16 {
        first_directory_cluster[index * 32] = 0xe5;
    }
    write_directory_entry(&mut first_directory_cluster, 1, b"LFN     TXT", 0x0f, 0, 0);
    write_directory_entry(&mut first_directory_cluster, 2, b"LABEL      ", 0x08, 0, 0);

    let mut second_directory_cluster = [0; 512];
    write_directory_entry(&mut second_directory_cluster, 0, b"HELLO   TXT", 0x20, 4, 0);
    write_directory_entry(&mut second_directory_cluster, 1, b"SUBDIR     ", 0x10, 5, 0);
    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, directory_fat()),
        (288, first_directory_cluster),
        (289, second_directory_cluster),
    ]))
    .unwrap()
}

/// rootに`Long File Name.txt`（alias `LONGFI~1.TXT`）を持つfixture。
/// `corrupt`を立てるとchecksumを壊して8.3名へfallbackさせる。
fn mounted_lfn_fixture(corrupt: bool) -> Fat32<MemoryReader<4>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);
    write_fat_entry(&mut fat, 4, 0x0fff_ffff);

    let mut root = [0; 512];
    write_lfn_records(&mut root, 0, "Long File Name.txt", b"LONGFI~1TXT");
    if corrupt {
        // `0x40`flag付きの末尾chunkが持つchecksumを壊す。
        root[13] ^= 0xff;
    }
    write_directory_entry(&mut root, 2, b"LONGFI~1TXT", 0x20, 4, 8);

    let mut data = [0; 512];
    data[..8].copy_from_slice(b"lfn data");

    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root),
        (290, data),
    ]))
    .unwrap()
}

/// LFN record列とshort entryの間にdeleted entryが挟まるfixture。
/// 対応が切れて8.3名へfallbackすることを確認する。
fn mounted_severed_lfn_fixture() -> Fat32<MemoryReader<3>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);

    let mut root = [0; 512];
    write_lfn_records(&mut root, 0, "Long File Name.txt", b"LONGFI~1TXT");
    // LFN列の直後をdeleted entryで区切り、対応を断つ。
    root[64] = 0xe5;
    write_directory_entry(&mut root, 3, b"LONGFI~1TXT", 0x20, 0, 0);

    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root),
    ]))
    .unwrap()
}

fn mounted_multicluster_file_fixture(content: &[u8]) -> Fat32<MemoryReader<5>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);
    write_fat_entry(&mut fat, 4, 5);
    write_fat_entry(&mut fat, 5, 0x0fff_ffff);

    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, content.len() as u32);

    assert!(content.len() <= 1024);
    let first_len = if content.len() < 512 {
        content.len()
    } else {
        512
    };
    let mut data = [0; 512];
    data[..first_len].copy_from_slice(&content[..first_len]);
    let mut next_data = [0xa5; 512];
    if content.len() > first_len {
        next_data[..content.len() - first_len].copy_from_slice(&content[first_len..]);
    }
    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root),
        (290, data),
        (291, next_data),
    ]))
    .unwrap()
}

fn mounted_empty_file_fixture() -> Fat32<MemoryReader<3>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);

    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"EMPTY   TXT", 0x20, 0, 0);
    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root),
    ]))
    .unwrap()
}

fn mounted_cyclic_file_fixture() -> Fat32<MemoryReader<4>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);
    write_fat_entry(&mut fat, 4, 4);

    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"LOOP    BIN", 0x20, 4, u32::MAX);
    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root),
        (290, [0x5a; 512]),
    ]))
    .unwrap()
}

fn mounted_malformed_name_fixture() -> Fat32<MemoryReader<3>> {
    let mut fat = [0; 512];
    write_fat_entry(&mut fat, 2, 0x0fff_ffff);
    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"BAD.NAME   ", 0x20, 4, 0);
    write_directory_entry(&mut root, 1, b"GOOD    TXT", 0x20, 4, 0);
    Fat32::mount(MemoryReader::with_sectors([
        (0, valid_boot_sector()),
        (32, fat),
        (288, root),
    ]))
    .unwrap()
}

#[test]
fn root_listing_filters_metadata_and_crosses_a_cluster_boundary() {
    let mut volume = mounted_directory_fixture();
    let mut names = Vec::new();
    volume
        .for_each_entry("", |entry| names.push(String::from(entry.name())))
        .unwrap();
    assert_eq!(names, ["HELLO.TXT", "SUBDIR"]);
}

#[test]
fn file_lookup_is_ascii_case_insensitive_and_streams_exact_size() {
    let mut volume = mounted_multicluster_file_fixture(b"hello from sd\n");
    let mut output = Vec::new();
    volume
        .read_file("hello.txt", |bytes| output.extend_from_slice(bytes))
        .unwrap();
    assert_eq!(output, b"hello from sd\n");
}

#[test]
fn empty_file_reads_no_data_cluster() {
    let mut volume = mounted_empty_file_fixture();
    let mut called = false;
    volume.read_file("EMPTY.TXT", |_| called = true).unwrap();
    assert!(!called);
}

#[test]
fn cyclic_chain_is_rejected_before_the_traversal_bound_is_exceeded() {
    let mut volume = mounted_cyclic_file_fixture();
    assert_eq!(
        volume.read_file("LOOP.BIN", |_| {}),
        Err(FatError::CorruptChain)
    );
}

#[test]
fn root_listing_stops_at_the_directory_end_marker() {
    let mut volume = mounted_multicluster_file_fixture(b"contents");
    let mut entries = Vec::<DirEntry>::new();
    volume
        .for_each_entry("", |entry| entries.push(*entry))
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "HELLO.TXT");
}

#[test]
fn root_listing_ignores_dots_inside_raw_short_name_fields() {
    let mut volume = mounted_malformed_name_fixture();
    let mut names = Vec::new();
    volume
        .for_each_entry("", |entry| names.push(String::from(entry.name())))
        .unwrap();
    assert_eq!(names, ["GOOD.TXT"]);
}

#[test]
fn file_lookup_rejects_missing_directory_and_invalid_names() {
    let mut volume = mounted_directory_fixture();
    assert_eq!(
        volume.read_file("MISSING.TXT", |_| {}),
        Err(FatError::NotFound)
    );
    assert_eq!(
        volume.read_file("SUBDIR", |_| {}),
        Err(FatError::IsDirectory)
    );
    // `A`がrootに存在しないため、path解決は`NotFound`で止まる。
    // `.TXT`や`A..TXT`のように8.3では不正でもLFNでは有効な要素は
    // 同じくlookupへ進み`NotFound`となる。
    for name in ["A/B.TXT", ".TXT", "TOO-LONG9.TXT", "A..TXT"] {
        assert_eq!(
            volume.read_file(name, |_| {}),
            Err(FatError::NotFound),
            "{name} unexpectedly accepted"
        );
    }
    for name in [
        "",
        "A\\\\B.TXT",
        "A//B.TXT",
        "/HELLO.TXT",
        "HELLO.TXT/",
        "./HELLO.TXT",
        "../HELLO.TXT",
    ] {
        assert_eq!(
            volume.read_file(name, |_| {}),
            Err(FatError::InvalidName),
            "{name} unexpectedly accepted"
        );
    }
}

#[test]
fn subdirectory_listing_filters_dot_entries() {
    let mut volume = mounted_nested_fixture();
    let mut names = Vec::new();
    volume
        .for_each_entry("SUBDIR", |entry| names.push(String::from(entry.name())))
        .unwrap();
    assert_eq!(names, ["NOTE.TXT"]);
}

#[test]
fn nested_file_reads_and_matches_ascii_case_insensitively() {
    let mut volume = mounted_nested_fixture();
    let mut output = Vec::new();
    volume
        .read_file("subdir/note.txt", |bytes| output.extend_from_slice(bytes))
        .unwrap();
    assert_eq!(output, b"nested note");
}

#[test]
fn nested_lookup_reports_missing_files_and_non_directories() {
    let mut volume = mounted_nested_fixture();
    assert_eq!(
        volume.read_file("SUBDIR/MISSING.TXT", |_| {}),
        Err(FatError::NotFound)
    );
    assert_eq!(
        volume.read_file("SUBDIR", |_| {}),
        Err(FatError::IsDirectory)
    );
    // fileをdirectoryとして潜るpathと、file自体を列挙するpath。
    assert_eq!(
        volume.read_file("HELLO.TXT/NOTE.TXT", |_| {}),
        Err(FatError::NotDirectory)
    );
    let mut visited = false;
    assert_eq!(
        volume.for_each_entry("HELLO.TXT", |_| visited = true),
        Err(FatError::NotDirectory)
    );
    assert!(!visited);
}

#[test]
fn lfn_name_appears_in_listing_and_resolves_for_reads() {
    let mut volume = mounted_lfn_fixture(false);
    let mut names = Vec::new();
    volume
        .for_each_entry("", |entry| names.push(String::from(entry.name())))
        .unwrap();
    assert_eq!(names, ["Long File Name.txt"]);

    // LFN名でも8.3 aliasでも読める。大小文字は区別しない。
    for name in ["Long File Name.txt", "long file name.txt", "LONGFI~1.TXT"] {
        let mut output = Vec::new();
        volume
            .read_file(name, |bytes| output.extend_from_slice(bytes))
            .unwrap_or_else(|error| panic!("{name} must resolve: {error:?}"));
        assert_eq!(output, b"lfn data");
    }
}

#[test]
fn lfn_falls_back_to_the_short_name_on_a_bad_checksum() {
    let mut volume = mounted_lfn_fixture(true);
    let mut names = Vec::new();
    volume
        .for_each_entry("", |entry| names.push(String::from(entry.name())))
        .unwrap();
    assert_eq!(names, ["LONGFI~1.TXT"]);
    assert_eq!(
        volume.read_file("Long File Name.txt", |_| {}),
        Err(FatError::NotFound)
    );
}

#[test]
fn lfn_falls_back_when_a_deleted_entry_severs_the_association() {
    let mut volume = mounted_severed_lfn_fixture();
    let mut names = Vec::new();
    volume
        .for_each_entry("", |entry| names.push(String::from(entry.name())))
        .unwrap();
    assert_eq!(names, ["LONGFI~1.TXT"]);
}

#[test]
fn file_streaming_emits_only_the_declared_final_partial_sector() {
    let mut volume = mounted_multicluster_file_fixture(&[0x11; 513]);
    let mut output = Vec::new();
    volume
        .read_file("HELLO.TXT", |bytes| output.extend_from_slice(bytes))
        .unwrap();
    assert_eq!(output.len(), 513);
    assert_eq!(output[512], 0x11);
}

#[test]
fn file_streaming_rejects_free_bad_reserved_and_out_of_range_clusters() {
    for value in [0, 1, 0x0fff_fff0, 0x0fff_fff7, 0x0fff_fff8, 69_714] {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, value);
        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"BAD     BIN", 0x20, 4, 513);
        let mut volume = Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
            (290, [0x22; 512]),
        ]))
        .unwrap();
        assert_eq!(
            volume.read_file("BAD.BIN", |_| {}),
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

// Catches open_file not resolving paths, accepting directories, or losing
// the entry's cluster and size.
#[test]
fn open_file_returns_a_descriptor_and_rejects_directories() {
    let mut fs = mounted_nested_fixture();
    let desc = fs.open_file("SUBDIR/NOTE.TXT").unwrap();
    assert_eq!(desc.size, 11);

    assert!(matches!(fs.open_file("SUBDIR"), Err(FatError::IsDirectory)));
    assert!(matches!(fs.open_file("MISSING"), Err(FatError::NotFound)));
    assert!(matches!(fs.open_file("A//B"), Err(FatError::InvalidName)));
}

// Catches stat rejecting directories, dropping the size, or resolving
// a different entry than open_file: file and directory metadata must
// come from the same resolve_path as the rest of the API.
#[test]
fn stat_reports_size_and_kind_for_files_and_directories() {
    let mut fs = mounted_nested_fixture();

    let file = fs.stat("SUBDIR/NOTE.TXT").unwrap();
    assert_eq!(
        file,
        FileStat {
            size: 11,
            directory: false
        }
    );

    let dir = fs.stat("SUBDIR").unwrap();
    assert_eq!(
        dir,
        FileStat {
            size: 0,
            directory: true
        }
    );

    assert!(matches!(fs.stat("MISSING"), Err(FatError::NotFound)));
    assert!(matches!(fs.stat("A//B"), Err(FatError::InvalidName)));
    assert!(matches!(
        fs.stat("SUBDIR/NOTE.TXT/DEEP"),
        Err(FatError::NotDirectory)
    ));
}

// Catches stat reporting a stale size after writes: the dir entry
// write-back must be visible to a later stat on the same path.
#[test]
fn stat_sees_size_growth_after_write() {
    let mut fs = mounted_writable_fixture();
    let mut desc = fs.create_file("GROW.TXT").unwrap();
    assert_eq!(fs.stat("GROW.TXT").unwrap().size, 0);

    fs.write_range(&mut desc, 0, b"abcde").unwrap();
    let file = fs.stat("GROW.TXT").unwrap();
    assert_eq!(file.size, 5);
}

// Catches entry_at enumerating the wrong order, exposing `.`/`..`,
// or reporting EOF too early: the index sequence must match
// for_each_entry and end exactly at the entry count.
#[test]
fn entry_at_enumerates_entries_by_index_and_reports_eof() {
    let mut fs = mounted_nested_fixture();

    // ""はroot。nested fixtureのrootはSUBDIRとHELLO.TXTの2件。
    let first = fs.entry_at("", 0).unwrap().unwrap();
    assert_eq!(first.name(), "SUBDIR");
    assert!(first.is_directory());
    let second = fs.entry_at("", 1).unwrap().unwrap();
    assert_eq!(second.name(), "HELLO.TXT");
    assert!(!second.is_directory());
    assert!(fs.entry_at("", 2).unwrap().is_none());
    assert!(fs.entry_at("", 9).unwrap().is_none());

    // subdirectoryも同じindex規約。NOTE.TXTだけが列挙される。
    let note = fs.entry_at("SUBDIR", 0).unwrap().unwrap();
    assert_eq!(note.name(), "NOTE.TXT");
    assert_eq!(note.size(), 11);
    assert!(fs.entry_at("SUBDIR", 1).unwrap().is_none());

    // file・不在・不正名は列挙ではなくerrnoとして返る。
    assert!(matches!(
        fs.entry_at("HELLO.TXT", 0),
        Err(FatError::NotDirectory)
    ));
    assert!(matches!(fs.entry_at("MISSING", 0), Err(FatError::NotFound)));
    assert!(matches!(fs.entry_at("A//B", 0), Err(FatError::InvalidName)));
}

// Catches read_range skipping the wrong clusters, ignoring the output
// bound, or not reporting EOF.
#[test]
fn read_range_streams_from_any_offset_within_the_chain() {
    let mut content = [0x30u8; 700];
    for (index, byte) in content.iter_mut().enumerate() {
        *byte = b'0' + (index % 10) as u8;
    }
    let mut fs = mounted_multicluster_file_fixture(&content);
    let desc = fs.open_file("HELLO.TXT").unwrap();

    let mut output = [0; 700];
    assert_eq!(fs.read_range(&desc, 0, &mut output).unwrap(), 700);
    assert_eq!(output, content);

    // cluster境界をまたぐoffset: 1 byte目がcluster 5の先頭。
    let mut tail = [0; 700];
    assert_eq!(fs.read_range(&desc, 500, &mut tail).unwrap(), 200);
    assert_eq!(tail[..200], content[500..]);

    // file末尾を超える要求はsizeで打ち切る。
    let mut end = [0; 700];
    assert_eq!(fs.read_range(&desc, 690, &mut end).unwrap(), 10);
    assert_eq!(end[..10], content[690..]);

    assert_eq!(fs.read_range(&desc, 700, &mut end).unwrap(), 0);
    assert_eq!(fs.read_range(&desc, 9000, &mut end).unwrap(), 0);
    assert_eq!(fs.read_range(&desc, 0, &mut []).unwrap(), 0);
}

// Catches read_range treating an empty file's cluster 0 as a chain start
// or following a chain past the mounted bound.
#[test]
fn read_range_handles_empty_files_and_corrupt_chains() {
    let mut fs = mounted_empty_file_fixture();
    let desc = fs.open_file("EMPTY.TXT").unwrap();
    let mut output = [0xaa; 8];
    assert_eq!(fs.read_range(&desc, 0, &mut output).unwrap(), 0);

    // cluster 0を指す破損descはchainへ入る前にCorruptChain。
    let bad = FileDesc {
        first_cluster: 0,
        size: 10,
        dir_cluster: 2,
        dir_index: 0,
    };
    assert!(matches!(
        fs.read_range(&bad, 0, &mut output),
        Err(FatError::CorruptChain)
    ));

    // offsetがcluster boundを超える場合も同じくCorruptChain。
    let mut cyclic = mounted_cyclic_file_fixture();
    let desc = cyclic.open_file("LOOP.BIN").unwrap();
    assert!(matches!(
        cyclic.read_range(&desc, u64::from(u32::MAX) - 512, &mut output),
        Err(FatError::CorruptChain)
    ));
}
