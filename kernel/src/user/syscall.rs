//! `write`、`read`、`exit`、未知のsystem callを純粋なdispatch結果へ変換する。

use crate::{
    user::{
        context::UserContext,
        memory::{check_user_writable_range, copy_from_user},
    },
    vm::{AddressSpace, FrameStore},
};
use minios_abi::{
    control::FrameKind,
    syscall::{
        EBADF, EFAULT, EINVAL, ENOSYS, MAX_READ_LEN, MAX_WRITE_LEN, STDERR, STDIN, STDOUT,
        SyscallNumber,
    },
};

/// syscall結果の受け先。kernelはUARTへframeを載せ、host testは記録する。
pub trait ControlSink {
    type Error;

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error>;
}

/// stdin byteの供給元。kernelはUARTのStdin frameから引き、host testは用意した列を返す。
pub trait ControlSource {
    type Error;

    /// 次の入力を`output`へ移す。`Ok(Some(n))`は受信byte数であり、0はEOF。
    /// `Ok(None)`は現時点で入力がないことを意味し、呼び出し側はprocessを
    /// `Blocked`へ回して後でやり直す。この契約によりsourceは受信待ちで
    /// 停まってはならない。
    fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error>;
}

/// 1個のsystem callを処理した後の継続種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallFlow<E, SE = E> {
    /// guest実行へ戻る。戻り値は`context`の`a0`へ書き込み済みである。
    Resume,
    /// guestが`exit`を要求した。codeは`a0`の下位32bitである。
    Exit(u32),
    /// 継続できない送信失敗。sinkのerrorをそのまま保持する。
    Fatal(E),
    /// 継続できない受信失敗。sourceのerrorをそのまま保持する。
    SourceFatal(SE),
    /// `read`の受信が済み、userへのcopy待ちである。受信byteは呼び出し側の
    /// scratch bufferにあり、handlerが先頭`len` byteを`start`へ移してから
    /// `a0`へ`len`を書いて戻る。4 KiBを値で返さないのは、trap stackの
    /// 多重frameでoverflowさせないためである。
    ReadComplete { start: u64, len: usize },
    /// `read`の検証は通ったが入力が未到着である。`sepc`はecallへ戻してあり、
    /// 入力到着後の再開で同じsyscallがやり直される。schedulerはこのprocessを
    /// stdin待ちへ回し、他のrunnable processを動かせる。
    Blocked,
}

/// `a7`のsystem call番号に従って`context`を処理する。
///
/// guest pointerをRust参照として解することなく、`write`は1回の検証付きcopyと
/// 1回の`sink.frame`で処理し、戻り値を`a0`へ書き込む。descriptorは1と2だけを
/// 許可し、4,096 byteを超える長さは拒否する。`read`はdescriptor 0だけを許可し、
/// 書き込み検証を通してから1回の`source.read_stdin`で`read_scratch`へ受信する。
/// sourceが`Ok(None)`を返す未到着では`Blocked`を返し、`sepc`をecallへ戻して
/// 再開時の再実行に委ねる。
/// userへのcopyは呼び出し側の`ReadComplete`処理へ委ねる。scratchは呼び出し側が
/// 1個だけ持ち、多重frameへ4 KiBを複製しない。
pub fn dispatch_syscall<const N: usize, M: FrameStore, S: ControlSink, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace<'_, N>,
    memory: &M,
    sink: &mut S,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<S::Error, R::Error> {
    let number = context.register(17);
    if number == SyscallNumber::Write as usize {
        dispatch_write(context, space, memory, sink)
    } else if number == SyscallNumber::Read as usize {
        dispatch_read(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Exit as usize {
        SyscallFlow::Exit(context.register(10) as u32)
    } else {
        context.set_register(10, ENOSYS as usize);
        SyscallFlow::Resume
    }
}

fn dispatch_read<const N: usize, M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace<'_, N>,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    if context.register(10) != STDIN {
        context.set_register(10, EBADF as usize);
        return SyscallFlow::Resume;
    }
    let len = context.register(12);
    if len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }
    let start = context.register(11) as u64;
    // stdinの消費は不可逆なため、sourceへ触れる前にEFAULTを確定させる。
    if check_user_writable_range(space, memory, start, len).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    // `Ok(None)`は入力未到着である。`sepc`はclassifyがecallの次へ進めて
    // あるため、4 byte戻して再開時に同じecallをやり直す。
    match source.read_stdin(&mut read_scratch[..len]) {
        Ok(Some(count)) => SyscallFlow::ReadComplete { start, len: count },
        Ok(None) => {
            context.set_sepc(context.sepc() - 4);
            SyscallFlow::Blocked
        }
        Err(error) => SyscallFlow::SourceFatal(error),
    }
}

