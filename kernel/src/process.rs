//! 複数processの所有権とround-robin選択。
//!
//! `UserRun` (一回限りの実行窓) とは別に、`Process`は再入可能な実行単位として
//! image・kernel trap stack・中断contextを所有する。allocatorやframe memoryへの
//! 参照は保持せず、各操作の呼び出し側が都度渡す。`ProcessTable`はheap-backedな
//! live process集合でround-robin選択を担う状態機械であり、host test可能にする。

use alloc::vec::Vec;
use core::fmt;

use crate::pipe::{MAX_PIPES, PipeTable};
#[cfg(not(target_arch = "riscv32"))]
use crate::storage::fat32::FileDesc;
#[cfg(not(target_arch = "riscv32"))]
use crate::user::syscall::{ConsoleFd, FD_TABLE_LEN};
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
    /// pipeの`read`/`write`が条件待ちで中断した。同pipeへのdata
    /// 到着・端のclose・いずれかのprocessのexitで`Runnable`へ戻る。
    /// 値は待つ対象のpipe id。
    BlockedOnPipe(usize),
    /// `sleep`が経過待ちで中断した。値は起床するtick番号で、run loopが
    /// `wake_sleepers`へ渡す現在tickがこれに達すると`Runnable`へ戻る。
    /// syscallは0を書いてecallの次へ進めてあるため、早く起こすと短い
    /// sleepになる。そのためstdin到着の`wake_all_blocked`では起こさない。
    BlockedUntil(u64),
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

/// fd tableのslotの中身。file fdはFAT32位置記述子とoffsetを持ち、
/// pipe fdは`PipeTable`内のpipe idと方向だけを持つ（pipe本体は
/// `ProcessTable`が所有する）。console entryは向きだけを持つ。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy)]
pub enum FdEntry {
    /// 新processのfd 0/1/2が最初に持つconsole entry。
    Console(ConsoleFd),
    /// `open`/`create`が割り当てたfile fd。
    File(FileFd),
    /// `pipe`が割り当てたpipe端。`write`は書き込み端を示す。
    Pipe { id: usize, write: bool },
}

#[cfg(not(target_arch = "riscv32"))]
impl FdEntry {
    /// file fdならその可変参照を返す。pipe端には`None`を返し、
    /// file専用の操作を呼び出し側へ委ねる。
    pub fn file_mut(&mut self) -> Option<&mut FileFd> {
        match self {
            Self::File(file) => Some(file),
            Self::Console(_) | Self::Pipe { .. } => None,
        }
    }

    /// pipe端ならそのpipe idを返す。close時のwake判定に使う。
    pub const fn pipe_id(&self) -> Option<usize> {
        match self {
            Self::Pipe { id, .. } => Some(*id),
            Self::Console(_) | Self::File(_) => None,
        }
    }

    /// console entryならその向きを返す。dispatchの経路選択に使う。
    pub const fn console(&self) -> Option<ConsoleFd> {
        match self {
            Self::Console(console) => Some(*console),
            Self::File(_) | Self::Pipe { .. } => None,
        }
    }
}

/// processごとのfile descriptor table。fd番号はslot indexそのもので、
/// fd 0/1/2もconsole entryを持つ普通のslotである。tableはprocess内に
/// 閉じるため他processのfdを構造的に参照できない。`spawn`はこのtableの
/// snapshotをchildへ渡すため、継承はcopyでありspawn後のoffsetやcloseは
/// 互いに影響しない。ただしpipe端はcopy先が同じpipe idを指すため
/// bufferは共有される。`dup2`も同じくslotのcopyである。
/// RV32はfdを持たないためZSTでコスト0にする。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy)]
pub struct FileFdTable {
    slots: [Option<FdEntry>; FD_TABLE_LEN],
}

#[cfg(target_arch = "riscv32")]
#[derive(Debug, Clone, Copy, Default)]
pub struct FileFdTable;

#[cfg(not(target_arch = "riscv32"))]
impl Default for FileFdTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_arch = "riscv32"))]
impl FileFdTable {
    /// 新processのfd table。fd 0/1/2がconsoleのstdin/stdout/stderrを指し、
    /// 残りは空である。manifestから起動する初期processに使う。
    pub const fn new() -> Self {
        let mut slots = [const { None }; FD_TABLE_LEN];
        slots[minios_abi::syscall::STDIN] = Some(FdEntry::Console(ConsoleFd::Stdin));
        slots[minios_abi::syscall::STDOUT] = Some(FdEntry::Console(ConsoleFd::Stdout));
        slots[minios_abi::syscall::STDERR] = Some(FdEntry::Console(ConsoleFd::Stderr));
        Self { slots }
    }

    fn fd_mut(&mut self, fd: usize) -> Option<&mut FdEntry> {
        self.slots.get_mut(fd)?.as_mut()
    }

    /// `fd`のslotを読む。closeする前にentryの種別をpeekするために使う。
    fn fd(&self, fd: usize) -> Option<&FdEntry> {
        self.slots.get(fd)?.as_ref()
    }

