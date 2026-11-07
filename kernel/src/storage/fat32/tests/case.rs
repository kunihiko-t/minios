use super::*;
use crate::storage::fat32::RenameOutcome;

fn names<const N: usize>(fs: &mut Fat32<MemoryReader<N>>, path: &str) -> Vec<String> {
    let mut names = Vec::new();
    fs.for_each_entry(path, |entry| names.push(String::from(entry.name())))
        .unwrap();
    names
}

// Catches the case of an 8.3-shaped name being lost or costing LFN
// records: a part that is all lowercase must set its `NTRes` flag (base
// 0x08, extension 0x10) on a plain 8.3 entry, an all-uppercase part must
// set none, and the listing must show each part in its own case.
#[test]
fn uniform_case_short_names_keep_their_case_without_lfn() {
    let mut fs = mounted_writable_fixture();
    for (name, slot, flags) in [
        ("notes.txt", 2, 0x18),
        ("README.MD", 3, 0x00),
        ("build.LOG", 4, 0x08),
        ("DATA.bin", 5, 0x10),
        ("work", 6, 0x08),
        ("2026.txt", 7, 0x10),
    ] {
        let desc = fs.create_file(name).unwrap();
        // LFN recordを持たないので、entryは次の空きslotへ1個だけ置かれる。
        assert_eq!((desc.dir_cluster, desc.dir_index), (2, slot), "{name}");
        let offset = slot as usize * 32;
        assert_eq!(sector_of(&fs, 33)[offset + 12], flags, "{name}");
    }
    assert_eq!(
        names(&mut fs, ""),
        [
            "HELLO.TXT",
            "SUBDIR",
            "notes.txt",
            "README.MD",
            "build.LOG",
            "DATA.bin",
            "work",
            "2026.txt"
        ]
    );
    // 照合は大小文字を区別しない。別の大小文字で作り直しても同じentryを開く。
    let again = fs.create_file("NOTES.TXT").unwrap();
    assert_eq!((again.dir_cluster, again.dir_index), (2, 2));
    assert_eq!(fs.open_file("Notes.Txt").unwrap().dir_index, 2);
}

// Catches a mixed-case 8.3 name being flattened to one case: it must be
// kept in LFN records whose alias is the plain uppercase 8.3 name (no
// numeric tail when that name is free), so both spellings resolve and a
// later all-lowercase create reopens the same entry.
#[test]
fn mixed_case_short_names_use_lfn_with_the_plain_alias() {
    let mut fs = mounted_writable_fixture();
    let desc = fs.create_file("Makefile").unwrap();
    // LFN recordが1個、alias entryがその次のslotに来る。
    assert_eq!((desc.dir_cluster, desc.dir_index), (2, 3));
    let root = sector_of(&fs, 33);
    assert_eq!(root[64 + 11], 0x0f);
    assert_eq!(&root[96..107], b"MAKEFILE   ");
    assert_eq!(root[96 + 12], 0);
    assert_eq!(names(&mut fs, ""), ["HELLO.TXT", "SUBDIR", "Makefile"]);
    for path in ["Makefile", "MAKEFILE", "makefile"] {
        assert_eq!(fs.open_file(path).unwrap().dir_index, 3, "{path}");
    }
    let again = fs.create_file("makefile").unwrap();
    assert_eq!((again.dir_cluster, again.dir_index), (2, 3));

    // 大小が混ざる名前でも、同じ8.3名の既存entryがあれば新しく作らずに開く。
    let desc = fs.create_file("Hello.Txt").unwrap();
    assert_eq!((desc.dir_cluster, desc.dir_index), (2, 0));
}

// Catches case-only renames being treated as no-ops that keep the old
// display name: lowering an 8.3 name must rewrite the `NTRes` flags in
// place, and moving to a mixed-case spelling must reuse the entry's own
// 8.3 name as the alias instead of taking a `~1` tail.
#[test]
fn case_only_renames_change_the_listed_name() {
    let mut fs = mounted_writable_fixture();
    let outcome = fs.rename("HELLO.TXT", "hello.txt").unwrap();
    assert_eq!(
        outcome,
        RenameOutcome {
            moved: None,
            replaced: None
        }
    );
    assert_eq!(sector_of(&fs, 33)[12], 0x18);
    assert_eq!(names(&mut fs, ""), ["hello.txt", "SUBDIR"]);

    let outcome = fs.rename("hello.txt", "Hello.txt").unwrap();
    assert_eq!(outcome.moved, Some(((2, 0), (2, 3))));
    assert_eq!(&sector_of(&fs, 33)[96..107], b"HELLO   TXT");
    assert_eq!(names(&mut fs, ""), ["SUBDIR", "Hello.txt"]);

    // LFNの名前から小文字だけの8.3名へ戻すとLFN recordは消える。
    fs.rename("Hello.txt", "SUBDIR/greeting.txt").unwrap();
    assert_eq!(names(&mut fs, ""), ["SUBDIR"]);
    assert_eq!(names(&mut fs, "SUBDIR"), ["greeting.txt"]);
    let desc = fs.open_file("SUBDIR/GREETING.TXT").unwrap();
    let mut out = [0u8; 11];
    assert_eq!(fs.read_range(&desc, 0, &mut out), Ok(11));
    assert_eq!(&out, b"hello world");
}
