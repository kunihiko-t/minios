use super::*;

// Catches pipe failing to validate the out pointer or mangling the fd
// pair layout: the source must run only after EFAULT is decided, and
// the two fds must land as LE u32s in the user buffer.
#[test]
fn pipe_writes_the_fd_pair_to_the_out_pointer() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let mut scratch_buf = scratch();
    let (context, flow) = dispatch_fixture(
        PIPE_NUMBER,
        MESSAGE_PAGE,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    assert_eq!(
        flow,
        SyscallFlow::ReadComplete {
            start: MESSAGE_PAGE as u64,
            len: 8,
        }
    );
    assert_eq!(&scratch_buf[..8], &[3, 0, 0, 0, 4, 0, 0, 0]);
    assert_eq!(source.pipes, 1);
}

// Catches pipe allocating fds before validating the destination: an
// unwritable out pointer must return EFAULT without touching the source.
#[test]
fn pipe_rejects_an_unwritable_out_pointer_before_allocating() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"");
    let (context, flow) =
        dispatch_fixture(PIPE_NUMBER, 0, 0, 0, &mut sink, &mut source, &mut scratch());

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.pipes, 0);
}

// Catches a pipe-blocked read returning the errno instead of blocking:
// EAGAIN is the control layer's "caller already marked" signal and must
// rewind sepc to the ecall like the stdin not-ready path does.
#[test]
fn read_fd_eagain_blocks_and_rewinds_sepc() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::failing(minios_abi::syscall::EAGAIN);
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        16,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Blocked);
    assert_eq!(context.sepc(), 0x0010_0500 - 4);
    assert_eq!(source.fd_reads, 1);
}

// Catches a pipe-blocked write returning the errno instead of blocking:
// write_fd's EAGAIN must take the same rewind-and-reschedule path.
#[test]
fn write_fd_eagain_blocks_and_rewinds_sepc() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::failing(minios_abi::syscall::EAGAIN);
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        FIRST_FILE_FD,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Blocked);
    assert_eq!(context.sepc(), 0x0010_0500 - 4);
    assert_eq!(source.writes, 1);
}

// Catches missing frames, duplicated frames, wrong frame kinds, wrong
// payloads, or a return value other than the written byte count.
#[test]
fn write_stdout_and_stderr_deliver_one_frame_each_and_return_the_length() {
    for (descriptor, expected_kind) in [(STDOUT, FrameKind::Stdout), (STDERR, FrameKind::Stderr)] {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"untouched");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            descriptor,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(sink.frames, vec![(expected_kind, Vec::from(MESSAGE))]);
        assert_eq!(context.register(10), MESSAGE.len());
        assert_eq!(context.register(17), WRITE_NUMBER);
        assert_eq!(context.register(11), MESSAGE_PAGE);
        assert_eq!(context.register(12), MESSAGE.len());
        assert_eq!(context.register(8), 0x5150_0000_0000_0008);
        assert_eq!(source.reads, 0);
    }
}

// Catches accepting a descriptor other than 1 or 2.
#[test]
fn write_reports_unknown_descriptors_with_ebadf() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"");
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        FIRST_FILE_FD + MAX_OPEN_FILES,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EBADF as usize);
    assert!(sink.frames.is_empty());
}

// Catches copying more than one kernel-page worth of bytes.
#[test]
fn write_reports_oversized_lengths_with_einval() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"");
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        STDOUT,
        MESSAGE_PAGE,
        MAX_WRITE_LEN + 1,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EINVAL as usize);
    assert!(sink.frames.is_empty());
}

// Catches dereferencing an unmapped guest pointer or copying before the
// page walk completes.
#[test]
fn write_reports_guest_faults_with_efault() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"");
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        STDOUT,
        0x0,
        4,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert!(sink.frames.is_empty());
}

// Catches resuming a guest that called an unknown syscall without the
// ENOSYS return value.
#[test]
fn unknown_numbers_report_enosys_and_resume() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"");
    let (context, flow) = dispatch_fixture(999, 0, 0, 0, &mut sink, &mut source, &mut scratch());

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
    assert!(sink.frames.is_empty());
}

