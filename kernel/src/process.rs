//! 複数processの所有権とround-robin選択。
//!
//! `UserRun` (一回限りの実行窓) とは別に、`Process`は再入可能な実行単位として
//! image・kernel trap stack・中断contextを所有する。allocatorやframe memoryへの
//! 参照は保持せず、各操作の呼び出し側が都度渡す。`ProcessTable`は最大
//! [`MAX_PROCS`] slotのround-robin選択を担う状態機械であり、host test可能にする。

use core::fmt;

use crate::{
    elf::{LoadError, LoadedImage, load_image_with_kernel_mappings},
    memory::frame::{FrameError, FrameSource, PAGE_SIZE, PhysFrame},
    user::{
        context::UserContext,
        run::KERNEL_STACK_PAGES,
        stack::{InitialStackError, write_initial_argv},
    },
    vm::{AddressSpace, FrameStore, KernelMapping, PhysPageNum, VirtAddr},
};

/// 同時に生存できるprocess数。manifestが宣言できるimage数の上限と一致させる。
pub const MAX_PROCS: usize = minios_abi::manifest::IMAGE_MAX_COUNT;

/// `user.S`のtrap frame配置に合わせた、kernel stack topからcontext slotまでの
/// byte offset。context (34語=272 byte) とheader (144 byte) の合計416 byteであり、
/// `user.S`側の`sp - 416`と一致させること。
const TRAP_FRAME_BYTES: usize = 416;

/// `Process::spawn`が失敗した理由。
#[derive(Debug, PartialEq, Eq)]
pub enum SpawnError<E> {
    /// ELFの解析・mapping・user stack確保に失敗した。
    Load(LoadError<E>),
    /// kernel trap stack用の物理frameを確保できなかった。
    OutOfFrames,
    /// 確保したstack frameが連続していなかった。
    NonContiguousStack,
    /// stack frameのzero化でframe memoryが失敗した。
    Memory(E),
    /// argv blockの構築・書き込みに失敗した。
    Argv(InitialStackError<E>),
    /// 途中失敗後の回収自体が失敗した。`image`が`SpawnFailure`へ残る。
    Cleanup(FrameError),
}

/// `Process::spawn`の失敗結果。回収しきれなかったimageを保持し、呼び出し側が
/// `LoadedImage::destroy`を再試行できるようにする。
pub struct SpawnFailure<E> {
    pub error: SpawnError<E>,
    pub image: Option<LoadedImage>,
}

impl<E> fmt::Debug for SpawnFailure<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpawnFailure")
            .field("image_retained", &self.image.is_some())
            .finish_non_exhaustive()
    }
}

/// processの実行可能状態。占有slotはすべて生きているprocessであり、
/// 終了したprocessは`take`でslotから除かれる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// 次のdispatchを受け付けられる。
    Runnable,
    /// `read`が入力待ちで中断した。stdinへbyteが届くと`Runnable`へ戻る。
    BlockedOnStdin,
}

/// 再入可能なuser process。image (user address spaceとその所有frame)、
/// 専用kernel trap stack、前回中断時の`UserContext`を所有する。
///
/// allocator/frame memoryへの参照は保持しないため、生存中のprocess同士が
/// borrowを共有せず、tableが複数のprocessを同時に抱えられる。
/// `dispatch`中だけkernelが当該stackを使い、戻った時点でcontextをslotから
/// 回収する (`reload_context`)。
pub struct Process {
    name: &'static str,
    image: Option<LoadedImage>,
    kernel_stack: [Option<PhysFrame>; KERNEL_STACK_PAGES],
    kernel_stack_bottom: usize,
    user_satp: u64,
    context: UserContext,
    state: ProcessState,
}

impl fmt::Debug for Process {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Process")
            .field("name", &self.name)
            .field("kernel_stack_bottom", &self.kernel_stack_bottom)
            .field("user_satp", &self.user_satp)
            .finish_non_exhaustive()
    }
}

