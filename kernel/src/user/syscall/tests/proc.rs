use super::*;

// Catches sbrk losing the increment's sign on the way to the source or
// writing anything but the old break / errno into a0, and the default
// source leaking success outside a process context.
#[test]
fn sbrk_passes_a_signed_increment_and_returns_the_source_result() {
    struct SbrkSource {
        seen: Option<isize>,
        result: Result<u64, isize>,
    }
    impl ControlSource for SbrkSource {
        type Error = SinkError;
        fn read_stdin(&mut self, _output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            Ok(Some(0))
        }
        fn sbrk(&mut self, increment: isize) -> Result<u64, isize> {
            self.seen = Some(increment);
            self.result
        }
    }

    let number = SyscallNumber::Sbrk as usize;
    let mut sink = FakeSink::default();
    let mut source = SbrkSource {
        seen: None,
        result: Ok(0x20_1000),
    };
    let (context, flow) =
        dispatch_fixture(number, 4096, 0, 0, &mut sink, &mut source, &mut scratch());
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(source.seen, Some(4096));
    assert_eq!(context.register(10), 0x20_1000);

    source.result = Err(EINVAL);
    let (context, _) = dispatch_fixture(
        number,
        -1isize as usize,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(source.seen, Some(-1));
    assert_eq!(context.register(10), EINVAL as usize);

    let mut fake = FakeSource::scripted(b"");
    let (context, _) = dispatch_fixture(number, 0, 0, 0, &mut sink, &mut fake, &mut scratch());
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches clock/sleep/yield wiring: clock passes the source value
// through, sleep(0) and yield must not touch the source, sleep(n) must
// mark via the source and return Yield with a0=0 (not rewind to re-sleep),
// and a source error must come back as errno without yielding.
#[test]
fn clock_sleep_and_yield_return_the_documented_flows() {
    struct TimeSource {
        slept: Option<usize>,
        result: Result<(), isize>,
    }
    impl ControlSource for TimeSource {
        type Error = SinkError;
        fn read_stdin(&mut self, _output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            Ok(Some(0))
        }
        fn clock(&mut self) -> isize {
            1234
        }
        fn sleep(&mut self, millis: usize) -> Result<(), isize> {
            self.slept = Some(millis);
            self.result
        }
    }
    let mut sink = FakeSink::default();
    let mut source = TimeSource {
        slept: None,
        result: Ok(()),
    };
    let clock = SyscallNumber::Clock as usize;
    let sleep = SyscallNumber::Sleep as usize;
    let yield_ = SyscallNumber::Yield as usize;

    let (context, flow) = dispatch_fixture(clock, 0, 0, 0, &mut sink, &mut source, &mut scratch());
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 1234);

    for (number, a0) in [(yield_, 9), (sleep, 0)] {
        let (context, flow) =
            dispatch_fixture(number, a0, 0, 0, &mut sink, &mut source, &mut scratch());
        assert_eq!(flow, SyscallFlow::Yield);
        assert_eq!(context.register(10), 0);
        assert_eq!(source.slept, None);
    }

    let (context, flow) = dispatch_fixture(sleep, 50, 0, 0, &mut sink, &mut source, &mut scratch());
    assert_eq!(flow, SyscallFlow::Yield);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.slept, Some(50));

    source.result = Err(ENOSYS);
    let (context, flow) = dispatch_fixture(sleep, 50, 0, 0, &mut sink, &mut source, &mut scratch());
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches getpid writing anything other than the source's pid into a0.
#[test]
fn getpid_returns_the_source_pid() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_fixture(
        GETPID_NUMBER,
        0,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 7);
}

