//! `write`、`exit`、未知のsystem callを純粋なdispatch結果へ変換する。

use crate::{
    user::{context::UserContext, memory::copy_from_user},
    vm::{AddressSpace, FrameStore},
};
use minios_abi::{
    control::FrameKind,
    syscall::{EBADF, EFAULT, EINVAL, ENOSYS, MAX_WRITE_LEN, STDERR, STDOUT, SyscallNumber},
};

/// syscall結果の受け先。kernelはUARTへframeを載せ、host testは記録する。
pub trait ControlSink {
    type Error;

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error>;
}

/// 1個のsystem callを処理した後の継続種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallFlow<E> {
    /// guest実行へ戻る。戻り値は`context`の`a0`へ書き込み済みである。
    Resume,
    /// guestが`exit`を要求した。codeは`a0`の下位32bitである。
    Exit(u32),
    /// 継続できない失敗。sinkのerrorをそのまま保持する。
    Fatal(E),
}

/// `a7`のsystem call番号に従って`context`を処理する。
///
/// guest pointerをRust参照として解することなく、`write`は1回の検証付きcopyと
/// 1回の`sink.frame`で処理し、戻り値を`a0`へ書き込む。descriptorは1と2だけを
/// 許可し、4,096 byteを超える長さは拒否する。
pub fn dispatch_syscall<const N: usize, M: FrameStore, S: ControlSink>(
    context: &mut UserContext,
    space: &AddressSpace<'_, N>,
    memory: &M,
    sink: &mut S,
) -> SyscallFlow<S::Error> {
    let number = context.register(17);
    if number == SyscallNumber::Write as usize {
        dispatch_write(context, space, memory, sink)
    } else if number == SyscallNumber::Exit as usize {
        SyscallFlow::Exit(context.register(10) as u32)
    } else {
        context.set_register(10, ENOSYS as usize);
        SyscallFlow::Resume
    }
}

fn dispatch_write<const N: usize, M: FrameStore, S: ControlSink>(
    context: &mut UserContext,
    space: &AddressSpace<'_, N>,
    memory: &M,
    sink: &mut S,
) -> SyscallFlow<S::Error> {
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

    use super::{ControlSink, SyscallFlow, dispatch_syscall};
    use crate::{
        memory::frame::{FrameAllocator, PAGE_SIZE},
        user::context::UserContext,
        vm::{AddressSpaceBuilder, AddressSpaceStorage, FrameStore, PageFlags, VirtPage},
    };
    use minios_abi::{
        control::FrameKind,
        syscall::{EBADF, EFAULT, EINVAL, ENOSYS, MAX_WRITE_LEN, STDERR, STDOUT, SyscallNumber},
    };

    const MESSAGE: &[u8] = b"MK4";
    const MESSAGE_PAGE: usize = 0x0010_1000;
    const WRITE_NUMBER: usize = SyscallNumber::Write as usize;
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

    fn syscall_context(number: usize, a0: usize, a1: usize, a2: usize) -> UserContext {
        let mut context = UserContext::patterned_for_test(0x0010_0500);
        context.set_register(17, number);
        context.set_register(10, a0);
        context.set_register(11, a1);
        context.set_register(12, a2);
        context
    }

    // MESSAGE page 1枚だけをmapした空間でdispatchを実行する。
    fn dispatch_fixture(
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        sink: &mut FakeSink,
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
        let flow = dispatch_syscall(&mut context, &space, &memory, sink);
        (context, flow)
    }

    // Catches missing frames, duplicated frames, wrong frame kinds, wrong
    // payloads, or a return value other than the written byte count.
    #[test]
    fn write_stdout_and_stderr_deliver_one_frame_each_and_return_the_length() {
        for (descriptor, expected_kind) in
            [(STDOUT, FrameKind::Stdout), (STDERR, FrameKind::Stderr)]
        {
            let mut sink = FakeSink::default();
            let (context, flow) = dispatch_fixture(
                WRITE_NUMBER,
                descriptor,
                MESSAGE_PAGE,
                MESSAGE.len(),
                &mut sink,
            );

            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(sink.frames, vec![(expected_kind, Vec::from(MESSAGE))]);
            assert_eq!(context.register(10), MESSAGE.len());
            assert_eq!(context.register(17), WRITE_NUMBER);
            assert_eq!(context.register(11), MESSAGE_PAGE);
            assert_eq!(context.register(12), MESSAGE.len());
            assert_eq!(context.register(8), 0x5150_0000_0000_0008);
        }
    }

    // Catches accepting a descriptor other than 1 or 2.
    #[test]
    fn write_reports_unknown_descriptors_with_ebadf() {
        let mut sink = FakeSink::default();
        let (context, flow) =
            dispatch_fixture(WRITE_NUMBER, 3, MESSAGE_PAGE, MESSAGE.len(), &mut sink);

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches copying more than one kernel-page worth of bytes.
    #[test]
    fn write_reports_oversized_lengths_with_einval() {
        let mut sink = FakeSink::default();
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            STDOUT,
            MESSAGE_PAGE,
            MAX_WRITE_LEN + 1,
            &mut sink,
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
        let (context, flow) = dispatch_fixture(WRITE_NUMBER, STDOUT, 0x0, 4, &mut sink);

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches resuming a guest that called an unknown syscall without the
    // ENOSYS return value.
    #[test]
    fn unknown_numbers_report_enosys_and_resume() {
        let mut sink = FakeSink::default();
        let (context, flow) = dispatch_fixture(999, 0, 0, 0, &mut sink);

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOSYS as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches losing the exit code or treating exit as a resumable call.
    #[test]
    fn exit_returns_the_code_without_touching_the_context() {
        let mut sink = FakeSink::default();
        let (context, flow) = dispatch_fixture(EXIT_NUMBER, 42, 0, 0, &mut sink);

        assert_eq!(flow, SyscallFlow::Exit(42));
        assert_eq!(context.register(10), 42);
        assert!(sink.frames.is_empty());
    }

    // Catches resuming after a sink failure or clobbering the guest context.
    #[test]
    fn sink_failure_is_fatal_and_preserves_the_context() {
        let mut sink = FakeSink {
            fail: true,
            ..FakeSink::default()
        };
        let (context, flow) =
            dispatch_fixture(WRITE_NUMBER, STDOUT, MESSAGE_PAGE, MESSAGE.len(), &mut sink);

        assert_eq!(flow, SyscallFlow::Fatal(SinkError::Injected));
        assert_eq!(context.register(10), STDOUT);
        assert!(sink.frames.is_empty());
    }
}