impl Process {
    /// ELFを独立したaddress spaceへ読み込み、kernel trap stackと初期contextを
    /// 組み立てる。途中失敗時は確保済みのresourceをすべて回収する。
    ///
    /// 所有権台帳はimage内のaddress spaceが所有するため専用arenaの受け渡しは
    /// 要らない。`kernel_mappings`はsupervisor専用の借用mappingとして
    /// 各user spaceへ複写される。
    pub fn spawn<M: FrameStore, I: IntoIterator<Item = KernelMapping>>(
        name: &'static str,
        elf: &[u8],
        arguments: &[&str],
        allocator: &mut dyn FrameSource,
        memory: &mut M,
        kernel_mappings: I,
    ) -> Result<Self, SpawnFailure<M::Error>> {
        let image = match load_image_with_kernel_mappings(elf, allocator, memory, kernel_mappings) {
            Ok(image) => image,
            Err(error) => {
                return Err(SpawnFailure {
                    error: SpawnError::Load(error),
                    image: None,
                });
            }
        };

        let mut kernel_stack = [const { None }; KERNEL_STACK_PAGES];
        let mut stack_bottom = None;
        for index in 0..KERNEL_STACK_PAGES {
            let Some(frame) = allocator.allocate() else {
                return Err(spawn_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    SpawnError::OutOfFrames,
                ));
            };
            let start = frame.start();
            let bottom = *stack_bottom.get_or_insert(start);
            if start != bottom + index * PAGE_SIZE {
                kernel_stack[index] = Some(frame);
                return Err(spawn_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    SpawnError::NonContiguousStack,
                ));
            }
            kernel_stack[index] = Some(frame);
            if let Err(error) = memory.zero_frame(start) {
                return Err(spawn_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    SpawnError::Memory(error),
                ));
            }
        }

        let initial = match write_initial_argv(image.address_space(), memory, name, arguments) {
            Ok(initial) => initial,
            Err(error) => {
                return Err(spawn_failure(
                    image,
                    &mut kernel_stack,
                    allocator,
                    SpawnError::Argv(error),
                ));
            }
        };

        let stack_pointer = VirtAddr::try_new(initial.stack_pointer as u64)
            .expect("initial user stack pointer is a canonical user address");
        let context = UserContext::with_arguments(
            image.entry(),
            stack_pointer,
            initial.argc,
            initial.argv_address,
        );
        let user_root = PhysPageNum::from_start(image.address_space().root().as_u64())
            .expect("loaded image roots are page-aligned physical page numbers");
        let user_satp = sv39_satp_bits(user_root);

        Ok(Self {
            name,
            image: Some(image),
            kernel_stack,
            kernel_stack_bottom: stack_bottom.expect("kernel stack has at least one page"),
            user_satp,
            context,
            state: ProcessState::Runnable,
        })
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// stdin待ちへ移す。`dispatch`が`Blocked`を返した直後に呼ぶ。
    pub fn block_on_stdin(&mut self) {
        self.state = ProcessState::BlockedOnStdin;
    }

    /// stdinへのbyte到着で再びdispatch可能にする。
    pub fn wake(&mut self) {
        self.state = ProcessState::Runnable;
    }

    pub const fn is_runnable(&self) -> bool {
        matches!(self.state, ProcessState::Runnable)
    }

    pub const fn address_space(&self) -> &AddressSpace {
        self.image
            .as_ref()
            .expect("live process retains its loaded image")
            .address_space()
    }

    pub const fn user_satp(&self) -> u64 {
        self.user_satp
    }

    /// kernel trap stackの上端。`__run_user`へ渡すと、trap入口はこの直下の
    /// context slotを使い、headerは`top-144`を起点に置く。
    pub const fn kernel_stack_top(&self) -> usize {
        self.kernel_stack_bottom + KERNEL_STACK_PAGES * PAGE_SIZE
    }

    /// 次の`__run_user`へ渡すcontextのpointer。中断後のcontextが既に
    /// `reload_context`で回収済みなら、再開位置が書き込まれている。
    pub fn context_ptr(&mut self) -> *mut UserContext {
        &mut self.context
    }

    /// 直前の`__run_user`がこのprocessのkernel stackへ保存したcontextを
    /// slotから回収する。
    ///
    /// # Safety
    /// 直前にこのprocessを`__run_user`でdispatchし、`ReturnToKernel`で
    /// 戻っていること。slotにはその時点の中断contextが書き込まれている。
    pub unsafe fn reload_context(&mut self) {
        let slot = (self.kernel_stack_top() - TRAP_FRAME_BYTES) as *const UserContext;
        // Safety: callerがdispatch直後であることを保証する。slotはこのprocess
        // 専用のkernel stack内であり、`user.S`の規約でcontextが置かれる。
        self.context = unsafe { slot.read() };
    }

    /// user address spaceがinactiveな状態で、trap stackとimageの全所有frameを
    /// 回収する。失敗したframeはstruct内へ戻すため再試行できる。
    pub fn reclaim(&mut self, allocator: &mut dyn FrameSource) -> Result<(), FrameError> {
        for index in (0..KERNEL_STACK_PAGES).rev() {
            let Some(frame) = self.kernel_stack[index].take() else {
                continue;
            };
            if let Err((error, frame)) = allocator.deallocate_recoverable(frame) {
                self.kernel_stack[index] = Some(frame);
                return Err(error);
            }
        }
        let Some(image) = self.image.take() else {
            return Ok(());
        };
        match image.destroy(allocator) {
            Ok(()) => Ok(()),
            Err(error) => {
                let (frame_error, image) = error.into_parts();
                self.image = Some(image);
                Err(frame_error)
            }
        }
    }
}