// Catches the default ControlSource::getpid leaking a successful result:
// outside a process context the guest must see ENOSYS.
#[test]
fn getpid_without_process_context_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_fixture(
        GETPID_NUMBER,
        0,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches spawn reaching the source with the copied path and the child pid
// landing in a0; also covers path validation before source side effects.
#[test]
fn spawn_reaches_the_source_with_the_path() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
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
    assert_eq!(context.register(10), 11);
    assert_eq!(source.spawns, 1);
    assert_eq!(source.seen_path.as_deref(), Some(b"CHILD.ELF".as_slice()));

    // path検証で落ちる場合はsourceへ届かない。
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
        0x40_000,
        9,
        0,
        0,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.spawns, 0);

    // sourceのerrnoはそのまま`a0`へ返る。
    let mut source = FileSource::serving(b"");
    source.spawn_result = Err(ENOENT);
    let (context, _) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
        MESSAGE_PAGE,
        9,
        0,
        0,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), ENOENT as usize);
    assert_eq!(source.spawns, 1);
}

// Catches the default ControlSource::spawn leaking a successful result:
// without storage the guest must see ENOSYS.
#[test]
fn spawn_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
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

/// spawn検査用のMESSAGE page内容。offset 0に`CHILD.ELF`、
/// `SPAWN_TABLE`にargv entry列、`strings`の各`(offset, bytes)`を置く。
const SPAWN_TABLE: usize = 64;

fn spawn_page(entries: &[(u64, u64)], strings: &[(usize, &[u8])]) -> Vec<u8> {
    let mut page = vec![0u8; PAGE_SIZE];
    page[..9].copy_from_slice(b"CHILD.ELF");
    for (index, (pointer, len)) in entries.iter().enumerate() {
        let at = SPAWN_TABLE + index * 16;
        page[at..at + 8].copy_from_slice(&pointer.to_le_bytes());
        page[at + 8..at + 16].copy_from_slice(&len.to_le_bytes());
    }
    for (offset, bytes) in strings {
        page[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    page
}

fn dispatch_spawn_page(page: &[u8], argc: usize) -> (usize, FileSource) {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
        MESSAGE_PAGE,
        9,
        MESSAGE_PAGE + SPAWN_TABLE,
        argc,
        page,
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    (context.register(10), source)
}

// Catches spawn dropping, reordering or truncating argv strings, and the
// zero-argc path reading a2: argc=0 must reach the source with an empty
// argv even when a2 is garbage, so old callers keep today's child argv.
#[test]
fn spawn_copies_argv_entries_in_order() {
    let base = MESSAGE_PAGE as u64;
    let page = spawn_page(
        &[(base + 256, 8), (base + 300, 5), (base + 320, 10)],
        &[(256, b"echoargs"), (300, b"alpha"), (320, b"beta gamma")],
    );
    let (a0, source) = dispatch_spawn_page(&page, 3);
    assert_eq!(a0, 11);
    assert_eq!(source.seen_argv, ["echoargs", "alpha", "beta gamma"]);

    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
        MESSAGE_PAGE,
        9,
        0x40_000,
        0,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), 11);
    assert_eq!(source.spawns, 1);
    assert!(source.seen_argv.is_empty());
}

// Catches argv limits drifting from the manifest's or an invalid argv
// reaching the source: every rejection must leave spawns at 0, and the
// exact limits (SPAWN_MAX_ARGC entries, ARG_MAX_LEN bytes) must pass.
#[test]
fn spawn_rejects_invalid_argv_before_the_source() {
    let base = MESSAGE_PAGE as u64;
    let long = [b'a'; ARG_MAX_LEN + 1];
    let full = [(base + 1024, 1); SPAWN_MAX_ARGC + 1];

    let page = spawn_page(&full, &[(1024, b"x")]);
    let (a0, source) = dispatch_spawn_page(&page, SPAWN_MAX_ARGC);
    assert_eq!(a0, 11);
    assert_eq!(source.seen_argv.len(), SPAWN_MAX_ARGC);
    let (a0, source) = dispatch_spawn_page(&page, SPAWN_MAX_ARGC + 1);
    assert_eq!(a0, EINVAL as usize);
    assert_eq!(source.spawns, 0);

    let page = spawn_page(&[(base + 1024, ARG_MAX_LEN as u64)], &[(1024, &long)]);
    assert_eq!(dispatch_spawn_page(&page, 1).0, 11);

    let cases: [(&[(u64, u64)], &[(usize, &[u8])], isize); 5] = [
        // 長すぎる文字列。
        (
            &[(base + 1024, ARG_MAX_LEN as u64 + 1)],
            &[(1024, &long)],
            EINVAL,
        ),
        // 未mapの文字列pointer。2個目で落ちても1個目は渡らない。
        (&[(base + 1024, 1), (0x40_000, 1)], &[(1024, b"x")], EFAULT),
        // page末尾をまたいで未mapへ出る文字列。
        (&[(base + 4095, 2)], &[], EFAULT),
        // NULを含む文字列。
        (&[(base + 1024, 3)], &[(1024, b"a\0b")], EINVAL),
        // UTF-8でない文字列。
        (&[(base + 1024, 1)], &[(1024, &[0xff])], EINVAL),
    ];
    for (entries, strings, errno) in cases {
        let page = spawn_page(entries, strings);
        let (a0, source) = dispatch_spawn_page(&page, entries.len());
        assert_eq!(a0, errno as usize);
        assert_eq!(source.spawns, 0);
    }

    // entry列自体が読めない。
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, _) = dispatch_numbered_file_fixture(
        SPAWN_NUMBER,
        MESSAGE_PAGE,
        9,
        0x40_000,
        1,
        b"CHILD.ELF",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.spawns, 0);
}

