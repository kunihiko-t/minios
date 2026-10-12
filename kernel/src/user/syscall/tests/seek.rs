use super::*;

// Catches exec resuming into the old image or skipping the slot write:
// a hit must overwrite the trap context with the new initial frame —
// new entry in sepc, a fresh user stack in sp — and return Exec so the
// run loop knows a retired image is waiting to be freed.
#[test]
fn exec_rewrites_the_trap_context_and_returns_the_exec_flow() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        EXEC_NUMBER,
        MESSAGE_PAGE,
        9,
        0,
        0,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Exec);
    assert_eq!(source.execs, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"CHILD.ELF".as_slice()));
    assert_eq!(context.sepc(), 0x40_0000);
    assert_eq!(context.register(2), 0x40_7000);
}

// Catches exec reaching the source after a bad path pointer: EFAULT
// must win over every storage or image side effect, and the source
// errno must reach a0 unchanged while the context stays intact.
#[test]
fn exec_validates_the_path_and_passes_errnos_through() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        EXEC_NUMBER,
        0x40_000,
        9,
        0,
        0,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.execs, 0);

    let mut source = FileSource::serving(b"");
    source.exec_result = Err(ENOENT);
    let (context, flow) = dispatch_numbered_file_fixture(
        EXEC_NUMBER,
        MESSAGE_PAGE,
        7,
        0,
        0,
        b"MISSING",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOENT as usize);
    assert_eq!(source.execs, 1);
    // 失敗時はcontextを書き換えず、旧imageのままecallの次へ戻る。
    assert_ne!(context.sepc(), 0x40_0000);
}

// Catches the default ControlSource::exec leaking a successful result:
// without storage the guest must see ENOSYS and keep running.
#[test]
fn exec_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_numbered_file_fixture(
        EXEC_NUMBER,
        MESSAGE_PAGE,
        9,
        0,
        0,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches the default ControlSource::unlink implementation leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn unlink_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
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
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches lseek dropping the signed offset or whence on the floor,
// accepting non-file descriptors, or swallowing the source's errno.
#[test]
fn lseek_routes_the_signed_offset_and_validates_the_fd() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_fixture(
        LSEEK_NUMBER,
        FIRST_FILE_FD,
        (-3isize) as usize,
        1,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 8);
    assert_eq!(source.seeks, 1);
    assert_eq!(source.seen_seek, Some((-3, 1)));

    // console entryはESPIPE、範囲外fdはEBADFで、sourceへ届かない。
    for (fd, expected) in [
        (STDIN, ESPIPE),
        (STDOUT, ESPIPE),
        (STDERR, ESPIPE),
        (FIRST_FILE_FD + MAX_OPEN_FILES, EBADF),
    ] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_fixture(
            LSEEK_NUMBER,
            fd,
            0,
            0,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), expected as usize);
        assert_eq!(source.seeks, 0);
    }

    let mut source = FileSource::serving(b"");
    source.seek_result = Err(EINVAL);
    let (context, flow) = dispatch_fixture(
        LSEEK_NUMBER,
        FIRST_FILE_FD,
        usize::MAX,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EINVAL as usize);
}

// Catches the default ControlSource::seek_fd implementation leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn lseek_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_fixture(
        LSEEK_NUMBER,
        FIRST_FILE_FD,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches pread ignoring a3's explicit offset, not capping the count,
// or letting reads through on non-file descriptors.
#[test]
fn pread_reads_at_the_explicit_offset() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"0123456789");
    let mut read_scratch = scratch();
    let (context, flow) = dispatch_numbered_file_fixture(
        PREAD_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        8,
        4,
        b"",
        &mut sink,
        &mut source,
        &mut read_scratch,
    );

    assert_eq!(
        flow,
        SyscallFlow::ReadComplete {
            start: MESSAGE_PAGE as u64,
            len: 8
        }
    );
    assert_eq!(source.preads, 1);
    assert_eq!(source.seen_offset, Some(4));
    assert_eq!(&read_scratch[..8], b"01234567");
    // a0はcopy-out経路が長さを書くまでfdのままである。
    assert_eq!(context.register(10), FIRST_FILE_FD);
}

