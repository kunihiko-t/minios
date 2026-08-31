//! user実行の所有権まとめとexit経路の回収。

use core::fmt;

use crate::{
    elf::LoadedImage,
    memory::frame::{FrameAllocator, FrameError, FrameStats, PAGE_SIZE, PhysFrame},
    user::syscall::ControlSink,
    vm::{AddressSpace, FrameStore, PhysPageNum},
};
use minios_abi::control::FrameKind;

/// user trap handler専用stackのpage数。
///
/// 現在のassembly frameは1 page内へ収まるが、将来の診断処理にも余裕を残し、
/// Task 2から4で使った16 KiBの静的test stackと同じ幅を所有frameで確保する。
pub const KERNEL_STACK_PAGES: usize = 4;

const fn sv39_satp_bits(root: PhysPageNum) -> u64 {
    (8u64 << 60) | root.as_u64()
}

/// `UserRun`構築中の失敗。
#[derive(Debug, PartialEq, Eq)]
pub enum RunBuildError<E> {
    /// imageと指定allocatorのprovenanceが一致しない。
    WrongAllocator,
    /// trap stackを構成する物理frameが不足した。
    OutOfFrames,
    /// allocatorの断片化により連続したtrap stackを構成できなかった。
    NonContiguousStack,
    /// trap stackのzero fillに失敗した。
    Memory(E),
    /// 構築失敗後の所有frame回収に失敗した。
    Cleanup(FrameError),
}

/// `UserRun`構築失敗と、必要ならcallerへ返すretryable image。
pub struct RunBuildFailure<'storage, const N: usize, E> {
    error: RunBuildError<E>,
    image: Option<LoadedImage<'storage, N>>,
}

impl<'storage, const N: usize, E> RunBuildFailure<'storage, N, E> {
    pub const fn error(&self) -> &RunBuildError<E> {
        &self.error
    }

    pub fn into_parts(self) -> (RunBuildError<E>, Option<LoadedImage<'storage, N>>) {
        (self.error, self.image)
    }
}

impl<const N: usize, E: fmt::Debug> fmt::Debug for RunBuildFailure<'_, N, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunBuildFailure")
            .field("error", &self.error)
            .field("retains_image", &self.image.is_some())
            .finish()
    }
}

/// user実行の終了または回収時の失敗。
#[derive(Debug, PartialEq, Eq)]
pub enum RunError<E> {
    /// 回収済みのrunを再びarchitecture入口へ渡そうとした。
    AlreadyExecuted,
    /// Exit control frameをsinkへ渡せなかった。
    Sink(E),
    /// user imageまたはkernel trap stackを回収できなかった。
    Reclaim(FrameError),
    /// sinkと回収が両方失敗した。
    SinkAndReclaim { sink: E, reclaim: FrameError },
}

/// architecture固有のU-mode入口へ渡す、borrowを含まない実行値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserLaunch {
    user_satp: u64,
    kernel_satp: u64,
    kernel_stack_top: usize,
}

impl UserLaunch {
    pub const fn user_satp(self) -> u64 {
        self.user_satp
    }

    pub const fn kernel_satp(self) -> u64 {
        self.kernel_satp
    }

    pub const fn kernel_stack_top(self) -> usize {
        self.kernel_stack_top
    }
}

/// architecture固有の実行窓がkernelへ戻した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Exit(u32),
    Fatal,
}

/// resource回収まで完了した実行結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCompletion {
    Exit(u32),
    Fatal,
}

/// 一回のuser実行にだけ属するresourceをまとめる。
///
/// `LoadedImage`はinactiveなELFとaddress spaceを所有し、`UserRun`は実行中だけ
/// 必要なkernel trap stack、allocator、frame memoryの排他的borrowを加える。
pub struct UserRun<'run, const N: usize, const WORDS: usize, M: FrameStore> {
    image: Option<LoadedImage<'run, N>>,
    allocator: &'run mut FrameAllocator<WORDS>,
    memory: &'run mut M,
    kernel_stack: [Option<PhysFrame>; KERNEL_STACK_PAGES],
    kernel_stack_bottom: usize,
    user_satp: u64,
    kernel_satp: u64,
    executed: bool,
}

impl<const N: usize, const WORDS: usize, M: FrameStore> fmt::Debug for UserRun<'_, N, WORDS, M> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserRun")
            .field("kernel_stack_bottom", &self.kernel_stack_bottom)
            .field("user_satp", &self.user_satp)
            .field("kernel_satp", &self.kernel_satp)
            .finish_non_exhaustive()
    }
}

