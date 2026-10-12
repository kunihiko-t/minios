use super::*;

// Catches open not forwarding the path, the returned fd, or the source's
// errno, and path validation being skipped.
#[test]
fn open_returns_the_allocated_fd_and_validates_the_path() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"file data");
    let (context, flow) = dispatch_numbered_file_fixture(
        OPEN_NUMBER,
        MESSAGE_PAGE,
        b"HELLO.TXT".len(),
        0,
        0,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), FIRST_FILE_FD);
    assert_eq!(source.seen_path.as_deref(), Some(b"HELLO.TXT".as_slice()));

    for (a0, a1, content, expected) in [
        (MESSAGE_PAGE, 0, b"HELLO.TXT".as_slice(), EINVAL),
        (
            MESSAGE_PAGE,
            MAX_PATH_LEN + 1,
            b"HELLO.TXT".as_slice(),
            EINVAL,
        ),
        (0x40_000, 9, b"HELLO.TXT".as_slice(), EFAULT),
        (MESSAGE_PAGE, 3, b"A\xffB".as_slice(), EINVAL),
    ] {
        let mut source = FileSource::serving(b"file data");
        let (context, flow) = dispatch_numbered_file_fixture(
            OPEN_NUMBER,
            a0,
            a1,
            0,
            0,
            content,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), expected as usize);
        assert!(source.seen_path.is_none());
    }

    let mut source = FileSource::failing(ENOENT);
    source.open_fd = Err(ENOENT);
    let (context, flow) = dispatch_numbered_file_fixture(
        OPEN_NUMBER,
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
    assert_eq!(context.register(10), ENOENT as usize);
}

// Catches read on a file fd not reaching read_fd, not truncating at the
// buffer, or swallowing source errno.
#[test]
fn read_on_a_file_fd_streams_via_read_fd() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"file contents");
    let buf_start = MESSAGE_PAGE + 512;
    let mut scratch_buf = scratch();
    let (_context, flow) = dispatch_fixture(
        READ_NUMBER,
        FIRST_FILE_FD,
        buf_start,
        64,
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    let (start, len) = read_completion(flow);
    assert_eq!((start, len), (buf_start as u64, 13));
    assert_eq!(&scratch_buf[..13], b"file contents");
    assert_eq!(source.fd_reads, 1);
    assert_eq!(source.reads, 0);

    let mut source = FileSource::failing(EBADF);
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        FIRST_FILE_FD,
        buf_start,
        64,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EBADF as usize);
}

// Catches close rejecting fds outside the table before the source, and
// forwarding every in-range fd (0..2 included, since they are ordinary
// slots now), including the source's EBADF for unallocated slots.
#[test]
fn close_rejects_out_of_range_fds_and_forwards_the_rest() {
    let mut sink = FakeSink::default();
    for fd in [STDIN, STDOUT, STDERR] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            CLOSE_NUMBER,
            fd,
            0,
            0,
            0,
            b"",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        // FileSourceはFIRST_FILE_FD以外を未割当として扱う。
        assert_eq!(context.register(10), EBADF as usize);
        assert_eq!(source.closes, 1);
    }
    for fd in [super::FD_TABLE_LEN, 100] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            CLOSE_NUMBER,
            fd,
            0,
            0,
            0,
            b"",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
        assert_eq!(source.closes, 0);
    }

    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        CLOSE_NUMBER,
        FIRST_FILE_FD,
        0,
        0,
        0,
        b"",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.closes, 1);

    let (context, flow) = dispatch_numbered_file_fixture(
        CLOSE_NUMBER,
        FIRST_FILE_FD + 1,
        0,
        0,
        0,
        b"",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EBADF as usize);
    assert_eq!(source.closes, 2);
}