// Catches losing the exit code or treating exit as a resumable call.
#[test]
fn exit_returns_the_code_without_touching_the_context() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"untouched");
    let (context, flow) = dispatch_fixture(
        EXIT_NUMBER,
        42,
        0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Exit(42));
    assert_eq!(context.register(10), 42);
    assert!(sink.frames.is_empty());
    assert_eq!(source.reads, 0);
}

// Catches resuming after a sink failure or clobbering the guest context.
#[test]
fn sink_failure_is_fatal_and_preserves_the_context() {
    let mut sink = FakeSink {
        fail: true,
        ..FakeSink::default()
    };
    let mut source = FakeSource::scripted(b"");
    let (context, flow) = dispatch_fixture(
        WRITE_NUMBER,
        STDOUT,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Fatal(SinkError::Injected));
    assert_eq!(context.register(10), STDOUT);
    assert!(sink.frames.is_empty());
}
// Catches wiring mistakes between dispatch and the real Stdin frame parser.
#[test]
fn read_serves_real_stdin_frames_through_staging() {
    use crate::user::stdin::{ByteReader, StdinError, StdinStaging};

    struct VecReader<'a> {
        bytes: &'a [u8],
        position: usize,
    }

    impl ByteReader for VecReader<'_> {
        fn read_byte(&mut self) -> u8 {
            let byte = self.bytes[self.position];
            self.position += 1;
            byte
        }

        fn try_read_byte(&mut self) -> Option<u8> {
            self.bytes.get(self.position).copied().inspect(|_| {
                self.position += 1;
            })
        }
    }

    struct StagingSource<'a> {
        staging: StdinStaging,
        reader: VecReader<'a>,
    }

    impl ControlSource for StagingSource<'_> {
        type Error = SinkError;

        fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            match self.staging.read(&mut self.reader, output) {
                Ok(count) => Ok(Some(count)),
                Err(StdinError::WouldBlock) => Ok(None),
                Err(_) => Err(SinkError::Injected),
            }
        }
    }

    let header = |len: u32| {
        minios_abi::control::FrameHeader {
            kind: minios_abi::control::FrameKind::Stdin,
            payload_len: len,
        }
        .encode()
    };
    let mut stream = Vec::from(header(2));
    stream.extend_from_slice(b"ab");
    stream.extend_from_slice(&header(0));

    let mut sink = FakeSink::default();
    let mut source = StagingSource {
        staging: StdinStaging::new(),
        reader: VecReader {
            bytes: &stream,
            position: 0,
        },
    };
    let mut received = scratch();
    let (_, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        512,
        &mut sink,
        &mut source,
        &mut received,
    );
    let (_, len) = read_completion(flow);
    assert_eq!(len, 2);
    assert_eq!(&received[..len], b"ab");
}

// Catches losing received bytes, misreporting the destination, or setting
// a0 before the handler copies the bytes to the guest.
#[test]
fn read_delivers_source_bytes_as_read_complete() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"hello");
    let mut received = scratch();
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        b"hello".len(),
        &mut sink,
        &mut source,
        &mut received,
    );

    let (start, len) = read_completion(flow);
    assert_eq!(start, MESSAGE_PAGE as u64);
    assert_eq!(len, b"hello".len());
    assert_eq!(&received[..len], b"hello");
    assert_eq!(received[len], 0xaa);
    assert_eq!(context.register(10), STDIN);
    assert_eq!(source.reads, 1);
    assert!(sink.frames.is_empty());
}

// Catches treating an exhausted source as an error instead of EOF.
#[test]
fn read_reports_eof_with_zero_length() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"");
    let mut received = scratch();
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut received,
    );

    let (start, len) = read_completion(flow);
    assert_eq!(start, MESSAGE_PAGE as u64);
    assert_eq!(len, 0);
    assert_eq!(received[0], 0xaa);
    assert_eq!(context.register(10), STDIN);
    assert_eq!(source.reads, 1);
}

