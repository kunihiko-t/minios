use super::*;
use crate::storage::fat32::lfn::{alias_candidate, lfn_records};

fn names<const N: usize>(fs: &mut Fat32<MemoryReader<N>>, path: &str) -> Vec<String> {
    let mut names = Vec::new();
    fs.for_each_entry(path, |entry| names.push(String::from(entry.name())))
        .unwrap();
    names
}

// Catches alias generation producing invalid or colliding 8.3 names: the
// basis must drop spaces and dots, map non-8.3 bytes to `_`, keep the last
// extension, and shorten the base so a longer numeric tail still fits.
#[test]
fn alias_candidates_follow_the_basis_and_tail_rules() {
    assert_eq!(&alias_candidate("long file.txt", 1), b"LONGFI~1TXT");
    assert_eq!(&alias_candidate("long file.txt", 10), b"LONGF~10TXT");
    assert_eq!(&alias_candidate(".hidden", 1), b"HIDDEN~1   ");
    assert_eq!(&alias_candidate("a+b.tar.gz", 1), b"A_BTAR~1GZ ");
    assert_eq!(&alias_candidate("x y", 3), b"XY~3       ");
}

// Catches LFN records that readers would reject: the last fragment comes
// first with the 0x40 flag, each record carries the checksum, a name that
// ends inside a record gets one NUL and 0xffff padding, and a name that
// fills its last record exactly gets neither.
#[test]
fn lfn_records_order_fragments_and_pad_the_tail() {
    let records = lfn_records("abcdefghijklmn", 0x5a);
    assert_eq!(records.len(), 2);
    assert_eq!((records[0][0], records[1][0]), (0x42, 0x01));
    assert!(records.iter().all(|r| r[11] == 0x0f && r[13] == 0x5a));
    // 2個目の断片は"n"、NUL、0xffffの順。
    assert_eq!(&records[0][1..7], &[b'n', 0, 0, 0, 0xff, 0xff]);
    assert_eq!(&records[1][1..3], &[b'a', 0]);

    let exact = lfn_records("abcdefghijklm", 0);
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0][0], 0x41);
    assert_eq!(&exact[0][30..32], &[b'm', 0]);
}

// Catches LFN creation writing records the read path cannot pair with the
// alias, or create_file losing its reopen semantics: the long name must be
// listed and resolvable (also case-insensitively and via its alias), and a
// second create must return the same entry instead of a duplicate.
#[test]
fn create_file_with_a_long_name_round_trips() {
    let mut fs = mounted_writable_fixture();
    let mut desc = fs.create_file("long file.txt").unwrap();
    assert_eq!((desc.dir_cluster, desc.dir_index), (2, 3));
    assert_eq!(fs.write_range(&mut desc, 0, b"lfn data"), Ok(8));

    assert_eq!(names(&mut fs, ""), ["HELLO.TXT", "SUBDIR", "long file.txt"]);
    for path in ["long file.txt", "LONG FILE.TXT", "LONGFI~1.TXT"] {
        let found = fs.open_file(path).unwrap();
        assert_eq!((found.dir_cluster, found.dir_index, found.size), (2, 3, 8));
    }
    let again = fs.create_file("Long File.TXT").unwrap();
    assert_eq!((again.dir_cluster, again.dir_index), (2, 3));

    // 同じbasisの別名は次の数字tailを使う。
    let second = fs.create_file("long filez.txt").unwrap();
    assert_eq!(second.dir_index, 6);
    let root = sector_of(&fs, 33);
    assert_eq!(&root[6 * 32..6 * 32 + 11], b"LONGFI~2TXT");
    // 最後のentryの次slotは終端印のままである。
    assert_eq!(root[7 * 32], 0x00);
}

// Catches names that FAT forbids in an LFN being written anyway: the
// reserved characters and trailing space or dot must be rejected before
// the directory changes.
#[test]
fn long_names_with_reserved_characters_are_rejected() {
    let mut fs = mounted_writable_fixture();
    let before = *sector_of(&fs, 33);
    for name in [
        "a:b.txt",
        "what?",
        "star*",
        "pipe|x",
        "quote\"x",
        "long name.",
        "space ",
    ] {
        assert!(
            matches!(fs.create_file(name), Err(FatError::InvalidName)),
            "{name}"
        );
    }
    assert_eq!(sector_of(&fs, 33), &before);
}