// Catches waitpid dropping the reaped code or the waited pid: a
// recorded exit must land in a0 after one source call with a0's pid.
#[test]
fn waitpid_returns_the_reaped_exit_code() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) = dispatch_fixture(
        WAITPID_NUMBER,
        3,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 42);
    assert_eq!(source.waitpids, 1);
    assert_eq!(source.seen_wait_pid, Some(3));
}

// Catches a live-target wait resuming instead of blocking, or blocking
// without rewinding sepc: Ok(None) must return Blocked and point sepc
// back at the ecall so the retry re-runs the same waitpid.
#[test]
fn waitpid_on_a_live_target_blocks_and_rewinds_sepc() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    source.waitpid_result = Ok(None);
    let (context, flow) = dispatch_fixture(
        WAITPID_NUMBER,
        3,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Blocked);
    assert_eq!(context.sepc(), 0x0010_0500 - 4);
    assert_eq!(context.register(10), 3);
}

// Catches waitpid swallowing the source errno: ECHILD/EINVAL must pass
// through to a0 unchanged.
#[test]
fn waitpid_passes_the_source_errno_through() {
    use minios_abi::syscall::ECHILD;

    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    source.waitpid_result = Err(ECHILD);
    let (context, flow) = dispatch_fixture(
        WAITPID_NUMBER,
        99,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ECHILD as usize);
    assert_eq!(source.seen_wait_pid, Some(99));
}

// Catches the default ControlSource::waitpid leaking a successful
// result: without a process context the guest must see ENOSYS.
#[test]
fn waitpid_without_process_context_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_fixture(
        WAITPID_NUMBER,
        1,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}

// Catches kill dropping the result or resuming differently: an accepted
// request must write 0 to a0, pass a0's pid to the source once, and let
// the caller keep running without blocking.
#[test]
fn kill_returns_zero_and_keeps_the_caller_running() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) =
        dispatch_fixture(KILL_NUMBER, 3, 0, 0, &mut sink, &mut source, &mut scratch());

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.kills, 1);
    assert_eq!(source.seen_kill_pid, Some(3));
}

// Catches kill swallowing the source errno: ECHILD/EINVAL must pass
// through to a0 unchanged.
#[test]
fn kill_passes_the_source_errno_through() {
    use minios_abi::syscall::ECHILD;

    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    source.kill_result = Err(ECHILD);
    let (context, flow) = dispatch_fixture(
        KILL_NUMBER,
        99,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ECHILD as usize);
    assert_eq!(source.seen_kill_pid, Some(99));
}

// Catches the default ControlSource::kill leaking a success: without a
// process context the guest must see ENOSYS.
#[test]
fn kill_without_process_context_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) =
        dispatch_fixture(KILL_NUMBER, 1, 0, 0, &mut sink, &mut source, &mut scratch());

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
}