impl<'run, const N: usize, const WORDS: usize, M: FrameStore> UserRun<'run, N, WORDS, M> {
    /// inactive imageへ実行時resourceを加える。
    ///
    /// 途中で失敗した場合は、この関数がtrap stackとimageを両方回収する。
    pub fn new(
        image: LoadedImage<'run, N>,
        allocator: &'run mut FrameAllocator<WORDS>,
        memory: &'run mut M,
        kernel_root: PhysPageNum,
    ) -> Result<Self, RunBuildFailure<'run, N, M::Error>> {
        if image.allocator_id() != allocator.allocator_id() {
            return Err(RunBuildFailure {
                error: RunBuildError::WrongAllocator,
                image: Some(image),
            });
        }
        let user_root = PhysPageNum::from_start(image.address_space().root().as_u64())
            .expect("loaded image roots are page-aligned physical page numbers");
        let mut kernel_stack = [const { None }; KERNEL_STACK_PAGES];
        let mut stack_bottom = None;

        for index in 0..KERNEL_STACK_PAGES {
            let Some(frame) = allocator.allocate() else {
                return Err(build_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    RunBuildError::OutOfFrames,
                ));
            };
            let start = frame.start();
            let bottom = *stack_bottom.get_or_insert(start);
            if start != bottom + index * PAGE_SIZE {
                kernel_stack[index] = Some(frame);
                return Err(build_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    RunBuildError::NonContiguousStack,
                ));
            }
            kernel_stack[index] = Some(frame);
            if let Err(error) = memory.zero_frame(start) {
                return Err(build_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    RunBuildError::Memory(error),
                ));
            }
        }

        Ok(Self {
            image: Some(image),
            allocator,
            memory,
            kernel_stack,
            kernel_stack_bottom: stack_bottom.expect("kernel stack has at least one page"),
            user_satp: sv39_satp_bits(user_root),
            kernel_satp: sv39_satp_bits(kernel_root),
            executed: false,
        })
    }

    pub const fn address_space(&self) -> &AddressSpace<'run, N> {
        self.image
            .as_ref()
            .expect("live run retains its loaded image")
            .address_space()
    }

    pub const fn memory(&self) -> &M {
        self.memory
    }

    pub const fn user_satp(&self) -> u64 {
        self.user_satp
    }

    pub const fn kernel_satp(&self) -> u64 {
        self.kernel_satp
    }

    pub const fn kernel_stack_bottom(&self) -> usize {
        self.kernel_stack_bottom
    }

    pub const fn kernel_stack_top(&self) -> usize {
        self.kernel_stack_bottom + KERNEL_STACK_PAGES * PAGE_SIZE
    }

    pub const fn allocator_stats(&self) -> FrameStats {
        self.allocator.stats()
    }

    /// architecture固有の実行窓を一度呼び、kernelへ戻った後に必ず回収する。
    ///
    /// `enter`は`UserLaunch`を使ってU-modeへ移り、kernel satpとcaller stackを
    /// 復元してから`RunOutcome`を返さなければならない。FatalではExit frameを
    /// 送らず、Exitではframe送信成否にかかわらず同じ回収処理を実行する。
    pub fn execute<S, F>(
        &mut self,
        sink: &mut S,
        enter: F,
    ) -> Result<RunCompletion, RunError<S::Error>>
    where
        S: ControlSink,
        F: FnOnce(UserLaunch) -> RunOutcome,
    {
        if self.executed {
            return Err(RunError::AlreadyExecuted);
        }
        self.executed = true;
        let launch = UserLaunch {
            user_satp: self.user_satp,
            kernel_satp: self.kernel_satp,
            kernel_stack_top: self.kernel_stack_top(),
        };
        match enter(launch) {
            RunOutcome::Exit(code) => self.finish_exit(code, sink).map(RunCompletion::Exit),
            RunOutcome::Fatal => {
                self.reclaim().map_err(RunError::Reclaim)?;
                Ok(RunCompletion::Fatal)
            }
        }
    }

    /// Exit frameを一回送った後、送信成否にかかわらず全resourceを回収する。
    pub fn finish_exit<S: ControlSink>(
        &mut self,
        code: u32,
        sink: &mut S,
    ) -> Result<u32, RunError<S::Error>> {
        let sink_result = sink.frame(FrameKind::Exit, &code.to_le_bytes());
        let reclaim_result = self.reclaim();
        match (sink_result, reclaim_result) {
            (Ok(()), Ok(())) => Ok(code),
            (Err(error), Ok(())) => Err(RunError::Sink(error)),
            (Ok(()), Err(error)) => Err(RunError::Reclaim(error)),
            (Err(sink), Err(reclaim)) => Err(RunError::SinkAndReclaim { sink, reclaim }),
        }
    }

    /// user address spaceがactiveでない状態で全所有frameを回収する。
    ///
    /// 失敗した所有権はstruct内へ戻すため、呼び出し側は同じrunで再試行できる。
    pub fn reclaim(&mut self) -> Result<(), FrameError> {
        self.executed = true;
        for index in (0..KERNEL_STACK_PAGES).rev() {
            let Some(frame) = self.kernel_stack[index].take() else {
                continue;
            };
            if let Err((error, frame)) = self.allocator.deallocate_recoverable(frame) {
                self.kernel_stack[index] = Some(frame);
                return Err(error);
            }
        }

        let Some(image) = self.image.take() else {
            return Ok(());
        };
        match image.destroy(self.allocator) {
            Ok(()) => Ok(()),
            Err(error) => {
                let (frame_error, image) = error.into_parts();
                self.image = Some(image);
                Err(frame_error)
            }
        }
    }
}