/// kernel stackとimageの部分回収。`UserRun`の`build_failure`と同じ順序で、
/// 途中失敗時に確保済みresourceを漏らさない。
fn spawn_failure<E>(
    image: LoadedImage,
    stack: &mut [Option<PhysFrame>; KERNEL_STACK_PAGES],
    allocator: &mut dyn FrameSource,
    primary: SpawnError<E>,
) -> SpawnFailure<E> {
    for slot in stack.iter_mut().rev() {
        if let Some(frame) = slot.take()
            && let Err((_, frame)) = allocator.deallocate_recoverable(frame)
        {
            *slot = Some(frame);
        }
    }
    match image.destroy(allocator) {
        Ok(()) => SpawnFailure {
            error: primary,
            image: None,
        },
        Err(error) => {
            let (frame_error, image) = error.into_parts();
            SpawnFailure {
                error: SpawnError::Cleanup(frame_error),
                image: Some(image),
            }
        }
    }
}

/// Sv39 satp値 (mode=8) を組み立てる。`arch::riscv64::csr::sv39_satp_bits`と
/// 同じ規約だが、このmoduleはhost testでも動かすため自前で計算する。
const fn sv39_satp_bits(root: PhysPageNum) -> u64 {
    (8_u64 << 60) | root.as_u64()
}