// Catches a long directory name breaking path resolution below it: files
// created inside an LFN directory must resolve through the long name.
#[test]
fn create_dir_with_a_long_name_holds_long_children() {
    let mut fs = mounted_writable_fixture();
    fs.create_dir("my documents").unwrap();
    assert!(matches!(
        fs.create_dir("MY DOCUMENTS"),
        Err(FatError::Exists)
    ));
    let mut inner = fs.create_file("my documents/inner note.txt").unwrap();
    assert_eq!(fs.write_range(&mut inner, 0, b"nested"), Ok(6));
    assert_eq!(names(&mut fs, "my documents"), ["inner note.txt"]);
    let found = fs.open_file("My Documents/INNER NOTE.TXT").unwrap();
    assert_eq!(found.size, 6);
}

// Catches LFN renames losing the entry or leaving the old name behind:
// renaming within a directory and across directories must move the whole
// record run, report the new location, and keep the file contents.
#[test]
fn rename_to_a_long_name_moves_the_record_run() {
    let mut fs = mounted_writable_fixture();
    let outcome = fs.rename("HELLO.TXT", "hello world.txt").unwrap();
    assert_eq!(outcome.moved, Some(((2, 0), (2, 4))));
    assert_eq!(outcome.replaced, None);
    assert_eq!(names(&mut fs, ""), ["SUBDIR", "hello world.txt"]);
    let desc = fs.open_file("hello world.txt").unwrap();
    let mut out = [0u8; 11];
    assert_eq!(fs.read_range(&desc, 0, &mut out), Ok(11));
    assert_eq!(&out, b"hello world");

    // 別directoryへLFN名のまま移し、さらに8.3名へ戻す。
    let outcome = fs
        .rename("hello world.txt", "SUBDIR/moved greeting.txt")
        .unwrap();
    assert!(outcome.moved.is_some());
    assert_eq!(names(&mut fs, ""), ["SUBDIR"]);
    assert_eq!(names(&mut fs, "SUBDIR"), ["moved greeting.txt"]);
    fs.rename("SUBDIR/moved greeting.txt", "SUBDIR/SHORT.TXT")
        .unwrap();
    assert_eq!(names(&mut fs, "SUBDIR"), ["SHORT.TXT"]);
}

// Catches unlink leaving orphaned LFN records that would attach to a later
// entry: deleting a long-named file must mark its whole run deleted.
#[test]
fn unlink_marks_the_long_record_run_deleted() {
    let mut fs = mounted_writable_fixture();
    fs.create_file("to be removed.txt").unwrap();
    fs.unlink_file("to be removed.txt").unwrap();
    let root = sector_of(&fs, 33);
    // LFN 2個とalias entryの3 slotがすべて削除跡になる。
    assert!((2..5).all(|index| root[index * 32] == 0xe5));
    assert_eq!(names(&mut fs, ""), ["HELLO.TXT", "SUBDIR"]);
}

// Catches the append path failing to grow the directory: once the
// one-sector root fills up, the next long entry must continue into a newly
// linked cluster and every name must stay listed.
#[test]
fn long_entries_spill_into_a_new_directory_cluster() {
    let mut fs = mounted_writable_fixture();
    // 1 file = LFN 1個 + alias entryの2 slot。rootの残り14 slotを7個で
    // 埋め、8個目が新clusterへ溢れる。
    let created: Vec<String> = (0..8).map(|n| std::format!("long name {n}")).collect();
    for name in &created {
        fs.create_file(name).unwrap();
    }
    let listed = names(&mut fs, "");
    assert_eq!(listed.len(), 10);
    assert!(created.iter().all(|name| listed.contains(name)));
    let last = fs.open_file("long name 7").unwrap();
    assert_ne!(last.dir_cluster, 2);
}

// Catches a cross-directory move leaving the source alias behind: when a
// long-named file moves to another directory under an 8.3 name, both its
// LFN records and its alias entry must disappear from the old directory.
#[test]
fn moving_a_long_named_file_under_a_short_name_removes_the_alias() {
    let mut fs = mounted_writable_fixture();
    fs.create_file("long file.txt").unwrap();
    let outcome = fs.rename("long file.txt", "SUBDIR/X.TXT").unwrap();
    assert!(outcome.moved.is_some());
    assert_eq!(names(&mut fs, ""), ["HELLO.TXT", "SUBDIR"]);
    assert_eq!(names(&mut fs, "SUBDIR"), ["X.TXT"]);
    let root = sector_of(&fs, 33);
    assert_eq!((root[2 * 32], root[3 * 32]), (0xe5, 0xe5));
}