    /// `FIRST_FILE_FD`以上で最小の空きslotへ`entry`を置きfd番号を返す。
    /// fd 0/1/2が空いていても使わないため、`open`/`create`/`pipe`の
    /// 戻り値は常に`FIRST_FILE_FD`以上である。空きがなければ`EMFILE`。
    fn alloc_entry(&mut self, entry: FdEntry) -> Result<usize, isize> {
        let first = minios_abi::syscall::FIRST_FILE_FD;
        let fd = first
            + self.slots[first..]
                .iter()
                .position(Option::is_none)
                .ok_or(minios_abi::syscall::EMFILE)?;
        self.slots[fd] = Some(entry);
        Ok(fd)
    }

    fn alloc_fd(&mut self, desc: FileDesc, writable: bool) -> Result<usize, isize> {
        self.alloc_entry(FdEntry::File(FileFd {
            desc,
            offset: 0,
            writable,
        }))
    }

    /// `id`のpipeの端を`write`方向で割り当てfd番号を返す。
    /// 空きslotがなければ`EMFILE`。
    fn alloc_pipe_fd(&mut self, id: usize, write: bool) -> Result<usize, isize> {
        self.alloc_entry(FdEntry::Pipe { id, write })
    }

    fn close_fd(&mut self, fd: usize) -> bool {
        self.slots
            .get_mut(fd)
            .is_some_and(|slot| slot.take().is_some())
    }

    /// `oldfd`のentryを`newfd`へcopyし、`newfd`に元々あったentryを返す。
    /// 呼び出し側は返ったentryがpipe端ならwaiterを起こす。`oldfd`が
    /// 未割当か、どちらかが範囲外なら`EBADF`。`oldfd == newfd`は何も
    /// 変えずに`None`を返す。
    // ponytail: file entryはoffsetごとcopyするため、POSIXと違いdup2後の
    // 2個のfdはoffsetを共有しない。共有するにはFileFdをprocess横断の
    // open file tableへ移し、slotはその参照を持つ形にする。
    fn dup2(&mut self, oldfd: usize, newfd: usize) -> Result<Option<FdEntry>, isize> {
        use minios_abi::syscall::EBADF;

        let entry = *self.fd(oldfd).ok_or(EBADF)?;
        let slot = self.slots.get_mut(newfd).ok_or(EBADF)?;
        if oldfd == newfd {
            return Ok(None);
        }
        Ok(slot.replace(entry))
    }

    /// `id`のpipeを指すこのtable内の端を`(read側, write側)`で数える。
    /// `ProcessTable`がpipeの生死を全process横断で派生するために使う。
    fn pipe_ends(&self, id: usize) -> (usize, usize) {
        let mut ends = (0, 0);
        for entry in self.slots.iter().flatten() {
            let &FdEntry::Pipe { id: end_id, write } = entry else {
                continue;
            };
            if end_id == id {
                if write {
                    ends.1 += 1;
                } else {
                    ends.0 += 1;
                }
            }
        }
        ends
    }

    /// `dir_location`が指すentryを開いているfdをすべて閉じる。
    /// `unlink`したfileのclusterは即座に解放されるため、残すと再利用
    /// されたslotやclusterを壊し得る。pipe端はdir entryを指さないため
    /// 対象外である。
    fn revoke_fd_at(&mut self, dir_cluster: u32, dir_index: u32) {
        for slot in self.slots.iter_mut() {
            if let Some(FdEntry::File(file)) = slot.as_mut()
                && file.desc.dir_location() == (dir_cluster, dir_index)
            {
                *slot = None;
            }
        }
    }

    /// `(old_cluster, old_index)`を指すfdのwrite-back先を
    /// `(new_cluster, new_index)`へ書き換える。cross-directory moveで
    /// entryが別dirのslotへ移った際、開いているfile fdを追従させる。
    /// pipe端はdir entryを指さないため対象外である。
    fn relocate_fd_at(
        &mut self,
        old_cluster: u32,
        old_index: u32,
        new_cluster: u32,
        new_index: u32,
    ) {
        for entry in self.slots.iter_mut().flatten() {
            if let FdEntry::File(file) = entry
                && file.desc.dir_location() == (old_cluster, old_index)
            {
                file.desc.set_dir_location(new_cluster, new_index);
            }
        }
    }
}

