//! 複数processの所有権とround-robin選択。
//!
//! `UserRun` (一回限りの実行窓) とは別に、`Process`は再入可能な実行単位として
//! image・kernel trap stack・中断contextを所有する。allocatorやframe memoryへの
//! 参照は保持せず、各操作の呼び出し側が都度渡す。`ProcessTable`はheap-backedな
//! live process集合でround-robin選択を担う状態機械であり、host test可能にする。

use alloc::vec::Vec;
use core::fmt;

#[cfg(not(target_arch = "riscv32"))]
use crate::storage::fat32::FileDesc;
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
    /// `waitpid`が対象processの終了待ちで中断した。対象が終了して
    /// `wake_on_exit`されると`Runnable`へ戻る。値は待つ対象のpid。
    BlockedOnPid(usize),
}

/// `open`/`create`がprocessへ割り当てたfile descriptor 1個。
/// `desc`はFAT32の位置記述子、`offset`は次に読み書きするbyte位置、
/// `writable`は`create`由来のfdを示す。fdはread専用またはwrite専用で、
/// 両方は許さない。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy)]
pub struct FileFd {
    desc: FileDesc,
    offset: u64,
    writable: bool,
}

#[cfg(not(target_arch = "riscv32"))]
impl FileFd {
    /// このfdが指すfileのFAT32位置記述子。
    pub const fn desc(&self) -> &FileDesc {
        &self.desc
    }

    /// `write_range`がsizeやfirst_clusterをwrite-backするための可変参照。
    pub fn desc_mut(&mut self) -> &mut FileDesc {
        &mut self.desc
    }

    /// 次に読み書きするbyte位置。
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// offsetを直接設定する。`lseek`が使う。
    pub fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }

    /// 処理したbyte数だけoffsetを進める。`read`/`write`が使う。
    pub fn advance(&mut self, count: u64) {
        self.offset += count;
    }

    /// `create`由来で`write`を受理するfdかどうか。
    pub const fn writable(&self) -> bool {
        self.writable
    }
}

/// processごとのfile descriptor table。fd番号はslot index +
/// `FIRST_FILE_FD`であり、tableはprocess内に閉じるため他processのfdを
/// 構造的に参照できない。RV32はfdを持たないためZSTでコスト0にする。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug)]
struct FileFdTable {
    slots: [Option<FileFd>; minios_abi::syscall::MAX_OPEN_FILES],
}

#[cfg(target_arch = "riscv32")]
#[derive(Debug)]
struct FileFdTable;

#[cfg(not(target_arch = "riscv32"))]
impl FileFdTable {
    const fn new() -> Self {
        Self {
            slots: [const { None }; minios_abi::syscall::MAX_OPEN_FILES],
        }
    }

    fn fd_mut(&mut self, fd: usize) -> Option<&mut FileFd> {
        let slot = fd.checked_sub(minios_abi::syscall::FIRST_FILE_FD)?;
        self.slots.get_mut(slot)?.as_mut()
    }

    fn alloc_fd(&mut self, desc: FileDesc, writable: bool) -> Result<usize, isize> {
        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(minios_abi::syscall::EMFILE)?;
        self.slots[slot] = Some(FileFd {
            desc,
            offset: 0,
            writable,
        });
        Ok(minios_abi::syscall::FIRST_FILE_FD + slot)
    }

    fn close_fd(&mut self, fd: usize) -> bool {
        let Some(slot) = fd.checked_sub(minios_abi::syscall::FIRST_FILE_FD) else {
            return false;
        };
        match self.slots.get_mut(slot) {
            Some(slot) => slot.take().is_some(),
            None => false,
        }
    }

    /// `dir_location`が指すentryを開いているfdをすべて閉じる。
    /// `unlink`したfileのclusterは即座に解放されるため、残すと再利用
    /// されたslotやclusterを壊し得る。
    fn revoke_fd_at(&mut self, dir_cluster: u32, dir_index: u32) {
        for slot in self.slots.iter_mut() {
            if let Some(fd) = slot
                && fd.desc.dir_location() == (dir_cluster, dir_index)
            {
                *slot = None;
            }
        }
    }