/// `ReadComplete`の受信済みbyteを検証済みuser rangeへ移し、`a0`へ長さを書く。
///
/// # Safety
///
/// 呼び出し側は`dispatch_read`へ渡したscratchの先頭`len` byteと、返却された
/// `start`と`len`をそのまま渡さなければならない。user satpが有効でguestが
/// 停止中のtrap handlerからのみ呼び、copyの間`sstatus.SUM`を立てておくこと。
/// 検証時からpage tableは不変であり、このcopyは正確に届く。
pub unsafe fn complete_read(context: &mut UserContext, start: u64, len: usize, data: &[u8]) {
    // Safety: 呼び出し側の契約により、検証済みuser rangeへのcopyである。
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), start as *mut u8, len);
    }
    context.set_register(10, len);
}

fn dispatch_write<const N: usize, M: FrameStore, S: ControlSink, SE>(
    context: &mut UserContext,
    space: &AddressSpace<'_, N>,
    memory: &M,
    sink: &mut S,
) -> SyscallFlow<S::Error, SE> {
    let kind = match context.register(10) {
        STDOUT => FrameKind::Stdout,
        STDERR => FrameKind::Stderr,
        _ => {
            context.set_register(10, EBADF as usize);
            return SyscallFlow::Resume;
        }
    };
    let len = context.register(12);
    if len > MAX_WRITE_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }

    // kernel stackに置く一次bufferは最大4,096 byteとする規約である。
    // 検証付きcopyが拒絶した場合、kernel panicではなくEFAULTとして返す。
    let mut buffer = [0u8; MAX_WRITE_LEN];
    match copy_from_user(
        space,
        memory,
        context.register(11) as u64,
        &mut buffer[..len],
    ) {
        Ok(()) => {}
        Err(_) => {
            context.set_register(10, EFAULT as usize);
            return SyscallFlow::Resume;
        }
    }

    match sink.frame(kind, &buffer[..len]) {
        Ok(()) => {
            context.set_register(10, len);
            SyscallFlow::Resume
        }
        Err(error) => SyscallFlow::Fatal(error),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

    use super::{ControlSink, ControlSource, SyscallFlow, dispatch_syscall};
    use crate::{
        memory::frame::{FrameAllocator, PAGE_SIZE},
        user::context::UserContext,
        vm::{AddressSpaceBuilder, AddressSpaceStorage, FrameStore, PageFlags, VirtPage},
    };
    use minios_abi::{
        control::FrameKind,
        syscall::{
            EBADF, EFAULT, EINVAL, ENOSYS, MAX_READ_LEN, MAX_WRITE_LEN, STDERR, STDIN, STDOUT,
            SyscallNumber,
        },
    };

    const MESSAGE: &[u8] = b"MK4";
    const MESSAGE_PAGE: usize = 0x0010_1000;
    const WRITE_NUMBER: usize = SyscallNumber::Write as usize;
    const READ_NUMBER: usize = SyscallNumber::Read as usize;
    const EXIT_NUMBER: usize = SyscallNumber::Exit as usize;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        RangeOutOfBounds,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; PAGE_SIZE]>>,
    }

    impl TestFrameStore {
        fn frame(&self, frame_start: usize) -> Result<&[u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get(&frame_start)
                .map(Box::as_ref)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn frame_mut(
            &mut self,
            frame_start: usize,
        ) -> Result<&mut [u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get_mut(&frame_start)
                .map(Box::as_mut)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn range(offset: usize, len: usize) -> Result<core::ops::Range<usize>, TestStoreError> {
            let end = offset
                .checked_add(len)
                .ok_or(TestStoreError::RangeOutOfBounds)?;
            if end > PAGE_SIZE {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            Ok(offset..end)
        }
    }

    impl FrameStore for TestFrameStore {
        type Error = TestStoreError;

        fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
            self.frames.insert(frame_start, Box::new([0; PAGE_SIZE]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            let offset = index * 8;
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&self.frame(frame_start)?[offset..offset + 8]);
            Ok(u64::from_le_bytes(bytes))
        }

        fn write_u64(
            &mut self,
            frame_start: usize,
            index: usize,
            value: u64,
        ) -> Result<(), Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            let offset = index * 8;
            self.frame_mut(frame_start)?[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }

        fn copy_into(
            &mut self,
            frame_start: usize,
            offset: usize,
            bytes: &[u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, bytes.len())?;
            self.frame_mut(frame_start)?[range].copy_from_slice(bytes);
            Ok(())
        }

        fn copy_out(
            &self,
            frame_start: usize,
            offset: usize,
            output: &mut [u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, output.len())?;
            output.copy_from_slice(&self.frame(frame_start)?[range]);
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SinkError {
        Injected,
    }

    #[derive(Default)]
    struct FakeSink {
        frames: Vec<(FrameKind, Vec<u8>)>,
        fail: bool,
    }

    impl ControlSink for FakeSink {
        type Error = SinkError;

        fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error> {
            if self.fail {
                return Err(SinkError::Injected);
            }
            self.frames.push((kind, Vec::from(payload)));
            Ok(())
        }
    }

    struct FakeSource {
        script: Vec<u8>,
        position: usize,
        fail: bool,
        reads: usize,
        ready: bool,
    }

    impl FakeSource {
        fn scripted(script: &[u8]) -> Self {
            Self {
                script: Vec::from(script),
                position: 0,
                fail: false,
                reads: 0,
                ready: true,
            }
        }

        fn not_ready() -> Self {
            Self {
                ready: false,
                ..Self::scripted(b"")
            }
        }
    }

    impl ControlSource for FakeSource {
        type Error = SinkError;

        fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            self.reads += 1;
            if self.fail {
                return Err(SinkError::Injected);
            }
            if !self.ready {
                return Ok(None);
            }
            let available = &self.script[self.position..];
            let count = core::cmp::min(available.len(), output.len());
            output[..count].copy_from_slice(&available[..count]);
            self.position += count;
            Ok(Some(count))
        }
    }

    fn syscall_context(number: usize, a0: usize, a1: usize, a2: usize) -> UserContext {
        let mut context = UserContext::patterned_for_test(0x0010_0500);
        context.set_register(17, number);
        context.set_register(10, a0);
        context.set_register(11, a1);
        context.set_register(12, a2);
        context
    }

    // MESSAGE page 1枚だけをmapした空間でdispatchを実行する。
    fn dispatch_fixture<R: ControlSource<Error = SinkError>>(
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        sink: &mut FakeSink,
        source: &mut R,
        read_scratch: &mut [u8; MAX_READ_LEN],
    ) -> (UserContext, SyscallFlow<SinkError>) {
        let mut allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut memory, &mut storage).unwrap();
        let page = builder
            .map_new_zeroed(
                VirtPage::from_start(MESSAGE_PAGE as u64).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        builder.copy_into(page, 0, MESSAGE).unwrap();
        let space = builder.finish();
        let mut context = syscall_context(number, a0, a1, a2);
        let flow = dispatch_syscall(&mut context, &space, &memory, sink, source, read_scratch);
        (context, flow)
    }

    /// `dispatch_fixture`と同じ空間を組み、既存のcontextを引き継いでdispatchする。
    /// `Blocked`からの再開 (同じecallの再実行) を再現するテストが使う。
    fn dispatch_fixture_at<R: ControlSource<Error = SinkError>>(
        mut context: UserContext,
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        sink: &mut FakeSink,
        source: &mut R,
        read_scratch: &mut [u8; MAX_READ_LEN],
    ) -> (UserContext, SyscallFlow<SinkError>) {
        let mut allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut memory, &mut storage).unwrap();
        let page = builder
            .map_new_zeroed(
                VirtPage::from_start(MESSAGE_PAGE as u64).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        builder.copy_into(page, 0, MESSAGE).unwrap();
        let space = builder.finish();
        context.set_register(17, number);
        context.set_register(10, a0);
        context.set_register(11, a1);
        context.set_register(12, a2);
        let flow = dispatch_syscall(&mut context, &space, &memory, sink, source, read_scratch);
        (context, flow)
    }

    fn scratch() -> [u8; MAX_READ_LEN] {
        [0xaa; MAX_READ_LEN]
    }

    // Catches missing frames, duplicated frames, wrong frame kinds, wrong
    // payloads, or a return value other than the written byte count.
    #[test]
    fn write_stdout_and_stderr_deliver_one_frame_each_and_return_the_length() {
        for (descriptor, expected_kind) in
            [(STDOUT, FrameKind::Stdout), (STDERR, FrameKind::Stderr)]
        {
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
            3,
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
        let (context, flow) =
            dispatch_fixture(999, 0, 0, 0, &mut sink, &mut source, &mut scratch());

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

    fn read_completion(flow: SyscallFlow<SinkError>) -> (u64, usize) {
        match flow {
            SyscallFlow::ReadComplete { start, len } => (start, len),
            other => panic!("expected ReadComplete, actual {other:?}"),
        }
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

    // Catches accepting a descriptor other than 0 or consuming input on EBADF.
    #[test]
    fn read_reports_unknown_descriptors_with_ebadf() {
        for descriptor in [1, 2, 3] {
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
}
