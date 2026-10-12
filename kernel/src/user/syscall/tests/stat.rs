use super::*;

// Catches stat dropping the metadata, mislabeling the out copy, or
// reaching the source before the out pointer is validated: a success
// must produce a ReadComplete of STAT_LEN serialized bytes at `a2`.
#[test]
fn stat_writes_the_serialized_metadata_to_the_out_pointer() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let mut scratch_buf = scratch();
    let (context, flow) = dispatch_numbered_file_fixture(
        STAT_NUMBER,
        MESSAGE_PAGE,
        9,
        MESSAGE_PAGE + 0x100,
        0,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    assert_eq!(
        flow,
        SyscallFlow::ReadComplete {
            start: (MESSAGE_PAGE + 0x100) as u64,
            len: STAT_LEN,
        }
    );
    assert_eq!(source.stats, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"HELLO.TXT".as_slice()));
    assert_eq!(
        Stat::from_le_bytes(scratch_buf[..STAT_LEN].try_into().unwrap()),
        Stat {
            size: 18,
            kind: STAT_KIND_FILE,
        }
    );
    // 成功時の戻り値は`ReadComplete`のcopy-out経路が`a0`へ書く
    // `len` (=STAT_LEN)で、`read`系のbyte-count規約に従う。
    // dispatch自体はa0へ触れない。
    let _ = context;
}

// Catches an unwritable out pointer reaching the source or passing
// without EFAULT: validation must reject it before any side effect.
#[test]
fn stat_rejects_an_unwritable_out_pointer_before_the_source() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        STAT_NUMBER,
        MESSAGE_PAGE,
        9,
        0,
        0,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.stats, 0);
}

// Catches stat swallowing the source errno: ENOENT/ENOTDIR must pass
// through to a0 unchanged.
#[test]
fn stat_passes_the_source_errno_through() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    source.stat_result = Err(ENOENT);
    let (context, flow) = dispatch_numbered_file_fixture(
        STAT_NUMBER,
        MESSAGE_PAGE,
        9,
        MESSAGE_PAGE + 0x100,
        0,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOENT as usize);
    assert_eq!(source.stats, 1);
}

// Catches fstat dispatching to the wrong fd or skipping the writable
// check: a file fd must reach the source and produce the same
// ReadComplete shape as stat.
#[test]
fn fstat_writes_metadata_for_a_file_fd() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let mut scratch_buf = scratch();
    let (context, flow) = dispatch_fixture(
        FSTAT_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE + 0x200,
        0,
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    assert_eq!(
        flow,
        SyscallFlow::ReadComplete {
            start: (MESSAGE_PAGE + 0x200) as u64,
            len: STAT_LEN,
        }
    );
    assert_eq!(source.fstats, 1);
    assert_eq!(source.seen_fstat_fd, Some(FIRST_FILE_FD));
    let _ = context;
}

// Catches fstat accepting an out-of-range fd, and an unwritable out
// pointer reaching the source.
#[test]
fn fstat_rejects_bad_fds_and_unwritable_out_pointers() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_fixture(
        FSTAT_NUMBER,
        super::FD_TABLE_LEN,
        MESSAGE_PAGE,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EBADF as usize);
    assert_eq!(source.fstats, 0);

    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_fixture(
        FSTAT_NUMBER,
        FIRST_FILE_FD,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.fstats, 0);
}

// Catches the default ControlSource::stat/fstat leaking a successful
// result: without storage the guest must see ENOSYS.
#[test]
fn stat_and_fstat_without_storage_return_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, _) = dispatch_numbered_file_fixture(
        STAT_NUMBER,
        MESSAGE_PAGE,
        9,
        MESSAGE_PAGE + 0x100,
        0,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), ENOSYS as usize);

    let mut source = FakeSource::scripted(b"stdin only");
    let (context, _) = dispatch_fixture(
        FSTAT_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches readdir dropping the entry, mislabeling the out copy, or
// reaching the source before the out pointer is validated: a hit
// must produce a ReadComplete of DIRENT_LEN serialized bytes at `a3`.
#[test]
fn readdir_writes_the_serialized_entry_to_the_out_pointer() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let mut scratch_buf = scratch();
    let (_, flow) = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        4,
        0,
        MESSAGE_PAGE + 0x100,
        b"DOCS",
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    assert_eq!(
        flow,
        SyscallFlow::ReadComplete {
            start: (MESSAGE_PAGE + 0x100) as u64,
            len: DIRENT_LEN,
        }
    );
    assert_eq!(source.readdirs, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"DOCS".as_slice()));
    assert_eq!(source.seen_readdir_index, Some(0));
    assert_eq!(
        DirEnt::from_le_bytes(scratch_buf[..DIRENT_LEN].try_into().unwrap()),
        dirent_fixture()
    );
}

// Catches the index argument not reaching the source: `a2` must
// select the entry inside the directory.
#[test]
fn readdir_passes_the_index_argument_to_the_source() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let _ = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        4,
        2,
        MESSAGE_PAGE,
        b"DOCS",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(source.seen_readdir_index, Some(2));
}

// Catches an empty path failing or reading a stray pointer: `a1`=0
// is the readdir-specific convention for the root directory and
// must reach the source as "".
#[test]
fn readdir_treats_an_empty_path_as_the_root_directory() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (_, flow) = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        0,
        0,
        MESSAGE_PAGE,
        b"ignored",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert!(matches!(flow, SyscallFlow::ReadComplete { .. }));
    assert_eq!(source.seen_path.as_deref(), Some(b"".as_slice()));
}

// Catches EOF not reaching the guest: an index past the end must
// return 0 in a0 like read's EOF, not a ReadComplete or an errno.
#[test]
fn readdir_returns_zero_when_the_index_passes_the_end() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    source.readdir_result = Ok(None);
    let (context, flow) = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        4,
        9,
        MESSAGE_PAGE,
        b"DOCS",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
}

// Catches an unwritable out pointer reaching the source or passing
// without EFAULT, and the source errno being swallowed.
#[test]
fn readdir_validates_the_out_pointer_and_passes_errnos_through() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        4,
        0,
        0,
        b"DOCS",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.readdirs, 0);

    let mut source = FileSource::serving(b"");
    source.readdir_result = Err(ENOTDIR);
    let (context, flow) = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        9,
        0,
        MESSAGE_PAGE,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOTDIR as usize);
    assert_eq!(source.readdirs, 1);
}

// Catches the default ControlSource::readdir leaking a successful
// result: without storage the guest must see ENOSYS.
#[test]
fn readdir_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, _) = dispatch_numbered_file_fixture(
        READDIR_NUMBER,
        MESSAGE_PAGE,
        4,
        0,
        MESSAGE_PAGE,
        b"DOCS",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), ENOSYS as usize);
}