// Catches write/read routing by fd number instead of by the entry:
// fd 1 redirected away from the console must reach write_fd, a console
// entry moved to fd 5 must still reach the sink, and dup2 must reject
// out-of-range fds before the source.
#[test]
fn console_routing_follows_the_entry_and_dup2_checks_the_range() {
    struct Redirected {
        writes: usize,
        dups: usize,
    }
    impl ControlSource for Redirected {
        type Error = SinkError;
        fn read_stdin(&mut self, _: &mut [u8]) -> Result<Option<usize>, SinkError> {
            Ok(None)
        }
        fn console_fd(&mut self, fd: usize) -> Option<super::ConsoleFd> {
            (fd == 5).then_some(super::ConsoleFd::Stdout)
        }
        fn write_fd(&mut self, _: usize, data: &[u8]) -> Result<usize, isize> {
            self.writes += 1;
            Ok(data.len())
        }
        fn dup2(&mut self, _: usize, newfd: usize) -> Result<usize, isize> {
            self.dups += 1;
            Ok(newfd)
        }
    }

    let mut sink = FakeSink::default();
    let mut source = Redirected { writes: 0, dups: 0 };
    for fd in [STDOUT, 5] {
        let (context, _) = dispatch_fixture(
            WRITE_NUMBER,
            fd,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(context.register(10), MESSAGE.len());
    }
    assert_eq!(source.writes, 1);
    assert_eq!(sink.frames.len(), 1);

    let dup2 = SyscallNumber::Dup2 as usize;
    for (oldfd, newfd, expected) in [
        (super::FD_TABLE_LEN, 1, EBADF as usize),
        (1, super::FD_TABLE_LEN, EBADF as usize),
        (3, 1, 1),
    ] {
        let (context, _) = dispatch_fixture(
            dup2,
            oldfd,
            newfd,
            0,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(context.register(10), expected);
    }
    assert_eq!(source.dups, 1);
}

// Catches create routing to create_file rather than open_file, so a
// writable fd never reaches a guest that only asked to read.
#[test]
fn create_routes_to_create_file_and_validates_the_path() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        CREATE_NUMBER,
        MESSAGE_PAGE,
        b"NEW.TXT".len(),
        0,
        0,
        b"NEW.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), FIRST_FILE_FD);
    assert_eq!(source.creates, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"NEW.TXT".as_slice()));

    // path検証はopenと同じく、sourceへ触れる前にerrnoを確定する。
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        CREATE_NUMBER,
        0x40_000,
        9,
        0,
        0,
        b"NEW.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.creates, 0);
}

// Catches unlink not forwarding the path, not returning 0 on success,
// or swallowing the source's errno, and path validation being skipped.
#[test]
fn unlink_deletes_via_the_source_and_validates_the_path() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        UNLINK_NUMBER,
        MESSAGE_PAGE,
        b"OLD.TXT".len(),
        0,
        0,
        b"OLD.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.unlinks, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"OLD.TXT".as_slice()));

    for (a0, a1, content, expected) in [
        (MESSAGE_PAGE, 0, b"OLD.TXT".as_slice(), EINVAL),
        (
            MESSAGE_PAGE,
            MAX_PATH_LEN + 1,
            b"OLD.TXT".as_slice(),
            EINVAL,
        ),
        (0x40_000, 7, b"OLD.TXT".as_slice(), EFAULT),
        (MESSAGE_PAGE, 3, b"A\xffB".as_slice(), EINVAL),
    ] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            UNLINK_NUMBER,
            a0,
            a1,
            0,
            0,
            content,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), expected as usize);
        assert_eq!(source.unlinks, 0);
    }

    let mut source = FileSource::failing(ENOENT);
    let (context, flow) = dispatch_numbered_file_fixture(
        UNLINK_NUMBER,
        MESSAGE_PAGE,
        7,
        0,
        0,
        b"OLD.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOENT as usize);
    assert_eq!(source.unlinks, 1);
}