// Catches reading a console output entry or an out-of-range fd, or
// consuming input on EBADF.
#[test]
fn read_reports_unknown_descriptors_with_ebadf() {
    // 1,2はconsole出力のentry、FD_TABLE_LEN以降はfd範囲外で`EBADF`。
    for descriptor in [1, 2, super::FD_TABLE_LEN, 100] {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"hello");
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            descriptor,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
        assert_eq!(source.reads, 0);
        assert!(sink.frames.is_empty());
    }
}

// Catches copying more than one kernel-page worth of bytes.
#[test]
fn read_reports_oversized_lengths_with_einval() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"hello");
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        MAX_READ_LEN + 1,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EINVAL as usize);
    assert_eq!(source.reads, 0);
    assert!(sink.frames.is_empty());
}

// Catches consuming stdin before the destination range is validated.
#[test]
fn read_reports_guest_faults_with_efault_without_consuming() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"hello");
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        0x0,
        4,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.reads, 0);
    assert!(sink.frames.is_empty());
}

// Catches validating the pointer or touching the source for a zero read.
#[test]
fn read_with_zero_length_returns_zero_without_touching_the_source() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"hello");
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        0x0,
        0,
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.reads, 0);
    assert!(sink.frames.is_empty());
}

// Catches a blocked read consuming input state or forgetting to rewind
// sepc: with no input pending, dispatch must reach the source, get
// `Ok(None)`, return Blocked, leave a0 and the script position untouched,
// and point sepc back at the ecall so resume retries the same syscall.
#[test]
fn read_without_ready_input_blocks_and_rewinds_sepc() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::not_ready();
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Blocked);
    // classifyはsepcをecallの次 (+4) へ進めてからdispatchするため、
    // Blockedはその4 byte分だけ戻した位置を指す。
    assert_eq!(context.sepc(), 0x0010_0500 - 4);
    assert_eq!(context.register(10), STDIN);
    assert_eq!(source.reads, 1);
    assert_eq!(source.position, 0);
    assert!(sink.frames.is_empty());
}

// Catches the source call running before argument validation: an
// invalid read must keep reporting its error even when input is pending
// or absent, and must not mark the process blocked.
#[test]
fn read_validation_errors_precede_the_blocked_check() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::not_ready();
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDOUT,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EBADF as usize);
    assert_eq!(context.sepc(), 0x0010_0500);
}

// Catches a woken reader failing to complete: once input arrives, the
// retried ecall must run the normal read path and finish.
#[test]
fn read_completes_after_input_arrives() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::not_ready();
    let mut scratch_buf = scratch();
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );
    assert_eq!(flow, SyscallFlow::Blocked);

    source.ready = true;
    source.script = Vec::from(&b"hi"[..]);
    // 再開で同じecallが再実行され、sepcは再び+4された状態でdispatchへ来る。
    let mut context = context;
    context.set_sepc(context.sepc() + 4);
    let (context, flow) = dispatch_fixture_at(
        context,
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    let (start, len) = read_completion(flow);
    assert_eq!((start, len), (MESSAGE_PAGE as u64, 2));
    assert_eq!(context.register(10), STDIN);
}

// Catches resuming after a source failure or clobbering the guest context.
#[test]
fn source_failure_is_fatal_and_preserves_the_context() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource {
        fail: true,
        ..FakeSource::scripted(b"hello")
    };
    let (context, flow) = dispatch_fixture(
        READ_NUMBER,
        STDIN,
        MESSAGE_PAGE,
        MESSAGE.len(),
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::SourceFatal(SinkError::Injected));
    assert_eq!(context.register(10), STDIN);
    assert!(sink.frames.is_empty());
}