/// 固定上限のprocess table。slot indexがprocess IDとなり、manifestの
/// image順と一致する。空slotは`pick_next`が読み飛ばす。
pub struct ProcessTable {
    slots: [Option<Process>; MAX_PROCS],
    next_hint: usize,
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; MAX_PROCS],
            next_hint: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 最初の空slotへprocessを置き、pid (=slot index) を返す。
    /// 満杯ならprocessをそのまま返す。
    ///
    /// Errがprocess全体を返すのは、callerが拒否されたprocessの所有frameを
    /// 回収するためであり、`deallocate_recoverable`が失敗時にframeを返すのと
    /// 同じ規約である。
    #[allow(clippy::result_large_err)]
    pub fn insert(&mut self, process: Process) -> Result<usize, Process> {
        let Some(index) = self.slots.iter().position(|slot| slot.is_none()) else {
            return Err(process);
        };
        self.slots[index] = Some(process);
        Ok(index)
    }

    pub fn get(&self, pid: usize) -> Option<&Process> {
        self.slots.get(pid).and_then(|slot| slot.as_ref())
    }

    pub fn get_mut(&mut self, pid: usize) -> Option<&mut Process> {
        self.slots.get_mut(pid).and_then(|slot| slot.as_mut())
    }

    /// slotからprocessを取り出す。呼び出し側が`reclaim`で所有frameを回収する。
    pub fn take(&mut self, pid: usize) -> Option<Process> {
        self.slots.get_mut(pid).and_then(|slot| slot.take())
    }

    /// 前回dispatchしたpidの次から時計回りに走査し、最初のrunnable slotの
    /// pidを返す。全slotが空か、占有slotがすべてblockedなら`None`を返す。
    /// 退出したprocessは`take`で取り除く。
    pub fn pick_next(&mut self) -> Option<usize> {
        for offset in 0..MAX_PROCS {
            let pid = (self.next_hint + offset) % MAX_PROCS;
            if self.slots[pid].as_ref().is_some_and(Process::is_runnable) {
                self.next_hint = (pid + 1) % MAX_PROCS;
                return Some(pid);
            }
        }
        None
    }

    /// 占有slotがありながら`pick_next`が`None`＝全processがstdin待ち。
    /// stdinへbyteが届いたら呼び、blocked processをすべてrunnableへ戻す。
    pub fn wake_all_blocked(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            if !slot.is_runnable() {
                slot.wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec::Vec};

    use super::*;
    use crate::{
        elf::fixture::valid_riscv64_elf,
        memory::frame::{FrameAllocator, FrameStats},
    };

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

    /// 実在しない物理frameを解参照しないhost test用fixture。satp値と
    /// 所有権の帳簿だけを検査する。
    struct SpawnFixture {
        frames: FrameAllocator<16>,
        memory: TestFrameStore,
    }

    impl SpawnFixture {
        fn new() -> Self {
            let mut frames = unsafe { FrameAllocator::<16>::new(0x1000, 0x181_000) }.unwrap();
            // bitmapの先頭を汚し、連続stack確保が先頭以外でも検査できるようにする。
            frames.allocate();
            frames.allocate();
            Self {
                frames,
                memory: TestFrameStore::default(),
            }
        }

        fn baseline(&self) -> FrameStats {
            self.frames.stats()
        }

        fn spawn(&mut self, name: &'static str) -> Process {
            let bytes = valid_riscv64_elf();
            Process::spawn(
                name,
                &bytes,
                &[],
                &mut self.frames,
                &mut self.memory,
                core::iter::empty(),
            )
            .unwrap_or_else(|error| panic!("fixture process must spawn: {error:?}"))
        }
    }

    // Catches a spawn that loses ownership of the image, stack, or context:
    // the process must hold an address space, a contiguous 4-page kernel stack,
    // and an Sv39 user satp rooted at the image's table.
    #[test]
    fn spawn_owns_image_stack_and_context() {
        let mut fixture = SpawnFixture::new();
        let before = fixture.baseline();

        let process = fixture.spawn("proc-a");

        assert_eq!(process.name(), "proc-a");
        assert_eq!(process.kernel_stack_top() % PAGE_SIZE, 0);
        assert_eq!(process.user_satp() >> 60, 8);
        assert!(process.address_space().owned_frames() > 0);
        assert!(fixture.frames.stats().allocated > before.allocated);
    }

    // Catches a partial-allocation leak: when the kernel trap stack cannot be
    // fully allocated, spawn must return every frame it took (image frames
    // included) and leave the storage arena empty.
    #[test]
    fn spawn_failure_releases_everything() {
        // imageが占有するframe数を測り、それより2枚だけ多いarenaを用意すると、
        // stack確保が途中で枯渇して部分確保の回収経路を踏む。
        let mut probe = SpawnFixture::new();
        let before = probe.baseline().allocated;
        let bytes = valid_riscv64_elf();
        let image = crate::elf::load_image(&bytes, &mut probe.frames, &mut probe.memory)
            .unwrap_or_else(|error| panic!("fixture image must load: {error:?}"));
        let image_frames = probe.frames.stats().allocated - before;
        image
            .destroy(&mut probe.frames)
            .unwrap_or_else(|error| panic!("fixture image must be reclaimable: {error:?}"));
        drop(probe);

        let mut frames =
            unsafe { FrameAllocator::<16>::new(0x1000, (image_frames + 2) * PAGE_SIZE).unwrap() };
        let mut memory = TestFrameStore::default();

        let bytes = valid_riscv64_elf();
        let failure = Process::spawn(
            "proc-b",
            &bytes,
            &[],
            &mut frames,
            &mut memory,
            core::iter::empty(),
        )
        .expect_err("starved allocator must fail spawn");

        assert_eq!(failure.error, SpawnError::OutOfFrames);
        assert!(failure.image.is_none());
        assert_eq!(frames.stats().allocated, 0);
    }

    // Catches reclaim dropping only part of a process: every owned frame must
    // return to the allocator and the storage arena must be reusable.
    #[test]
    fn reclaim_releases_all_owned_frames() {
        let mut fixture = SpawnFixture::new();
        let before = fixture.baseline();
        let mut process = fixture.spawn("proc-c");

        process
            .reclaim(&mut fixture.frames)
            .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));

        assert_eq!(fixture.baseline(), before);
    }

    // Catches round-robin order drifting: picks must cycle over live slots and
    // resume after the last dispatched pid rather than restarting at slot 0.
    #[test]
    fn table_cycles_over_live_slots() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let p0 = fixture.spawn("p0");
        let p1 = fixture.spawn("p1");
        let p2 = fixture.spawn("p2");
        assert_eq!(table.insert(p0).expect("insert p0"), 0);
        assert_eq!(table.insert(p1).expect("insert p1"), 1);
        assert_eq!(table.insert(p2).expect("insert p2"), 2);

        let sequence: Vec<usize> = (0..7).map(|_| table.pick_next().unwrap()).collect();
        assert_eq!(sequence, [0, 1, 2, 0, 1, 2, 0]);

        // slot 1を除くと順序は0,2,0,2…へ縮む。
        let mut removed = table.take(1).expect("slot 1 is live");
        removed
            .reclaim(&mut fixture.frames)
            .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));

        let sequence: Vec<usize> = (0..4).map(|_| table.pick_next().unwrap()).collect();
        assert_eq!(sequence, [2, 0, 2, 0]);
        assert_eq!(table.len(), 2);
    }

    // Catches the table accepting more processes than the manifest limit or
    // losing the rejected process's ownership on overflow.
    #[test]
    fn table_rejects_overflow_and_returns_process() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        for index in 0..MAX_PROCS {
            let process = fixture.spawn("slot");
            assert_eq!(table.insert(process).expect("insert process"), index);
        }
        assert_eq!(table.len(), MAX_PROCS);

        let extra = fixture.spawn("extra");
        let mut rejected = table
            .insert(extra)
            .expect_err("table must reject a fifth process");
        rejected
            .reclaim(&mut fixture.frames)
            .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));
    }

    // Catches the scheduler running a process after its slot was taken, or
    // spinning forever once every process has exited.
    #[test]
    fn table_reports_empty_after_all_exit() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let p0 = fixture.spawn("p0");
        let p1 = fixture.spawn("p1");
        table.insert(p0).expect("insert p0");
        table.insert(p1).expect("insert p1");

        for pid in [0, 1] {
            let mut process = table.take(pid).expect("slot is live");
            process
                .reclaim(&mut fixture.frames)
                .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));
        }

        assert!(table.is_empty());
        assert_eq!(table.pick_next(), None);
    }

    // Catches the scheduler dispatching a stdin-blocked process, or failing
    // to wake it when input arrives: blocked slots must be skipped by
    // pick_next, and wake_all_blocked must return them to the rotation.
    #[test]
    fn blocked_slots_are_skipped_and_woken() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let p0 = fixture.spawn("p0");
        let p1 = fixture.spawn("p1");
        table.insert(p0).expect("insert p0");
        table.insert(p1).expect("insert p1");

        table.get_mut(0).expect("slot 0 is live").block_on_stdin();

        let sequence: Vec<usize> = (0..3).map(|_| table.pick_next().unwrap()).collect();
        assert_eq!(sequence, [1, 1, 1]);

        table.wake_all_blocked();
        let sequence: Vec<usize> = (0..4).map(|_| table.pick_next().unwrap()).collect();
        assert_eq!(sequence, [0, 1, 0, 1]);
    }

    // Catches the all-blocked case collapsing into an empty-table verdict:
    // with live but blocked slots the table must stay non-empty so the
    // scheduler can distinguish "wait for stdin" from "all exited".
    #[test]
    fn all_blocked_slots_still_report_live() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let p0 = fixture.spawn("p0");
        table.insert(p0).expect("insert p0");
        table.get_mut(0).expect("slot 0 is live").block_on_stdin();

        assert_eq!(table.pick_next(), None);
        assert!(!table.is_empty());
    }
}