#[cfg(target_arch = "riscv32")]
impl FileFdTable {
    /// 空のfd table。RV32ではfdを持たないため常にZSTを返す。
    pub const fn new() -> Self {
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
    /// `exec`で置き換えられた旧image。trap実行窓では旧callerのsatpが
    /// activeのままなため解放できず、run loopがkernel satpへ戻った直後に
    /// `take_retired_image`で取り出してdestroyする。
    retired_image: Option<LoadedImage>,
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
        file_fds: FileFdTable,
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
            retired_image: None,
            kernel_stack,
            kernel_stack_bottom: stack_bottom.expect("kernel stack has at least one page"),
            user_satp,
            context,
            state: ProcessState::Runnable,
            file_fds,
        })
    }

    /// 呼び出しprocessのimageを`elf`で置き換える (`exec` syscall)。
    /// pid・kernel trap stack・fd tableは引き継ぎ、address spaceと
    /// 初期contextだけを新しくする。
    ///
    /// 新imageの構築をすべて終えてからswapするため、失敗時は旧imageに
    /// 触れずcallerはerrnoを受けて動き続ける。成功時は旧imageを
    /// `retired_image`へ退避し、新imageの初期contextを返す。
    /// 退避したimageはtrap実行窓ではまだ旧satpがactiveなため解放
    /// できず、run loopがkernel satpへ戻った直後に回収する。
    pub fn exec<M: FrameStore, I: IntoIterator<Item = KernelMapping>>(
        &mut self,
        name: &'static str,
        elf: &[u8],
        arguments: &[&str],
        allocator: &mut dyn FrameSource,
        memory: &mut M,
        kernel_mappings: I,
    ) -> Result<UserContext, SpawnFailure<M::Error>> {
        let image = match load_image_with_kernel_mappings(elf, allocator, memory, kernel_mappings) {
            Ok(image) => image,
            Err(error) => {
                return Err(SpawnFailure {
                    error: SpawnError::Load(error),
                    image: None,
                });
            }
        };

        let initial = match write_initial_argv(image.address_space(), memory, name, arguments) {
            Ok(initial) => initial,
            Err(error) => {
                // kernel stackは既存を使い回すため回収対象はimageだけである。
                return Err(match image.destroy(allocator) {
                    Ok(()) => SpawnFailure {
                        error: SpawnError::Argv(error),
                        image: None,
                    },
                    Err(destroy_error) => {
                        let (frame_error, image) = destroy_error.into_parts();
                        SpawnFailure {
                            error: SpawnError::Cleanup(frame_error),
                            image: Some(image),
                        }
                    }
                });
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

        // ここ以降は失敗しない。旧imageはtrap実行窓では旧satp経由で
        // kernel textが動いているため、解放をrun loopへ遅延する。
        let old = self.image.replace(image);
        debug_assert!(self.retired_image.is_none());
        self.retired_image = old;
        self.user_satp = sv39_satp_bits(user_root);
        self.name = name;
        Ok(context)
    }

    /// `exec`で退避した旧imageを取り出す。run loopがkernel satpへ
    /// 戻った直後に呼んで解放する。退避がなければ`None`。
    pub fn take_retired_image(&mut self) -> Option<LoadedImage> {
        self.retired_image.take()
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

    /// pipeのread/writeで`id`のpipe待ちへ移す。table経由でのみ呼ばれ、
    /// callerがそのpipeの端を持つことはdispatch層が検証済みである。
    pub fn block_on_pipe(&mut self, id: usize) {
        self.state = ProcessState::BlockedOnPipe(id);
    }

    /// pipe待ちの対象id。pipe待ちでなければ`None`。
    pub const fn waiting_on_pipe(&self) -> Option<usize> {
        match self.state {
            ProcessState::BlockedOnPipe(id) => Some(id),
            _ => None,
        }
    }

    /// `sleep`で`deadline` tickまでの経過待ちへ移す。trap窓からのみ呼ばれる。
    pub fn block_until(&mut self, deadline: u64) {
        self.state = ProcessState::BlockedUntil(deadline);
    }

    /// fd tableのsnapshot。`spawn`がchildの初期tableとして引き継ぐため
    /// trap窓から呼ばれる。copyなのでcaller側tableへの影響はない。
    pub const fn file_fds_snapshot(&self) -> FileFdTable {
        self.file_fds
    }

    /// `fd`に対応するslotを借りる。未割り当てや範囲外なら`None`。
    /// trap窓からのみ呼ばれ、借用はその窓内で完結する。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn fd_mut(&mut self, fd: usize) -> Option<&mut FdEntry> {
        self.file_fds.fd_mut(fd)
    }

    /// `fd`のslotを読む。closeする前にpipe端かをpeekするために使う。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn fd(&self, fd: usize) -> Option<&FdEntry> {
        self.file_fds.fd(fd)
    }

    /// file記述子を割り当てfd番号を返す。`writable`のfdは`write`だけを
    /// 受理し、それ以外は`read`だけを受理する。空きslotがなければ`EMFILE`。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn alloc_fd(&mut self, desc: FileDesc, writable: bool) -> Result<usize, isize> {
        self.file_fds.alloc_fd(desc, writable)
    }

    /// `id`のpipe端を`write`方向で割り当てfd番号を返す。
    /// 空きslotがなければ`EMFILE`。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn alloc_pipe_fd(&mut self, id: usize, write: bool) -> Result<usize, isize> {
        self.file_fds.alloc_pipe_fd(id, write)
    }

    /// `fd`を閉じる。未割り当てなら`false`を返す。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn close_fd(&mut self, fd: usize) -> bool {
        self.file_fds.close_fd(fd)
    }

    /// `oldfd`のentryを`newfd`へ複製し、`newfd`が元々持っていたentryを
    /// 返す。規約は`FileFdTable::dup2`と同じである。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn dup2(&mut self, oldfd: usize, newfd: usize) -> Result<Option<FdEntry>, isize> {
        self.file_fds.dup2(oldfd, newfd)
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

    /// `sbrk` syscall。現在imageのbreakを動かし旧breakを返す。breakは
    /// imageに属するため、`exec`は新imageの初期breakへ、`spawn`のchildは
    /// 自分のimageの初期breakから始まる。
    pub fn sbrk<M: FrameStore>(
        &mut self,
        increment: isize,
        allocator: &mut dyn FrameSource,
        memory: &mut M,
    ) -> Result<u64, isize> {
        self.image
            .as_mut()
            .expect("live process retains its loaded image")
            .sbrk(increment, allocator, memory)
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
    /// `exec`退避imageが残っていれば先に解放する。
    pub fn reclaim(&mut self, allocator: &mut dyn FrameSource) -> Result<(), FrameError> {
        if let Some(image) = self.retired_image.take()
            && let Err(error) = image.destroy(allocator)
        {
            let (frame_error, image) = error.into_parts();
            self.retired_image = Some(image);
            return Err(frame_error);
        }
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

/// `ProcessTable::pipe_read`の結果。`Blocked`はcallerを
/// `BlockedOnPipe`へmark済みであることを意味し、control層は
/// `EAGAIN`へ写像してdispatchのBlocked経路へ渡す。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, PartialEq, Eq)]
pub enum PipeReadOutcome {
    /// `n` byte読み出した。
    Read(usize),
    /// write端が残っているためcallerをblockした。
    Blocked,
    /// write端がすべて閉じた。`read`は0を返す。
    Eof,
}

/// `ProcessTable::pipe_write`の結果。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, PartialEq, Eq)]
pub enum PipeWriteOutcome {
    /// `n` byte書き込んだ。
    Wrote(usize),
    /// buffer満杯のためcallerをblockした。
    Blocked,
    /// read端がすべて閉じた。control層は`EPIPE`へ写像する。
    Broken,
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
    /// `pipe` syscallが生成したkernel所有のbuffer。slotの生死は
    /// 全processのfd tableを走査したlive端数で派生する。
    pipes: PipeTable,
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
            pipes: PipeTable::new(),
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

    /// stdinへbyteが届いたら呼び、blocked processをrunnableへ戻す。
    /// pid待ちのprocessも起こされるが、再dispatchで条件未達なら再び
    /// blockする（疑似wakeは許容する）。sleep中のprocessはecallを
    /// やり直さないため疑似wakeが短いsleepになってしまい、対象から外す。
    pub fn wake_all_blocked(&mut self) {
        for process in self.procs.iter_mut() {
            if !process.is_runnable() && !matches!(process.state, ProcessState::BlockedUntil(_)) {
                process.wake();
            }
        }
    }

    /// deadlineが`now`（現在tick）以下のsleep中processを`Runnable`へ戻す。
    /// run loopが`pick_next`の直前に毎回呼ぶ。
    pub fn wake_sleepers(&mut self, now: u64) {
        for process in self.procs.iter_mut() {
            if matches!(process.state, ProcessState::BlockedUntil(deadline) if deadline <= now) {
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
    /// 終了したprocessのpipe端もtableから消えるため、pipe待ちのprocessも
    /// 全て起こしてEOF/EPIPEを再判定させる。無関係なwaiterは再実行で
    /// 条件未達なら再びblockする（疑似wakeは許容する）。
    pub fn wake_on_exit(&mut self, pid: usize) {
        for process in self.procs.iter_mut() {
            if process.waiting_on() == Some(pid) || process.waiting_on_pipe().is_some() {
                process.wake();
            }
        }
    }

    /// `id`のpipeを指す全processのlive端を`(read側, write側)`で数える。
    /// pipeの生死とEOF/EPIPE判定はこの走査から派生する。
    #[cfg(not(target_arch = "riscv32"))]
    fn pipe_ends(&self, id: usize) -> (usize, usize) {
        let mut ends = (0, 0);
        for process in &self.procs {
            let (readers, writers) = process.file_fds.pipe_ends(id);
            ends.0 += readers;
            ends.1 += writers;
        }
        ends
    }

    /// `BlockedOnPipe(id)`のprocessをすべて`Runnable`へ戻す。
    /// pipeへのdata到着・空き発生・端のcloseで呼ぶ。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn wake_pipe_waiters(&mut self, id: usize) {
        for process in self.procs.iter_mut() {
            if process.waiting_on_pipe() == Some(id) {
                process.wake();
            }
        }
    }

    /// 新しいpipeを割り当てidを返す。空slotかlive端0の死んだslotを
    /// 再利用する。全slotがliveなら`None`（callerは`ENOMEM`へ写像）。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn pipe_alloc(&mut self) -> Option<usize> {
        for id in 0..MAX_PIPES {
            if self.pipes.is_vacant(id) || self.pipe_ends(id) == (0, 0) {
                self.pipes.claim(id);
                return Some(id);
            }
        }
        None
    }

    /// pipe `id`から`out`へ読む。dataがあれば`Read(n)`で同pipeの
    /// waiterを起こす。空でwrite端がliveなら`caller`を`BlockedOnPipe`
    /// へmarkして`Blocked`。write端が0なら`Eof`（`read`の0返し）。
    /// `None`はfdが指すpipeがslotにない不変条件違反への防御である。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn pipe_read(
        &mut self,
        caller: usize,
        id: usize,
        out: &mut [u8],
    ) -> Option<PipeReadOutcome> {
        if self.pipes.get_mut(id)?.is_empty() {
            if self.pipe_ends(id).1 > 0 {
                self.get_mut(caller)
                    .expect("caller pid is live")
                    .block_on_pipe(id);
                return Some(PipeReadOutcome::Blocked);
            }
            return Some(PipeReadOutcome::Eof);
        }
        let n = self.pipes.get_mut(id).expect("pipe exists above").read(out);
        self.wake_pipe_waiters(id);
        Some(PipeReadOutcome::Read(n))
    }

    /// pipe `id`へ`data`を書く。read端が0なら`Broken`（`EPIPE`）、
    /// 空きがあれば`Wrote(n)`でwaiterを起こし、満杯なら`caller`を
    /// `BlockedOnPipe`へmarkして`Blocked`。`data`が空ならpipeの状態に
    /// 関わらず`Wrote(0)`を返す（`write`の0 byte規約）。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn pipe_write(
        &mut self,
        caller: usize,
        id: usize,
        data: &[u8],
    ) -> Option<PipeWriteOutcome> {
        self.pipes.get_mut(id)?;
        if data.is_empty() {
            return Some(PipeWriteOutcome::Wrote(0));
        }
        if self.pipe_ends(id).0 == 0 {
            return Some(PipeWriteOutcome::Broken);
        }
        if self.pipes.get_mut(id).expect("pipe exists above").free() == 0 {
            self.get_mut(caller)
                .expect("caller pid is live")
                .block_on_pipe(id);
            return Some(PipeWriteOutcome::Blocked);
        }
        let n = self
            .pipes
            .get_mut(id)
            .expect("pipe exists above")
            .write(data);
        self.wake_pipe_waiters(id);
        Some(PipeWriteOutcome::Wrote(n))
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
            self.spawn_with_fds(name, FileFdTable::new())
        }

        fn spawn_with_fds(&mut self, name: &'static str, file_fds: FileFdTable) -> Process {
            let bytes = valid_riscv64_elf();
            Process::spawn(
                name,
                &bytes,
                &[],
                file_fds,
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
            FileFdTable::new(),
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

    // Catches exec leaking or swapping before the new image is complete: a
    // successful exec must keep pid and kernel stack, retire the old image
    // without freeing it, and hand back a context that starts at the new
    // entry on a fresh user stack.
    #[test]
    fn exec_swaps_the_image_and_defers_the_old_one() {
        let mut fixture = SpawnFixture::new();
        let mut process = fixture.spawn("proc-exec");
        let old_satp = process.user_satp();
        let old_stack_top = process.kernel_stack_top();
        let allocated_before = fixture.frames.stats().allocated;
        // heap pageは旧imageと一緒にretireされ、destroyで返る。
        let heap_start = process
            .sbrk(0, &mut fixture.frames, &mut fixture.memory)
            .unwrap();
        process
            .sbrk(16, &mut fixture.frames, &mut fixture.memory)
            .unwrap();
        let bytes = valid_riscv64_elf();

        let context = process
            .exec(
                "proc-exec-2",
                &bytes,
                &[],
                &mut fixture.frames,
                &mut fixture.memory,
                core::iter::empty(),
            )
            .unwrap_or_else(|error| panic!("exec must succeed: {error:?}"));

        assert_eq!(process.name(), "proc-exec-2");
        // breakはimageに属するため、新imageは初期breakから始まる。
        assert_eq!(
            process.sbrk(0, &mut fixture.frames, &mut fixture.memory),
            Ok(heap_start)
        );
        assert_eq!(process.kernel_stack_top(), old_stack_top);
        assert_ne!(process.user_satp(), old_satp);
        assert_eq!(process.user_satp() >> 60, 8);
        assert_eq!(context.register(10), 1);
        assert_ne!(context.register(2), 0);
        // 旧imageはretiredへ退避され、まだallocatorへ返っていない。
        assert!(fixture.frames.stats().allocated > allocated_before);
        let retired = process
            .take_retired_image()
            .expect("exec must retire the old image");
        retired
            .destroy(&mut fixture.frames)
            .unwrap_or_else(|error| panic!("retired image must destroy: {error:?}"));
        assert!(process.take_retired_image().is_none());
        assert_eq!(fixture.frames.stats().allocated, allocated_before);
    }

    // Catches exec touching the live image on a failed load: the process
    // must keep its old image, satp, and name so the caller can continue.
    #[test]
    fn exec_failure_leaves_the_old_image_untouched() {
        let mut fixture = SpawnFixture::new();
        let mut process = fixture.spawn("proc-exec-bad");
        let old_satp = process.user_satp();
        let allocated_before = fixture.frames.stats().allocated;

        let failure = match process.exec(
            "never",
            b"not an elf",
            &[],
            &mut fixture.frames,
            &mut fixture.memory,
            core::iter::empty(),
        ) {
            Err(failure) => failure,
            Ok(_) => panic!("a non-ELF payload must fail exec"),
        };

        assert!(matches!(failure.error, SpawnError::Load(_)));
        assert_eq!(process.name(), "proc-exec-bad");
        assert_eq!(process.user_satp(), old_satp);
        assert!(process.take_retired_image().is_none());
        assert_eq!(fixture.frames.stats().allocated, allocated_before);
    }

    // Catches reclaim forgetting the retired image: frames from a replaced
    // image must also return to the allocator when the process dies before
    // the run loop drained the retired slot.
    #[test]
    fn reclaim_releases_the_retired_image_too() {
        let mut fixture = SpawnFixture::new();
        let before = fixture.baseline();
        let mut process = fixture.spawn("proc-exec-c");
        let bytes = valid_riscv64_elf();
        process
            .exec(
                "proc-exec-c2",
                &bytes,
                &[],
                &mut fixture.frames,
                &mut fixture.memory,
                core::iter::empty(),
            )
            .unwrap_or_else(|error| panic!("exec must succeed: {error:?}"));

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

    // Catches a sleeper waking early: wake_sleepers must release it only
    // once the tick reaches its deadline, and stdin's wake_all_blocked must
    // leave it asleep because the syscall already returned 0.
    #[test]
    fn sleepers_wake_only_at_their_deadline() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        table.insert(fixture.spawn("p0")).expect("insert p0");
        table.insert(fixture.spawn("p1")).expect("insert p1");

        table.get_mut(0).expect("slot 0 is live").block_until(5);
        table.wake_all_blocked();
        table.wake_sleepers(4);
        assert_eq!(table.pick_next(), Some(1));
        assert_eq!(table.pick_next(), Some(1));

        table.wake_sleepers(5);
        assert_eq!(table.pick_next(), Some(0));
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
            p0.fd_mut(fd0)
                .and_then(FdEntry::file_mut)
                .map(|fd| fd.desc().dir_location()),
            Some((9, 0))
        );
        assert_eq!(
            p1.fd_mut(fd1)
                .and_then(FdEntry::file_mut)
                .map(|fd| fd.desc().dir_location()),
            Some((8, 7))
        );
        assert!(p1.fd_mut(fd1 + 1).is_none());
    }

    // Catches spawn losing the caller's fd snapshot: a child spawned with the
    // parent's table must see the same fd numbers, descriptors, and offsets,
    // while later parent-side seeks and closes leave the child's copy intact.
    #[test]
    fn spawn_inherits_fd_table_snapshot() {
        let mut fixture = SpawnFixture::new();
        let mut parent = fixture.spawn("fd-parent");
        let fd = parent
            .alloc_fd(FileDesc::for_test(5, 0), false)
            .expect("slot must be free");
        parent
            .fd_mut(fd)
            .and_then(FdEntry::file_mut)
            .expect("parent fd is live")
            .set_offset(4);

        let mut child = fixture.spawn_with_fds("fd-child", parent.file_fds_snapshot());

        let inherited = child
            .fd_mut(fd)
            .and_then(FdEntry::file_mut)
            .expect("child inherits the fd");
        assert_eq!(inherited.desc().dir_location(), (5, 0));
        assert_eq!(inherited.offset(), 4);
        assert!(!inherited.writable());

        parent
            .fd_mut(fd)
            .and_then(FdEntry::file_mut)
            .expect("parent fd is live")
            .set_offset(100);
        assert!(parent.close_fd(fd));
        let still = child
            .fd_mut(fd)
            .and_then(FdEntry::file_mut)
            .expect("child copy survives parent edits");
        assert_eq!(still.offset(), 4);
        assert_eq!(still.desc().dir_location(), (5, 0));
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
        assert!(!process.close_fd(FIRST_FILE_FD + 100));

        // fd 0..2は普通のslotなので閉じられ、二度目は未割当になる。
        assert!(process.close_fd(0));
        assert!(!process.close_fd(0));

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

        let entry = process
            .fd_mut(fd)
            .and_then(FdEntry::file_mut)
            .expect("fd is live");
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

    // Catches pipe_alloc leaking slots or ignoring dead pipes: slots must be
    // handed out while any end is live, and a slot whose ends all closed
    // must be reusable for the next pipe.
    #[test]
    fn pipe_alloc_reuses_slots_whose_ends_all_closed() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let process = fixture.spawn("pipe-proc");
        table.insert(process).expect("insert");

        let id = table.pipe_alloc().expect("slot is free");
        let read_fd = table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, false)
            .expect("slot is free");
        let write_fd = table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, true)
            .expect("slot is free");

        // 端がliveな間はそのslotは再利用されない。
        assert_ne!(table.pipe_alloc(), Some(id));
        assert!(table.get_mut(0).unwrap().close_fd(read_fd));
        assert!(table.get_mut(0).unwrap().close_fd(write_fd));

        // 両端が消えたslotは再利用される。
        assert_eq!(table.pipe_alloc(), Some(id));
    }

    // Catches pipe exhaustion reporting the wrong error: with MAX_PIPES
    // live pipes, pipe_alloc must return None rather than recycle a slot
    // that still has ends.
    #[test]
    fn pipe_alloc_refuses_when_every_slot_has_live_ends() {
        use crate::pipe::MAX_PIPES;

        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let process = fixture.spawn("pipe-full");
        table.insert(process).expect("insert");

        for _ in 0..MAX_PIPES {
            let id = table.pipe_alloc().expect("slot is free");
            table
                .get_mut(0)
                .unwrap()
                .alloc_pipe_fd(id, true)
                .expect("slot is free");
        }
        assert_eq!(table.pipe_alloc(), None);
    }

    // Catches the shared-buffer contract breaking across inherited fds: a
    // child spawned with the parent's snapshot must write into the same
    // pipe the parent's read end drains, and the write must wake a
    // pipe-blocked reader.
    #[test]
    fn inherited_pipe_ends_share_the_buffer_and_wake_readers() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let mut parent = fixture.spawn("pipe-parent");
        let id = table.pipe_alloc().expect("slot is free");
        parent.alloc_pipe_fd(id, false).expect("read end");
        parent.alloc_pipe_fd(id, true).expect("write end");
        table.insert(parent).expect("insert parent");

        // childはparentのfd snapshotを継承し、両端がlivenessに計上される。
        let snapshot = table.get(0).unwrap().file_fds_snapshot();
        let child = fixture.spawn_with_fds("pipe-child", snapshot);
        table.insert(child).expect("insert child");
        assert_eq!(table.pipe_ends(id), (2, 2));

        // 空なのでparentのreadはblockし、BlockedOnPipeへmarkされる。
        let mut out = [0u8; 8];
        assert_eq!(
            table.pipe_read(0, id, &mut out),
            Some(PipeReadOutcome::Blocked)
        );
        assert_eq!(table.get(0).unwrap().waiting_on_pipe(), Some(id));
        assert_eq!(table.pick_next(), Some(1));

        // childのwriteはbufferを共有し、blockしたreaderを起こす。
        assert_eq!(
            table.pipe_write(1, id, b"ping"),
            Some(PipeWriteOutcome::Wrote(4))
        );
        assert!(table.get(0).unwrap().is_runnable());
        assert_eq!(
            table.pipe_read(0, id, &mut out),
            Some(PipeReadOutcome::Read(4))
        );
        assert_eq!(&out[..4], b"ping");
    }

    // Catches dup2 losing an end or the console: a fresh table must hold
    // console entries at 0..2, a duplicated pipe end must count as a live
    // end until every copy closes, and bad fds must fail before any change.
    #[test]
    fn dup2_copies_entries_and_duplicated_pipe_ends_stay_live() {
        use minios_abi::syscall::{EBADF, STDIN, STDOUT};

        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        table.insert(fixture.spawn("dup")).expect("insert");
        let id = table.pipe_alloc().expect("slot is free");
        let process = table.get_mut(0).unwrap();
        assert_eq!(
            process.fd(STDIN).and_then(FdEntry::console),
            Some(ConsoleFd::Stdin)
        );
        let read_fd = process.alloc_pipe_fd(id, false).expect("read end");
        let write_fd = process.alloc_pipe_fd(id, true).expect("write end");

        let replaced = process.dup2(write_fd, STDOUT).expect("dup2 succeeds");
        assert_eq!(
            replaced.as_ref().and_then(FdEntry::console),
            Some(ConsoleFd::Stdout)
        );
        assert!(matches!(process.dup2(read_fd, read_fd), Ok(None)));
        assert_eq!(process.dup2(write_fd + 1, STDOUT).err(), Some(EBADF));
        assert_eq!(process.dup2(write_fd, FD_TABLE_LEN).err(), Some(EBADF));
        assert_eq!(table.pipe_ends(id), (1, 2));

        // 片方を閉じてもwrite端は残り、両方を閉じて初めて0になる。
        let process = table.get_mut(0).unwrap();
        assert!(process.close_fd(write_fd));
        assert_eq!(table.pipe_ends(id), (1, 1));
        assert!(table.get_mut(0).unwrap().close_fd(STDOUT));
        assert_eq!(table.pipe_ends(id), (1, 0));
    }

    // Catches EOF and broken-pipe polarity flipping: reads must block only
    // while a write end is live and report EOF once all writers close,
    // while writes must report Broken once all readers close.
    #[test]
    fn pipe_read_eof_and_write_broken_follow_open_ends() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let process = fixture.spawn("pipe-ends");
        table.insert(process).expect("insert");

        let id = table.pipe_alloc().expect("slot is free");
        let read_fd = table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, false)
            .expect("read end");
        let write_fd = table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, true)
            .expect("write end");

        let mut out = [0u8; 4];
        assert_eq!(
            table.pipe_read(0, id, &mut out),
            Some(PipeReadOutcome::Blocked)
        );
        table.get_mut(0).unwrap().wake();

        // write端を閉じるとreadはEOFへ転じる。
        assert!(table.get_mut(0).unwrap().close_fd(write_fd));
        assert_eq!(table.pipe_read(0, id, &mut out), Some(PipeReadOutcome::Eof));

        // read端を閉じるとwriteはBrokenへ転じる。
        let id2 = table.pipe_alloc().expect("fresh slot");
        let read_fd2 = table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id2, false)
            .expect("read end");
        table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id2, true)
            .expect("write end");
        assert_eq!(
            table.pipe_write(0, id2, b"x"),
            Some(PipeWriteOutcome::Wrote(1))
        );
        assert!(table.get_mut(0).unwrap().close_fd(read_fd2));
        assert_eq!(
            table.pipe_write(0, id2, b"x"),
            Some(PipeWriteOutcome::Broken)
        );
        // 0 byte writeはEPIPEにならない。
        assert_eq!(
            table.pipe_write(0, id2, b""),
            Some(PipeWriteOutcome::Wrote(0))
        );
    }

    // Catches a writer blocking forever on a full pipe: a full buffer must
    // mark the caller BlockedOnPipe, and a read draining it must wake the
    // writer so the retried write succeeds.
    #[test]
    fn full_pipe_blocks_the_writer_until_a_read_drains_it() {
        use crate::pipe::PIPE_CAPACITY;

        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let process = fixture.spawn("pipe-full-io");
        table.insert(process).expect("insert");

        let id = table.pipe_alloc().expect("slot is free");
        table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, false)
            .expect("read end");
        table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, true)
            .expect("write end");

        let data = alloc::vec![0xAAu8; PIPE_CAPACITY];
        assert_eq!(
            table.pipe_write(0, id, &data),
            Some(PipeWriteOutcome::Wrote(PIPE_CAPACITY))
        );
        assert_eq!(
            table.pipe_write(0, id, b"x"),
            Some(PipeWriteOutcome::Blocked)
        );
        assert_eq!(table.get(0).unwrap().waiting_on_pipe(), Some(id));
        assert_eq!(table.pick_next(), None);

        let mut out = alloc::vec![0u8; PIPE_CAPACITY];
        assert_eq!(
            table.pipe_read(0, id, &mut out),
            Some(PipeReadOutcome::Read(PIPE_CAPACITY))
        );
        assert!(table.get(0).unwrap().is_runnable());
        assert_eq!(
            table.pipe_write(0, id, b"x"),
            Some(PipeWriteOutcome::Wrote(1))
        );
    }

    // Catches pipe waiters being stranded by a peer's exit: wake_on_exit
    // must release BlockedOnPipe processes too, since the exiting process's
    // ends disappear and the waiter's EOF/EPIPE verdict may flip.
    #[test]
    fn wake_on_exit_releases_pipe_waiters() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();

        let mut writer = fixture.spawn("pipe-writer");
        let id = table.pipe_alloc().expect("slot is free");
        writer.alloc_pipe_fd(id, true).expect("write end");
        table.insert(writer).expect("insert writer");

        let mut reader = fixture.spawn("pipe-reader");
        reader.alloc_pipe_fd(id, false).expect("read end");
        table.insert(reader).expect("insert reader");

        let mut out = [0u8; 4];
        assert_eq!(
            table.pipe_read(1, id, &mut out),
            Some(PipeReadOutcome::Blocked)
        );
        assert_eq!(table.get(1).unwrap().waiting_on_pipe(), Some(id));

        table.wake_on_exit(0);
        assert!(table.get(1).unwrap().is_runnable());
    }

    // Catches a stale block_on_stdin clobbering a pipe wait: like the
    // waitpid guard, the run loop's Blocked handling must leave the
    // control-marked BlockedOnPipe state alone.
    #[test]
    fn pipe_block_survives_the_run_loop_stdin_fallback() {
        let mut fixture = SpawnFixture::new();
        let mut table = ProcessTable::new();
        let process = fixture.spawn("pipe-state");
        table.insert(process).expect("insert");

        let id = table.pipe_alloc().expect("slot is free");
        table
            .get_mut(0)
            .unwrap()
            .alloc_pipe_fd(id, true)
            .expect("write end");

        let mut out = [0u8; 4];
        assert_eq!(
            table.pipe_read(0, id, &mut out),
            Some(PipeReadOutcome::Blocked)
        );
        table.get_mut(0).unwrap().block_on_stdin();
        assert_eq!(table.get(0).unwrap().waiting_on_pipe(), Some(id));
        assert!(!table.get(0).unwrap().is_runnable());
    }
}