// Catches pread skipping descriptor/length/buffer validation or letting
// EFAULT leak past the storage side effect.
#[test]
fn pread_validates_the_descriptor_length_and_buffer() {
    let mut sink = FakeSink::default();

    // fd検証・len検証・buf検証をそれぞれ個別に確認する。
    for (fd, len, buf, expected) in [
        (STDIN, 8usize, MESSAGE_PAGE, ESPIPE),
        (FIRST_FILE_FD + MAX_OPEN_FILES, 8, MESSAGE_PAGE, EBADF),
        (FIRST_FILE_FD, MAX_READ_LEN + 1, MESSAGE_PAGE, EINVAL),
        (FIRST_FILE_FD, 8, 0x40_000usize, EFAULT),
    ] {
        let mut source = FileSource::serving(b"0123456789");
        let (context, flow) = dispatch_numbered_file_fixture(
            PREAD_NUMBER,
            fd,
            buf,
            len,
            0,
            b"",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), expected as usize);
        assert_eq!(source.preads, 0);
    }

    // len==0はsourceを呼ばず0を返す。
    let mut source = FileSource::serving(b"0123456789");
    let (context, flow) = dispatch_numbered_file_fixture(
        PREAD_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        0,
        0,
        b"",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.preads, 0);
}

// Catches the default ControlSource::pread_fd implementation leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn pread_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_numbered_file_fixture(
        PREAD_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        4,
        0,
        b"",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches pwrite ignoring a3's explicit offset or writing through
// non-file descriptors.
#[test]
fn pwrite_writes_at_the_explicit_offset() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        PWRITE_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        MESSAGE.len(),
        2,
        MESSAGE,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), MESSAGE.len());
    assert_eq!(source.pwrites, 1);
    assert_eq!(source.seen_offset, Some(2));
    assert_eq!(source.written, MESSAGE);
}

// Catches pwrite skipping descriptor/length/buffer validation or letting
// EFAULT leak past the storage side effect.
#[test]
fn pwrite_validates_the_descriptor_length_and_buffer() {
    let mut sink = FakeSink::default();

    for (fd, len, buf, expected) in [
        (STDIN, 3usize, MESSAGE_PAGE, ESPIPE),
        (FIRST_FILE_FD + MAX_OPEN_FILES, 3, MESSAGE_PAGE, EBADF),
        (FIRST_FILE_FD, MAX_WRITE_LEN + 1, MESSAGE_PAGE, EINVAL),
        (FIRST_FILE_FD, 3, 0x40_000usize, EFAULT),
    ] {
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            PWRITE_NUMBER,
            fd,
            buf,
            len,
            0,
            MESSAGE,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), expected as usize);
        assert_eq!(source.pwrites, 0);
    }

    let mut source = FileSource::failing(EINVAL);
    let (context, flow) = dispatch_numbered_file_fixture(
        PWRITE_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        3,
        10,
        MESSAGE,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EINVAL as usize);
    assert_eq!(source.pwrites, 1);
}

// Catches the default ControlSource::pwrite_fd implementation leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn pwrite_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_numbered_file_fixture(
        PWRITE_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        3,
        0,
        MESSAGE,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches write on a file fd reaching neither write_fd nor the errno
// path, and frame output never leaking to fd 3+.
#[test]
fn write_on_a_file_fd_routes_to_write_fd() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), MESSAGE.len());
    assert_eq!(source.writes, 1);
    assert_eq!(source.written, MESSAGE);
    assert!(sink.frames.is_empty());

    // sourceのerrnoはそのままa0へ返る。
    let mut source = FileSource::failing(EBADF);
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EBADF as usize);

    // 検証できないuser pointerはsourceへ届く前にEFAULT。
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        FIRST_FILE_FD,
        0x40_000,
        64,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.writes, 0);
}