// Catches missing read_file dispatch, path truncation, a missing
// ReadComplete flow, or a byte count larger than the buffer.
#[test]
fn read_file_streams_into_scratch_and_reports_read_complete() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"file data");
    let buf_start = MESSAGE_PAGE + 512;
    let mut scratch_buf = scratch();
    let (context, flow) = dispatch_file_fixture(
        MESSAGE_PAGE,
        b"HELLO.TXT".len(),
        buf_start,
        64,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    let (start, len) = read_completion(flow);
    assert_eq!((start, len), (buf_start as u64, 9));
    assert_eq!(&scratch_buf[..9], b"file data");
    assert_eq!(source.seen_path.as_deref(), Some(b"HELLO.TXT".as_slice()));
    assert_eq!(source.reads, 1);
    assert_eq!(context.register(10), MESSAGE_PAGE);
}

// Catches reads exceeding the guest buffer not being truncated.
#[test]
fn read_file_truncates_the_stream_at_the_guest_buffer() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(&[0x5a; 8192]);
    let buf_start = MESSAGE_PAGE + 512;
    let mut scratch_buf = scratch();
    let (_context, flow) = dispatch_file_fixture(
        MESSAGE_PAGE,
        b"HELLO.TXT".len(),
        buf_start,
        100,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch_buf,
    );

    let (start, len) = read_completion(flow);
    assert_eq!((start, len), (buf_start as u64, 100));
    assert!(scratch_buf[..100].iter().all(|b| *b == 0x5a));
}

// Catches storage errors reaching the guest as errno rather than
// aborting the process or returning a partial count.
#[test]
fn read_file_maps_source_errors_to_errno() {
    for errno in [ENOENT, ENOTDIR, EISDIR, ENODEV] {
        let mut sink = FakeSink::default();
        let mut source = FileSource::failing(errno);
        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            b"HELLO.TXT".len(),
            MESSAGE_PAGE + 512,
            64,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), errno as usize);
    }
}

// Catches missing argument validation: an empty or overlong path and a
// buffer beyond MAX_READ_LEN must be EINVAL before any source call.
#[test]
fn read_file_rejects_bad_arguments_before_touching_the_source() {
    for (a1, a3) in [(0, 64), (MAX_PATH_LEN + 1, 64), (9, MAX_READ_LEN + 1)] {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file data");
        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            a1,
            MESSAGE_PAGE + 512,
            a3,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EINVAL as usize);
        assert_eq!(source.reads, 0);
    }

    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"file data");
    let (context, flow) = dispatch_file_fixture(
        MESSAGE_PAGE,
        9,
        MESSAGE_PAGE + 512,
        0,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
    assert_eq!(source.reads, 0);
}

// Catches EFAULT not being reported for an unreadable path pointer or an
// unwritable destination range, and the source being touched anyway.
#[test]
fn read_file_reports_efault_for_bad_user_ranges() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"file data");
    let (context, flow) = dispatch_file_fixture(
        0x40_000,
        9,
        MESSAGE_PAGE + 512,
        64,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.reads, 0);

    let (context, flow) = dispatch_file_fixture(
        MESSAGE_PAGE,
        9,
        0x40_000,
        64,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EFAULT as usize);
    assert_eq!(source.reads, 0);
}

// Catches a non-UTF-8 path being passed to the filesystem rather than
// rejected with EINVAL.
#[test]
fn read_file_rejects_a_non_utf8_path() {
    let mut sink = FakeSink::default();
    let mut source = FileSource::serving(b"file data");
    let (context, flow) = dispatch_file_fixture(
        MESSAGE_PAGE,
        3,
        MESSAGE_PAGE + 512,
        64,
        b"A\xffB",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), EINVAL as usize);
    assert_eq!(source.reads, 0);
}

// Catches the default ControlSource::read_file implementation leaking a
// successful result: without storage the guest must see ENOSYS.
#[test]
fn read_file_without_storage_returns_enosys() {
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"stdin only");
    let (context, flow) = dispatch_file_fixture(
        MESSAGE_PAGE,
        9,
        MESSAGE_PAGE + 512,
        64,
        b"HELLO.TXT",
        &mut sink,
        &mut source,
        &mut scratch(),
    );

    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), ENOSYS as usize);
    assert_eq!(source.reads, 0);
}