fn build_failure<'storage, const N: usize, const WORDS: usize, E>(
    image: LoadedImage<'storage, N>,
    stack: &mut [Option<PhysFrame>; KERNEL_STACK_PAGES],
    allocator: &mut FrameAllocator<WORDS>,
    primary: RunBuildError<E>,
) -> RunBuildFailure<'storage, N, E> {
    reclaim_stack(stack, allocator);
    match image.destroy(allocator) {
        Ok(()) => RunBuildFailure {
            error: primary,
            image: None,
        },
        Err(error) => {
            let (frame_error, image) = error.into_parts();
            RunBuildFailure {
                error: RunBuildError::Cleanup(frame_error),
                image: Some(image),
            }
        }
    }
}

fn reclaim_stack<const WORDS: usize>(
    stack: &mut [Option<PhysFrame>; KERNEL_STACK_PAGES],
    allocator: &mut FrameAllocator<WORDS>,
) {
    for index in (0..KERNEL_STACK_PAGES).rev() {
        let Some(frame) = stack[index].take() else {
            continue;
        };
        allocator
            .deallocate(frame)
            .expect("newly allocated kernel stack frame remains owned during rollback");
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

    use super::{KERNEL_STACK_PAGES, RunBuildError, RunCompletion, RunError, RunOutcome, UserRun};
    use crate::{
        elf::{fixture::valid_riscv64_elf, load_image},
        memory::frame::{FrameAllocator, FrameStats, PAGE_SIZE},
        user::syscall::ControlSink,
        vm::{AddressSpaceStorage, FrameStore, PhysPageNum},
    };
    use minios_abi::control::FrameKind;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        RangeOutOfBounds,
        InjectedZero,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; PAGE_SIZE]>>,
        fail_next_zero: bool,
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

        fn fail_next_zero(&mut self) {
            self.fail_next_zero = true;
        }
    }

    impl FrameStore for TestFrameStore {
        type Error = TestStoreError;

        fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
            if self.fail_next_zero {
                self.fail_next_zero = false;
                return Err(TestStoreError::InjectedZero);
            }
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

    const SYNTHETIC_KERNEL_ROOT: u64 = 0x1234_5000;

    struct RunFixture {
        frames: FrameAllocator<16>,
        memory: TestFrameStore,
        storage: AddressSpaceStorage<2688>,
    }

    impl RunFixture {
        // host testは実在しない物理frameを解参照しない。所有権の帳簿とsatp値
        // だけを検査するため、kernel rootは合成値でよい。
        fn new() -> Self {
            let mut frames = unsafe { FrameAllocator::<16>::new(0x1000, 0x181_000) }.unwrap();
            let memory = TestFrameStore::default();
            // bitmapの先頭を汚し、stack確保が先頭以外から始めても連続性の
            // 検査が成立することを確かめる。
            frames.allocate();
            frames.allocate();
            let storage = AddressSpaceStorage::<2688>::new();
            Self {
                frames,
                memory,
                storage,
            }
        }

        fn baseline(&self) -> FrameStats {
            self.frames.stats()
        }

        fn loaded_image_frame_count() -> usize {
            let mut fixture = Self::new();
            let before = fixture.frames.stats().allocated;
            let bytes = valid_riscv64_elf();
            let image = load_image(
                &bytes,
                &mut fixture.frames,
                &mut fixture.memory,
                &mut fixture.storage,
            )
            .unwrap_or_else(|error| panic!("fixture image must load: {error:?}"));
            let count = fixture.frames.stats().allocated - before;
            image
                .destroy(&mut fixture.frames)
                .unwrap_or_else(|error| panic!("fixture image must be reclaimable: {error:?}"));
            assert!(fixture.storage.is_empty());
            count
        }

        fn build_run(&mut self) -> UserRun<'_, 2688, 16, TestFrameStore> {
            let bytes = valid_riscv64_elf();
            let image = load_image(
                &bytes,
                &mut self.frames,
                &mut self.memory,
                &mut self.storage,
            )
            .unwrap_or_else(|error| panic!("fixture image must load: {error:?}"));
            let kernel_root =
                PhysPageNum::from_start(SYNTHETIC_KERNEL_ROOT).expect("kernel root is valid");
            UserRun::new(image, &mut self.frames, &mut self.memory, kernel_root)
                .unwrap_or_else(|error| panic!("fixture run must build: {error:?}"))
        }

        fn run_exit(&mut self, code: u32) -> Result<u32, RunError<SinkError>> {
            let mut run = self.build_run();
            run.finish_exit(code, &mut FakeSink::default())
        }
    }

    // Catches leaking the fixture's page tables, user pages, stack pages, or
    // the freshly installed kernel trap stack after a successful exit.
    #[test]
    fn successful_exit_restores_allocator_and_storage() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let code = fixture.run_exit(42).unwrap();

        assert_eq!(code, 42);
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches dropping the exit code or sending a payload other than the
    // little-endian u32 exactly once.
    #[test]
    fn exit_delivers_one_frame_with_the_code_and_returns_it() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let mut sink = FakeSink::default();
        let mut run = fixture.build_run();
        let code = run.finish_exit(42, &mut sink).unwrap();

        assert_eq!(code, 42);
        assert_eq!(
            sink.frames,
            vec![(FrameKind::Exit, Vec::from(42_u32.to_le_bytes()))]
        );
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches skipping reclamation when the control sink rejects the Exit
    // frame: the allocator and storage must still return to the baseline.
    #[test]
    fn sink_failure_is_reported_and_still_reclaims_everything() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let mut sink = FakeSink {
            fail: true,
            ..FakeSink::default()
        };
        let mut run = fixture.build_run();
        let outcome = run.execute(&mut sink, |_| RunOutcome::Exit(42));

        assert_eq!(outcome, Err(RunError::Sink(SinkError::Injected)));
        assert!(sink.frames.is_empty());
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches leaking the run when execution reports a fatal user trap and no
    // Exit frame must be sent.
    #[test]
    fn fatal_execution_restores_the_baseline_without_any_frame() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let mut sink = FakeSink::default();
        let mut run = fixture.build_run();
        let completion = run.execute(&mut sink, |_| RunOutcome::Fatal).unwrap();

        assert_eq!(completion, RunCompletion::Fatal);
        assert!(sink.frames.is_empty());
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches launching again with the stale SATP and trap-stack addresses
    // left behind after the first execution reclaimed every owned frame.
    #[test]
    fn reclaimed_run_rejects_a_second_execution() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let mut sink = FakeSink::default();
        let mut run = fixture.build_run();

        assert_eq!(
            run.execute(&mut sink, |_| RunOutcome::Fatal),
            Ok(RunCompletion::Fatal)
        );
        let mut entered_again = false;
        let second = run.execute(&mut sink, |_| {
            entered_again = true;
            RunOutcome::Fatal
        });

        assert_eq!(second, Err(RunError::AlreadyExecuted));
        assert!(!entered_again);
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches explicitly reclaiming an inactive run and then launching with
    // the addresses of the page tables and trap stack that reclaim released.
    #[test]
    fn explicitly_reclaimed_run_cannot_be_executed() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let mut sink = FakeSink::default();
        let mut run = fixture.build_run();

        run.reclaim().unwrap();
        let mut entered = false;
        let outcome = run.execute(&mut sink, |_| {
            entered = true;
            RunOutcome::Fatal
        });

        assert_eq!(outcome, Err(RunError::AlreadyExecuted));
        assert!(!entered);
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches consuming the loaded image when no frame remains for the first
    // kernel stack page. Constructor failure must reclaim the image itself.
    #[test]
    fn stack_allocation_failure_reclaims_the_loaded_image() {
        let mut fixture = RunFixture::new();
        let before_load = fixture.frames.stats().allocated;
        let bytes = valid_riscv64_elf();
        let image = load_image(
            &bytes,
            &mut fixture.frames,
            &mut fixture.memory,
            &mut fixture.storage,
        )
        .unwrap();
        let image_frames = fixture.frames.stats().allocated - before_load;
        let mut held = Vec::new();
        while let Some(frame) = fixture.frames.allocate() {
            held.push(frame);
        }
        let allocated_when_full = fixture.frames.stats().allocated;
        let kernel_root = PhysPageNum::from_start(SYNTHETIC_KERNEL_ROOT).unwrap();

        let outcome = UserRun::new(image, &mut fixture.frames, &mut fixture.memory, kernel_root);

        assert_eq!(outcome.unwrap_err().error(), &RunBuildError::OutOfFrames);
        assert_eq!(
            fixture.frames.stats().allocated,
            allocated_when_full - image_frames
        );
        assert!(fixture.storage.is_empty());
        assert!(!held.is_empty());
    }

    // Catches leaking both the newly allocated stack frame and the loaded
    // image when zeroing the stack fails after allocation.
    #[test]
    fn stack_zero_failure_reclaims_the_stack_and_loaded_image() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let bytes = valid_riscv64_elf();
        let image = load_image(
            &bytes,
            &mut fixture.frames,
            &mut fixture.memory,
            &mut fixture.storage,
        )
        .unwrap();
        fixture.memory.fail_next_zero();
        let kernel_root = PhysPageNum::from_start(SYNTHETIC_KERNEL_ROOT).unwrap();

        let outcome = UserRun::new(image, &mut fixture.frames, &mut fixture.memory, kernel_root);

        assert_eq!(
            outcome.unwrap_err().error(),
            &RunBuildError::Memory(TestStoreError::InjectedZero)
        );
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches consuming a retryable image when the caller pairs it with a
    // different allocator instance. No stack allocation may start first.
    #[test]
    fn wrong_allocator_failure_returns_the_image_for_origin_retry() {
        let mut fixture = RunFixture::new();
        let before = fixture.baseline();
        let bytes = valid_riscv64_elf();
        let image = load_image(
            &bytes,
            &mut fixture.frames,
            &mut fixture.memory,
            &mut fixture.storage,
        )
        .unwrap();
        let loaded = fixture.frames.stats();
        let mut wrong_allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x181_000) }.unwrap();
        let wrong_before = wrong_allocator.stats();
        let kernel_root = PhysPageNum::from_start(SYNTHETIC_KERNEL_ROOT).unwrap();

        let failure = UserRun::new(
            image,
            &mut wrong_allocator,
            &mut fixture.memory,
            kernel_root,
        )
        .unwrap_err();
        assert_eq!(failure.error(), &RunBuildError::WrongAllocator);
        assert_eq!(fixture.frames.stats(), loaded);
        let (error, image) = failure.into_parts();
        assert_eq!(error, RunBuildError::WrongAllocator);
        image
            .expect("wrong allocator failure retains the image")
            .destroy(&mut fixture.frames)
            .unwrap();

        assert_eq!(wrong_allocator.stats(), wrong_before);
        assert_eq!(fixture.frames.stats(), before);
        assert!(fixture.storage.is_empty());
    }

    // Catches a trap stack that is not page-granular, not the configured page
    // count, or not charged to the allocator as owned kernel stack frames.
    #[test]
    fn the_run_owns_a_contiguous_kernel_trap_stack() {
        let mut fixture = RunFixture::new();
        let before = fixture.frames.stats().allocated;
        let image_frames = RunFixture::loaded_image_frame_count();
        let mut run = fixture.build_run();

        assert_eq!(
            run.allocator_stats().allocated,
            before + image_frames + KERNEL_STACK_PAGES
        );
        let top = run.kernel_stack_top();
        assert_eq!(top % PAGE_SIZE, 0);
        assert_eq!(
            top - run.kernel_stack_bottom(),
            KERNEL_STACK_PAGES * PAGE_SIZE
        );

        run.reclaim().unwrap();
        assert!(fixture.storage.is_empty());
    }

    // Catches duplicating the Sv39 encoding with a different MODE or PPN.
    #[test]
    fn kernel_satp_matches_the_csr_encoding() {
        let mut fixture = RunFixture::new();
        let run = fixture.build_run();

        assert_eq!(
            run.kernel_satp(),
            crate::arch::riscv64::csr::sv39_satp_bits(
                PhysPageNum::from_start(SYNTHETIC_KERNEL_ROOT).unwrap()
            )
        );
        assert_eq!(run.user_satp() & (0xf << 60), 8 << 60);
    }
}