    /// `(old_cluster, old_index)`を指すfdのwrite-back先を
    /// `(new_cluster, new_index)`へ書き換える。cross-directory moveで
    /// entryが別dirのslotへ移った際、開いているfile fdを追従させる。
    fn relocate_fd_at(
        &mut self,
        old_cluster: u32,
        old_index: u32,
        new_cluster: u32,
        new_index: u32,
    ) {
        for fd in self.slots.iter_mut().flatten() {
            if fd.desc.dir_location() == (old_cluster, old_index) {
                fd.desc.set_dir_location(new_cluster, new_index);
            }
        }
    }
}

#[cfg(target_arch = "riscv32")]
impl FileFdTable {
    const fn new() -> Self {
        Self
    }
}

/// 再入可能なuser process。image (user address spaceとその所有frame)、
/// 専用kernel trap stack、前回中断時の`UserContext`、file descriptor
/// tableを所有する。
///
/// allocator/frame memoryへの参照は保持しないため、生存中のprocess同士が
/// borrowを共有せず、tableが複数のprocessを同時に抱えられる。
/// `dispatch`中だけkernelが当該stackを使い、戻った時点でcontextをslotから
/// 回収する (`reload_context`)。fd tableはprocess dropとともに死ぬため、
/// 終了時の明示的なcloseは要らない。
/// `pid`は`ProcessTable::insert`が採番する。`usize::MAX`は未割当を示す。
pub struct Process {
    name: &'static str,
    pid: usize,
    image: Option<LoadedImage>,
    kernel_stack: [Option<PhysFrame>; KERNEL_STACK_PAGES],
    kernel_stack_bottom: usize,
    user_satp: u64,
    context: UserContext,
    state: ProcessState,
    file_fds: FileFdTable,
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
            pid: usize::MAX,
            image: Some(image),
            kernel_stack,
            kernel_stack_bottom: stack_bottom.expect("kernel stack has at least one page"),
            user_satp,
            context,
            state: ProcessState::Runnable,
            file_fds: FileFdTable::new(),
        })
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// `ProcessTable::insert`が採番したpid。未insertなら`usize::MAX`。
    pub const fn pid(&self) -> usize {
        self.pid
    }

    /// stdin待ちへ移す。`dispatch`が`Blocked`を返した直後に呼ぶ。
    /// `waitpid`が先に`BlockedOnPid`へ移した場合は遷移しない。
    pub fn block_on_stdin(&mut self) {
        if self.is_runnable() {
            self.state = ProcessState::BlockedOnStdin;
        }
    }

    /// `waitpid`で`pid`の終了待ちへ移す。trap窓のtable経由でのみ呼ばれる。
    pub fn block_on_pid(&mut self, pid: usize) {
        self.state = ProcessState::BlockedOnPid(pid);
    }

    /// `waitpid`で待っている対象pid。pid待ちでなければ`None`。
    pub const fn waiting_on(&self) -> Option<usize> {
        match self.state {
            ProcessState::BlockedOnPid(pid) => Some(pid),
            _ => None,
        }
    }

    /// `fd`に対応するslotを借りる。未割り当てや範囲外なら`None`。
    /// trap窓からのみ呼ばれ、借用はその窓内で完結する。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn fd_mut(&mut self, fd: usize) -> Option<&mut FileFd> {
        self.file_fds.fd_mut(fd)
    }

    /// file記述子を割り当てfd番号を返す。`writable`のfdは`write`だけを
    /// 受理し、それ以外は`read`だけを受理する。空きslotがなければ`EMFILE`。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn alloc_fd(&mut self, desc: FileDesc, writable: bool) -> Result<usize, isize> {
        self.file_fds.alloc_fd(desc, writable)
    }

    /// `fd`を閉じる。未割り当てなら`false`を返す。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn close_fd(&mut self, fd: usize) -> bool {
        self.file_fds.close_fd(fd)
    }

    /// `(dir_cluster, dir_index)`のdir entryを指すfdを閉じる。
    /// `unlink`成功後にtable経由で呼ばれる。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn revoke_fd_at(&mut self, dir_cluster: u32, dir_index: u32) {
        self.file_fds.revoke_fd_at(dir_cluster, dir_index);
    }

    /// `(old_cluster, old_index)`を指すfdのwrite-back先を
    /// `(new_cluster, new_index)`へ書き換える。cross-directory move
    /// 成功後にtable経由で呼ばれる。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn relocate_fd_at(
        &mut self,
        old_cluster: u32,
        old_index: u32,
        new_cluster: u32,
        new_index: u32,
    ) {
        self.file_fds
            .relocate_fd_at(old_cluster, old_index, new_cluster, new_index);
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

/// heap-backedのprocess table。`Vec`がlive processだけを保持し、
/// pidは`insert`のたびに`next_pid`から単調採番される。終了したpidは
/// 再利用されないため、PROC_EXIT frameや将来のspawn syscallが参照する
/// pidと新processのpidは衝突しない。`last_picked`は直前にdispatchした
/// pidを保持し、removeによる詰め直しに左右されない再開点となる。
pub struct ProcessTable {
    procs: Vec<Process>,
    next_pid: usize,
    last_picked: Option<usize>,
    /// 終了したprocessの`(pid, exit code)`。`waitpid`が1回だけ消費する
    /// 未回収statusの台帳。上限`MAX_PROCS`件で、超過時は最古をdropする
    /// （dropされたpidへの`waitpid`は`ECHILD`を返す）。
    exits: Vec<(usize, u32)>,
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    /// `MAX_PROCS`分のcapacityを先に確保する。`insert`はlive数を
    /// `MAX_PROCS`へ制限するため、以後のpushはreallocを起こさず、
    /// `get_mut`が返す要素pointerがtableの生存期間中ずっと安定する。
    /// これはtrap窓が`Process`へのraw pointerを保持し、その窓内で
    /// `spawn`が`insert`し得る設計の不変条件である。
    pub fn new() -> Self {
        Self {
            procs: Vec::with_capacity(MAX_PROCS),
            next_pid: 0,
            last_picked: None,
            exits: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.procs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.procs.is_empty()
    }

    /// processへpidを採番してtableへ登録し、そのpidを返す。
    /// live数が`MAX_PROCS` (manifest上限) に達しているかpid採番が
    /// overflowした場合はprocessをそのまま返す。
    ///
    /// Errがprocess全体を返すのは、callerが拒否されたprocessの所有frameを
    /// 回収するためであり、`deallocate_recoverable`が失敗時にframeを返すのと
    /// 同じ規約である。
    #[allow(clippy::result_large_err)]
    pub fn insert(&mut self, mut process: Process) -> Result<usize, Process> {
        if self.procs.len() == MAX_PROCS {
            return Err(process);
        }
        let Some(next) = self.next_pid.checked_add(1) else {
            return Err(process);
        };
        let pid = self.next_pid;
        self.next_pid = next;
        process.pid = pid;
        self.procs.push(process);
        Ok(pid)
    }

    pub fn get(&self, pid: usize) -> Option<&Process> {
        self.procs.iter().find(|process| process.pid == pid)
    }

    pub fn get_mut(&mut self, pid: usize) -> Option<&mut Process> {
        self.procs.iter_mut().find(|process| process.pid == pid)
    }

    /// tableからprocessを取り出す。呼び出し側が`reclaim`で所有frameを
    /// 回収する。`swap_remove`ではなく`remove`を使い、残るprocessの
    /// 並び (round-robin順) を変えない。
    pub fn take(&mut self, pid: usize) -> Option<Process> {
        let index = self.procs.iter().position(|process| process.pid == pid)?;
        Some(self.procs.remove(index))
    }

    /// 先頭のprocessを取り出す。table全回収のdrain経路で使う。
    pub fn take_oldest(&mut self) -> Option<Process> {
        if self.procs.is_empty() {
            return None;
        }
        Some(self.procs.remove(0))
    }

    /// live processをmanifest順に走査する。spawn一覧の表示に使う。
    pub fn iter(&self) -> impl Iterator<Item = &Process> {
        self.procs.iter()
    }

    /// 前回dispatchしたpidの次から時計回りに走査し、最初のrunnable
    /// processのpidを返す。processが空か、すべてblockedなら`None`を返す。
    /// `last_picked`が既にtableを出ていれば先頭から走査する。
    pub fn pick_next(&mut self) -> Option<usize> {
        let start = self
            .last_picked
            .and_then(|last| self.procs.iter().position(|process| process.pid == last))
            .map(|index| index + 1)
            .unwrap_or(0);
        for offset in 0..self.procs.len() {
            let index = (start + offset) % self.procs.len();
            if self.procs[index].is_runnable() {
                self.last_picked = Some(self.procs[index].pid);
                return Some(self.procs[index].pid);
            }
        }
        None
    }

    /// processがありながら`pick_next`が`None`＝全processがstdin待ち。
    /// stdinへbyteが届いたら呼び、blocked processをすべてrunnableへ戻す。
    /// pid待ちのprocessも起こされるが、再dispatchで条件未達なら再び
    /// blockする（疑似wakeは許容する）。
    pub fn wake_all_blocked(&mut self) {
        for process in self.procs.iter_mut() {
            if !process.is_runnable() {
                process.wake();
            }
        }
    }

    /// 終了したprocessの`(pid, code)`を台帳へ記録する。run loopのexit
    /// 経路から呼ぶ。`MAX_PROCS`件を越える場合は最古をdropする。
    pub fn record_exit(&mut self, pid: usize, code: u32) {
        if self.exits.len() == MAX_PROCS {
            self.exits.remove(0);
        }
        self.exits.push((pid, code));
    }

    /// `pid`の未回収statusを消費して返す。未記録か消費済みなら`None`。
    pub fn take_exit(&mut self, pid: usize) -> Option<u32> {
        let index = self.exits.iter().position(|(p, _)| *p == pid)?;
        Some(self.exits.remove(index).1)
    }

    /// `pid`が終了したときに呼び、`BlockedOnPid(pid)`のprocessをすべて
    /// `Runnable`へ戻す。statusの有無に関わらずwaiterは解放される。
    pub fn wake_on_exit(&mut self, pid: usize) {
        for process in self.procs.iter_mut() {
            if process.waiting_on() == Some(pid) {
                process.wake();
            }
        }
    }

    /// `caller`を`target`の終了待ちへ移す。`ECHILD`は自分自身・対象
    /// 不在（未記録のpid）、`EINVAL`はwait連鎖がcallerへ戻るcycle。
    /// live確認とcycle検査をblockへの遷移より先に行う。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn block_waitpid(&mut self, caller: usize, target: usize) -> Result<(), isize> {
        use minios_abi::syscall::{ECHILD, EINVAL};

        if caller == target {
            return Err(ECHILD);
        }
        if self.get(target).is_none() {
            return Err(ECHILD);
        }
        // targetのwait連鎖を辿り、callerへ戻る経路があればcycle。
        // 連鎖はblocked processの数だけ辿れば必ず終端かcycleへ着く。
        let mut cursor = target;
        for _ in 0..self.procs.len() {
            let Some(next) = self
                .procs
                .iter()
                .find(|process| process.pid == cursor)
                .and_then(Process::waiting_on)
            else {
                break;
            };
            if next == caller {
                return Err(EINVAL);
            }
            cursor = next;
        }
        self.get_mut(caller)
            .expect("caller pid is live")
            .block_on_pid(target);
        Ok(())
    }

    /// `(dir_cluster, dir_index)`のdir entryを指すfdを全processから閉じる。
    /// `unlink`したfileのclusterは即座に解放されるため、開いたままのfdを
    /// 残すと再利用されたslotやclusterを壊し得る。呼び出しprocess自身の
    /// fdも失効する。trap窓からのみ呼ばれる。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn revoke_file_fds(&mut self, dir_cluster: u32, dir_index: u32) {
        for process in self.procs.iter_mut() {
            process.revoke_fd_at(dir_cluster, dir_index);
        }
    }

    /// `(old_cluster, old_index)`のdir entryを指すfdを全processで
    /// `(new_cluster, new_index)`へ追従させる。cross-directory moveで
    /// entryの物理位置が変わってもopen fileは有効のままにするための
    /// POSIX不変条件。呼び出しprocess自身のfdも対象になる。
    /// trap窓からのみ呼ばれる。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn relocate_file_fds(
        &mut self,
        old_cluster: u32,
        old_index: u32,
        new_cluster: u32,
        new_index: u32,
    ) {
        for process in self.procs.iter_mut() {
            process.relocate_fd_at(old_cluster, old_index, new_cluster, new_index);
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

    // Catches the exit ledger losing a status or letting one be reaped
    // twice: record_exit must make exactly one take_exit succeed per exit,
    // and unknown or already-reaped pids must come back empty.
    #[test]
    fn exit_ledger_records_and_reaps_each_status_once() {
        let mut table = ProcessTable::new();

        table.record_exit(7, 42);
        table.record_exit(8, 3);

        assert_eq!(table.take_exit(7), Some(42));
        assert_eq!(table.take_exit(7), None);
        assert_eq!(table.take_exit(9), None);
        assert_eq!(table.take_exit(8), Some(3));
    }

    // Catches the ledger growing without bound or evicting the wrong entry:
    // past MAX_PROCS records the oldest status must be dropped so a waiter
    // on it sees ECHILD, while newer statuses stay reapable.
    #[test]
    fn exit_ledger_is_bounded_and_drops_the_oldest() {
        let mut table = ProcessTable::new();

        for index in 0..MAX_PROCS + 1 {
            table.record_exit(index, index as u32);
        }

        assert_eq!(table.take_exit(0), None);
        assert_eq!(table.take_exit(MAX_PROCS), Some(MAX_PROCS as u32));
    }

    // Catches waitpid blocking the caller before checking the target, or
    // failing to skip a pid-blocked slot: block_waitpid must mark the
    // caller BlockedOnPid only for a live, distinct target, pick_next
    // must skip it, and a stale block_on_stdin must not clobber the wait.
    #[test]
    fn waitpid_blocks_the_caller_and_survives_stdin_wakeup() {
        use minios_abi::syscall::ECHILD;

        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let p0 = fixture.spawn("parent");
        let p1 = fixture.spawn("child");
        table.insert(p0).expect("insert parent");
        table.insert(p1).expect("insert child");

        table.block_waitpid(0, 1).expect("live child must block");
        assert_eq!(table.get(0).unwrap().waiting_on(), Some(1));
        assert_eq!(table.pick_next(), Some(1));

        // dispatchが`Blocked`を返した後にrun loopが呼ぶblock_on_stdinは、
        // controlが先にmarkしたpid待ちをstdin待ちへ書き換えてはならない。
        table.get_mut(0).unwrap().block_on_stdin();
        assert_eq!(table.get(0).unwrap().waiting_on(), Some(1));

        // stdin到着の疑似wakeはpid待ちも一度runnableへ戻すが、再実行された
        // waitpidがlive targetを見て再びBlockedOnPidへmarkするため許容する。
        table.wake_all_blocked();
        assert!(table.get(0).unwrap().is_runnable());
        table.block_waitpid(0, 1).expect("retry must re-block");
        assert_eq!(table.get(0).unwrap().waiting_on(), Some(1));

        // 自分自身や存在しないpidへのwaitはblockせずECHILDを返す。
        assert_eq!(table.block_waitpid(0, 0), Err(ECHILD));
        assert_eq!(table.block_waitpid(0, 99), Err(ECHILD));
        assert_eq!(table.get(0).unwrap().waiting_on(), Some(1));
    }

    // Catches a waiter never being released: when the target exits,
    // wake_on_exit must return every BlockedOnPid waiter to the rotation.
    #[test]
    fn waiters_wake_when_the_target_exits() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        for name in ["w0", "w1", "child"] {
            let process = fixture.spawn(name);
            table.insert(process).expect("insert");
        }
        table.block_waitpid(0, 2).expect("live child must block");
        table.block_waitpid(1, 2).expect("live child must block");
        assert_eq!(table.pick_next(), Some(2));

        table.wake_on_exit(2);
        let sequence: Vec<usize> = (0..3).map(|_| table.pick_next().unwrap()).collect();
        assert_eq!(sequence, [0, 1, 2]);
    }

    // Catches a wait cycle deadlocking the whole table: if following the
    // target's wait chain returns to the caller, block_waitpid must reject
    // with EINVAL and leave the caller runnable instead of blocking forever.
    #[test]
    fn waitpid_rejects_wait_cycles() {
        use minios_abi::syscall::EINVAL;

        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        for name in ["a", "b", "c"] {
            let process = fixture.spawn(name);
            table.insert(process).expect("insert");
        }
        table.block_waitpid(0, 1).expect("a waits b");
        table.block_waitpid(1, 2).expect("b waits c");

        assert_eq!(table.block_waitpid(2, 0), Err(EINVAL));
        assert!(table.get(2).unwrap().is_runnable());
        assert_eq!(table.block_waitpid(2, 1), Err(EINVAL));
        assert!(table.get(2).unwrap().is_runnable());
    }

    // Catches fd numbering or capacity drifting: a fresh process must
    // allocate fds upward from FIRST_FILE_FD and reject the allocation past
    // MAX_OPEN_FILES with EMFILE.
    #[test]
    fn process_allocates_fds_up_to_the_table_limit() {
        use minios_abi::syscall::{EMFILE, FIRST_FILE_FD, MAX_OPEN_FILES};

        let mut fixture = SpawnFixture::new();
        let mut process = fixture.spawn("fd-proc");

        for index in 0..MAX_OPEN_FILES {
            let fd = process
                .alloc_fd(FileDesc::for_test(9, index as u32), false)
                .expect("slot must be free");
            assert_eq!(fd, FIRST_FILE_FD + index);
        }
        assert_eq!(
            process.alloc_fd(FileDesc::for_test(9, 99), false),
            Err(EMFILE)
        );
    }

    // Catches fd state leaking between processes: each process must see only
    // its own table, so the same fd number resolves to different descriptors
    // and one process's allocation must not occupy another's slots.
    #[test]
    fn fd_tables_are_isolated_per_process() {
        use minios_abi::syscall::FIRST_FILE_FD;

        let mut fixture = SpawnFixture::new();
        let mut p0 = fixture.spawn("fd-a");
        let mut p1 = fixture.spawn("fd-b");

        let fd0 = p0
            .alloc_fd(FileDesc::for_test(9, 0), false)
            .expect("p0 slot must be free");
        let fd1 = p1
            .alloc_fd(FileDesc::for_test(8, 7), true)
            .expect("p1 slot must be free");
        assert_eq!(fd0, FIRST_FILE_FD);
        assert_eq!(fd1, FIRST_FILE_FD);

        assert_eq!(
            p0.fd_mut(fd0).map(|fd| fd.desc().dir_location()),
            Some((9, 0))
        );
        assert_eq!(
            p1.fd_mut(fd1).map(|fd| fd.desc().dir_location()),
            Some((8, 7))
        );
        assert!(p1.fd_mut(fd1 + 1).is_none());
    }

    // Catches close leaving a slot stuck or double-close reporting success:
    // closed fds must fail lookup, report false on re-close, free the slot
    // for reuse, and reject out-of-range numbers.
    #[test]
    fn close_fd_frees_the_slot_and_rejects_unknown_fds() {
        use minios_abi::syscall::FIRST_FILE_FD;

        let mut fixture = SpawnFixture::new();
        let mut process = fixture.spawn("fd-close");

        let fd = process
            .alloc_fd(FileDesc::for_test(9, 0), false)
            .expect("slot must be free");
        assert!(process.close_fd(fd));
        assert!(!process.close_fd(fd));
        assert!(process.fd_mut(fd).is_none());
        assert!(!process.close_fd(0));
        assert!(!process.close_fd(FIRST_FILE_FD + 100));

        let again = process
            .alloc_fd(FileDesc::for_test(9, 1), false)
            .expect("closed slot must be reusable");
        assert_eq!(again, fd);
    }

    // Catches fd bookkeeping losing the position or direction: offset must
    // advance by I/O counts, lseek must overwrite it, and the writable flag
    // must survive.
    #[test]
    fn fd_tracks_offset_and_direction() {
        let mut fixture = SpawnFixture::new();
        let mut process = fixture.spawn("fd-io");
        let fd = process
            .alloc_fd(FileDesc::for_test(9, 0), true)
            .expect("slot must be free");

        let entry = process.fd_mut(fd).expect("fd is live");
        assert!(entry.writable());
        assert_eq!(entry.offset(), 0);
        entry.advance(5);
        assert_eq!(entry.offset(), 5);
        entry.set_offset(100);
        assert_eq!(entry.offset(), 100);
        assert_eq!(entry.desc().dir_location(), (9, 0));
    }

    // Catches unlink revoking only the caller's table or skipping writable
    // fds: revocation must cross every live process and every direction,
    // while descriptors for other entries stay open.
    #[test]
    fn revoke_file_fds_crosses_process_boundaries() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let mut p0 = fixture.spawn("fd-r0");
        let mut p1 = fixture.spawn("fd-r1");

        let keep0 = p0
            .alloc_fd(FileDesc::for_test(9, 1), false)
            .expect("p0 slot must be free");
        let gone0 = p0
            .alloc_fd(FileDesc::for_test(4, 2), false)
            .expect("p0 slot must be free");
        let gone1 = p1
            .alloc_fd(FileDesc::for_test(4, 2), true)
            .expect("p1 slot must be free");
        let keep1 = p1
            .alloc_fd(FileDesc::for_test(9, 3), false)
            .expect("p1 slot must be free");
        table.insert(p0).expect("insert p0");
        table.insert(p1).expect("insert p1");

        table.revoke_file_fds(4, 2);

        let p0 = table.get_mut(0).expect("p0 is live");
        assert!(p0.fd_mut(gone0).is_none());
        assert!(p0.fd_mut(keep0).is_some());
        let p1 = table.get_mut(1).expect("p1 is live");
        assert!(p1.fd_mut(gone1).is_none());
        assert!(p1.fd_mut(keep1).is_some());
    }

    // Catches the fd table outliving its process: a process spawned after a
    // predecessor exited must start with an empty table instead of
    // inheriting the predecessor's descriptors.
    #[test]
    fn fresh_process_starts_with_empty_fd_table() {
        use minios_abi::syscall::{FIRST_FILE_FD, MAX_OPEN_FILES};

        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let mut p0 = fixture.spawn("fd-old");
        p0.alloc_fd(FileDesc::for_test(9, 0), false)
            .expect("slot must be free");
        p0.alloc_fd(FileDesc::for_test(9, 1), false)
            .expect("slot must be free");
        table.insert(p0).expect("insert p0");

        let mut exited = table.take(0).expect("pid 0 is live");
        exited
            .reclaim(&mut fixture.frames)
            .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));

        let p1 = fixture.spawn("fd-new");
        let pid = table.insert(p1).expect("insert p1");
        let fresh = table.get_mut(pid).expect("p1 is live");
        for fd in FIRST_FILE_FD..FIRST_FILE_FD + MAX_OPEN_FILES {
            assert!(fresh.fd_mut(fd).is_none());
        }
    }

    // Catches pid reuse sneaking back in: a process inserted after another
    // exited must get a fresh monotonic pid, so an exit frame's pid can
    // never name a different live process.
    #[test]
    fn insert_assigns_fresh_monotonic_pids() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let p0 = fixture.spawn("mono-a");
        let p1 = fixture.spawn("mono-b");
        assert_eq!(table.insert(p0).expect("insert p0"), 0);
        assert_eq!(table.insert(p1).expect("insert p1"), 1);
        assert_eq!(table.get(0).expect("p0 is live").pid(), 0);

        let mut exited = table.take(0).expect("pid 0 is live");
        exited
            .reclaim(&mut fixture.frames)
            .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));

        let p2 = fixture.spawn("mono-c");
        let pid = table.insert(p2).expect("insert p2");
        assert_eq!(pid, 2);
        assert_eq!(table.get(2).expect("p2 is live").pid(), 2);
        assert!(table.get(0).is_none());
    }

    // Catches the drain path skipping live processes: take_oldest must pop
    // the earliest-spawned process each time until the table is empty.
    #[test]
    fn take_oldest_drains_in_spawn_order() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        for name in ["d0", "d1", "d2"] {
            let process = fixture.spawn(name);
            table.insert(process).expect("insert process");
        }

        for expected_pid in 0..3usize {
            let mut process = table.take_oldest().expect("process must be live");
            assert_eq!(process.pid(), expected_pid);
            process
                .reclaim(&mut fixture.frames)
                .unwrap_or_else(|error| panic!("reclaim must succeed: {error:?}"));
        }
        assert!(table.take_oldest().is_none());
        assert!(table.is_empty());
    }

    // Catches the Vec ever re-allocating while it holds live processes: the
    // scheduler keeps a raw pointer into `procs` across a syscall dispatch
    // that may itself insert a spawned process, so filling the table must
    // not move existing entries.
    #[test]
    fn table_fill_never_moves_live_processes() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let mut pointers = Vec::new();

        for index in 0..MAX_PROCS {
            let name = ["fill-", "0123456789abcdef".get(index..index + 1).unwrap()].concat();
            let process = fixture.spawn(Box::leak(name.into_boxed_str()));
            table.insert(process).expect("insert process");
            pointers.push(table.get(index).expect("process is live") as *const Process);
        }

        for (pid, pointer) in pointers.iter().enumerate() {
            let process = table.get(pid).expect("process is live");
            assert_eq!(process as *const Process, *pointer);
        }
    }
}