// Catches rename dropping one of the paths, forwarding them in the
// wrong order, or running the source before validating both buffers:
// both EFAULT/EINVAL results must be decided before any rename.
#[test]
fn rename_forwards_both_paths_and_validates_them_first() {
    let mut sink = FakeSink::default();
    let mut page = [0u8; 512];
    page[..7].copy_from_slice(b"OLD.TXT");
    page[256..263].copy_from_slice(b"NEW.TXT");
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        RENAME_NUMBER,
        MESSAGE_PAGE,
        7,
        MESSAGE_PAGE + 256,
        7,
        &page,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.renames, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"OLD.TXT".as_slice()));
    assert_eq!(source.seen_new_path.as_deref(), Some(b"NEW.TXT".as_slice()));

    // 検証で落ちる組合せはsourceへ届かない。target側のEFAULTも
    // source側のcopyと同じく呼び出し前に確定する。
    for (a0, a1, a2, a3, content, expected) in [
        (
            MESSAGE_PAGE,
            0,
            MESSAGE_PAGE + 256,
            7,
            page.as_slice(),
            EINVAL,
        ),
        (
            MESSAGE_PAGE,
            MAX_PATH_LEN + 1,
            MESSAGE_PAGE + 256,
            7,
            page.as_slice(),
            EINVAL,
        ),
        (0x40_000, 7, MESSAGE_PAGE + 256, 7, page.as_slice(), EFAULT),
        (
            MESSAGE_PAGE,
            7,
            MESSAGE_PAGE + 256,
            0,
            page.as_slice(),
            EINVAL,
        ),
        (MESSAGE_PAGE, 7, 0x40_000, 7, page.as_slice(), EFAULT),
    ] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            RENAME_NUMBER,
            a0,
            a1,
            a2,
            a3,
            content,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), expected as usize);
        assert_eq!(source.renames, 0);
    }

    // 非UTF-8と、source側のerrnoもそのまま`a0`へ返る。
    let mut bad = [0u8; 512];
    bad[..3].copy_from_slice(b"A\xffB");
    bad[256..263].copy_from_slice(b"NEW.TXT");
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        RENAME_NUMBER,
        MESSAGE_PAGE,
        3,
        MESSAGE_PAGE + 256,
        7,
        &bad,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EINVAL as usize);
    assert_eq!(source.renames, 0);

    let mut source = FileSource::failing(ENOENT);
    let (context, _) = dispatch_numbered_file_fixture(
        RENAME_NUMBER,
        MESSAGE_PAGE,
        7,
        MESSAGE_PAGE + 256,
        7,
        &page,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), ENOENT as usize);
    assert_eq!(source.renames, 1);
}

// Catches the default ControlSource::rename implementation leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn rename_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let mut page = [0u8; 512];
    page[..7].copy_from_slice(b"OLD.TXT");
    page[256..263].copy_from_slice(b"NEW.TXT");
    let (context, flow) = dispatch_numbered_file_fixture(
        RENAME_NUMBER,
        MESSAGE_PAGE,
        7,
        MESSAGE_PAGE + 256,
        7,
        &page,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches mkdir/rmdir not reaching the source, or the path being
// copied wrong: both share the single-path dispatch contract.
#[test]
fn mkdir_and_rmdir_reach_the_source_with_the_path() {
    let mut sink = FakeSink::default();
    for (number, counter) in [(MKDIR_NUMBER, "mkdirs"), (RMDIR_NUMBER, "rmdirs")] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            number,
            MESSAGE_PAGE,
            6,
            0,
            0,
            b"NEWDIR",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), 0);
        assert_eq!(source.seen_path.as_deref(), Some(b"NEWDIR".as_slice()));
        match counter {
            "mkdirs" => assert_eq!((source.mkdirs, source.rmdirs), (1, 0)),
            _ => assert_eq!((source.mkdirs, source.rmdirs), (0, 1)),
        }
    }

    // path検証で落ちる場合と、sourceのerrnoもそのまま`a0`へ返る。
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        MKDIR_NUMBER,
        0x40_000,
        6,
        0,
        0,
        b"NEWDIR",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.mkdirs, 0);

    let mut source = FileSource::failing(ENOTEMPTY);
    let (context, _) = dispatch_numbered_file_fixture(
        RMDIR_NUMBER,
        MESSAGE_PAGE,
        6,
        0,
        0,
        b"NEWDIR",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), ENOTEMPTY as usize);
    assert_eq!(source.rmdirs, 1);
}

// Catches the default ControlSource::make_dir/remove_dir leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn mkdir_and_rmdir_without_storage_return_enosys() {
    let mut sink = FakeSink::default();
    for number in [MKDIR_NUMBER, RMDIR_NUMBER] {
        let mut source = FakeSource::scripted(b"stdin only");
        let (context, flow) = dispatch_numbered_file_fixture(
            number,
            MESSAGE_PAGE,
            6,
            0,
            0,
            b"NEWDIR",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOSYS as usize);
    }
}
