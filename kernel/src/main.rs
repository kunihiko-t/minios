#![no_std]
#![no_main]

#[cfg(any(
    all(
        feature = "qemu-test-boot",
        any(
            feature = "qemu-test-timer",
            feature = "qemu-test-trap",
            feature = "qemu-test-memory",
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-timer",
        any(
            feature = "qemu-test-trap",
            feature = "qemu-test-memory",
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-trap",
        any(
            feature = "qemu-test-memory",
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-memory",
        any(
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-vm",
        any(
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-elf",
        any(
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-user-entry",
        any(
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-user-trap",
        any(
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(
        feature = "qemu-test-user-syscall",
        any(
            feature = "qemu-test-user-exit",
            feature = "qemu-test-fdt",
            feature = "qemu-test-heap",
            feature = "qemu-test-virtio"
        )
    ),
    all(feature = "qemu-test-user-exit", feature = "qemu-test-fdt"),
    all(
        feature = "qemu-test-fdt",
        feature = "qemu-test-heap",
        feature = "qemu-test-virtio"
    )
))]
compile_error!("QEMU kernel test features are mutually exclusive; enable at most one");

#[cfg(target_arch = "riscv64")]
mod arch;
#[cfg(target_arch = "riscv32")]
mod arch;
#[cfg(target_arch = "riscv64")]
mod console;
#[cfg(target_arch = "riscv32")]
mod console;
#[cfg(target_arch = "riscv64")]
mod drivers;
#[cfg(target_arch = "riscv32")]
mod drivers;
#[cfg(target_arch = "riscv32")]
mod storage {
    pub use minios_kernel::storage::{SectorReader, fat32, sd};
}
#[cfg(target_arch = "riscv64")]
mod storage {
    pub use minios_kernel::storage::{SectorReader, fat32, virtio_blk};
}
#[cfg(target_arch = "riscv64")]
mod machine;
#[cfg(target_arch = "riscv64")]
mod payload;
#[cfg(target_arch = "riscv64")]
mod proc_runtime;
#[cfg(target_arch = "riscv64")]
mod qemu_test;
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
mod shell;
#[cfg(target_arch = "riscv64")]
mod time;
#[cfg(target_arch = "riscv64")]
mod user_probe;
#[cfg(target_arch = "riscv64")]
mod user_trap;

#[cfg(target_arch = "riscv64")]
use proc_runtime::*;

#[cfg(target_arch = "riscv64")]
extern crate alloc;

#[cfg(target_arch = "riscv64")]
mod control;

#[cfg(target_arch = "riscv64")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv32")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv64")]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_arch = "riscv32")]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_arch = "riscv64")]
use minios_kernel::boot_payload::BootPayload;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-exit"))]
use minios_kernel::elf::fixture::user_exit_probe_elf;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-syscall"))]
use minios_kernel::elf::fixture::user_syscall_probe_elf;
#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    )
))]
use minios_kernel::elf::load::load_image_with_kernel_mappings;
#[cfg(target_arch = "riscv64")]
use minios_kernel::memory::{
    KERNEL_HEAP_LEN, KernelSections,
    frame::{FrameAllocator, FrameError, FrameSource, FrameStats, PAGE_SIZE, PhysFrame},
};
#[cfg(target_arch = "riscv64")]
use minios_kernel::process::{Process, ProcessTable};
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-exit"))]
use minios_kernel::user::run::{RunCompletion, RunOutcome, UserRun};
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::stdin::StdinStaging;
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::syscall::{SyscallFlow, complete_read, dispatch_syscall};
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::trap::TrapAction;
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::{RunExit, SSTATUS_SIE, UserContext};
#[cfg(target_arch = "riscv64")]
use minios_kernel::vm::AddressSpace;
#[cfg(target_arch = "riscv64")]
use minios_kernel::vm::{AddressSpaceBuilder, IdentityFrameStore, KernelMapPlan, PhysPageNum};
#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-vm", feature = "qemu-test-elf")
))]
use minios_kernel::vm::{VirtAddr, VmError};
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
use minios_kernel::{
    elf::{
        LoadedImage, USER_GUARD_BOTTOM, USER_STACK_BOTTOM, fixture::valid_riscv64_elf, load_image,
    },
    vm::{FrameStore, IdentityFrameStoreError, PageFlags, PhysAddr},
};

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
const UNKNOWN_HART_ID: usize = usize::MAX;
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(UNKNOWN_HART_ID);

/// `Heap`をspin lockで包んだglobal allocator。
/// 単一ハートかつ「割り込みハンドラー内では割り当てない」規約を前提にする。
/// 割り込み内で割り当てると、lock保持者を待つspinがそのままdeadlockする。
#[cfg(target_arch = "riscv64")]
struct KernelHeap {
    locked: core::sync::atomic::AtomicBool,
    heap: core::cell::UnsafeCell<minios_kernel::memory::heap::Heap>,
}

#[cfg(target_arch = "riscv64")]
// Safety: 単一ハート上で`locked`が排他アクセスを保証する。
unsafe impl Sync for KernelHeap {}

#[cfg(target_arch = "riscv64")]
impl KernelHeap {
    const fn empty() -> Self {
        Self {
            locked: core::sync::atomic::AtomicBool::new(false),
            heap: core::cell::UnsafeCell::new(minios_kernel::memory::heap::Heap::empty()),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut minios_kernel::memory::heap::Heap) -> R) -> R {
        while self
            .locked
            .swap(true, core::sync::atomic::Ordering::Acquire)
        {
            core::hint::spin_loop();
        }
        // Safety: `locked`を取得したためheapへの排他アクセスが成り立つ。
        let result = f(unsafe { &mut *self.heap.get() });
        self.locked
            .store(false, core::sync::atomic::Ordering::Release);
        result
    }

    /// ヒープ領域を登録する。起動直後の一度だけ呼ぶ。
    ///
    /// # Safety
    ///
    /// `Heap::init`と同じ排他性の契約を呼び出し側が保証する。
    unsafe fn init(
        &self,
        start: usize,
        len: usize,
    ) -> Result<(), minios_kernel::memory::heap::HeapError> {
        self.with(|heap| unsafe { heap.init(start, len) })
    }

    #[cfg(feature = "qemu-test-heap")]
    fn stats(&self) -> minios_kernel::memory::heap::HeapStats {
        self.with(|heap| heap.stats())
    }
}

/// `FrameAllocator`をspin lockで包んだ大域frame供給元。
/// `KernelHeap`と同じ単一ハート・割り込み内不使用の規約を前提にする。
/// lock順序はheap→framesの一方向だけを許し、frame側の臨界区間は
/// heapを割り当てないbitmap操作に限るため循環しない。
#[cfg(target_arch = "riscv64")]
struct LockedFrames {
    locked: core::sync::atomic::AtomicBool,
    ready: core::sync::atomic::AtomicBool,
    allocator: core::cell::UnsafeCell<core::mem::MaybeUninit<FrameAllocator<512>>>,
}

#[cfg(target_arch = "riscv64")]
// Safety: 単一ハート上で`locked`が排他アクセスを保証する。
unsafe impl Sync for LockedFrames {}

#[cfg(target_arch = "riscv64")]
impl LockedFrames {
    const fn empty() -> Self {
        Self {
            locked: core::sync::atomic::AtomicBool::new(false),
            ready: core::sync::atomic::AtomicBool::new(false),
            allocator: core::cell::UnsafeCell::new(core::mem::MaybeUninit::uninit()),
        }
    }

    /// 管理範囲を登録する。起動直後の一度だけ呼ぶ。
    ///
    /// # Safety
    ///
    /// `FrameAllocator::new`と同じ排他性の契約を呼び出し側が保証する。
    unsafe fn init(&self, base: usize, end: usize) -> Result<(), FrameError> {
        // Safety: 起動直後の単一実行であり、`ready`公開前に他の参照はない。
        let allocator = unsafe { FrameAllocator::new(base, end) }?;
        // Safety: 同上。
        unsafe { (*self.allocator.get()).write(allocator) };
        self.ready
            .store(true, core::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn with<R>(&self, f: impl FnOnce(&mut FrameAllocator<512>) -> R) -> Option<R> {
        if !self.ready.load(core::sync::atomic::Ordering::Acquire) {
            return None;
        }
        while self
            .locked
            .swap(true, core::sync::atomic::Ordering::Acquire)
        {
            core::hint::spin_loop();
        }
        // Safety: `locked`を取得し、`ready`以後はallocatorが初期化済みである。
        let result = f(unsafe { (*self.allocator.get()).assume_init_mut() });
        self.locked
            .store(false, core::sync::atomic::Ordering::Release);
        Some(result)
    }
}

#[cfg(target_arch = "riscv64")]
static GLOBAL_FRAMES: LockedFrames = LockedFrames::empty();

/// `GLOBAL_FRAMES`へlock付きで委譲する`FrameSource`実装。
/// ZSTなので必要な場所で値を作り、`&mut`経由で渡す。
#[cfg(target_arch = "riscv64")]
struct GlobalFrames;

#[cfg(target_arch = "riscv64")]
impl minios_kernel::memory::frame::FrameSource for GlobalFrames {
    fn allocate(&mut self) -> Option<PhysFrame> {
        GLOBAL_FRAMES.with(|frames| frames.allocate()).flatten()
    }

    fn allocate_at(&mut self, start: usize) -> Option<PhysFrame> {
        GLOBAL_FRAMES
            .with(|frames| frames.allocate_at(start))
            .flatten()
    }

    fn allocate_contiguous(&mut self, frames: &mut [Option<PhysFrame>]) -> bool {
        GLOBAL_FRAMES
            .with(|allocator| allocator.allocate_contiguous(frames))
            .unwrap_or(false)
    }

    fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        GLOBAL_FRAMES
            .with(|frames| frames.deallocate(frame))
            .unwrap_or(Err(FrameError::WrongAllocator))
    }

    fn deallocate_recoverable(&mut self, frame: PhysFrame) -> Result<(), (FrameError, PhysFrame)> {
        if !GLOBAL_FRAMES
            .ready
            .load(core::sync::atomic::Ordering::Acquire)
        {
            return Err((FrameError::WrongAllocator, frame));
        }
        GLOBAL_FRAMES
            .with(|frames| frames.deallocate_recoverable(frame))
            .expect("allocator is initialized once ready is set")
    }

    fn allocator_id(&self) -> u64 {
        GLOBAL_FRAMES
            .with(|frames| frames.allocator_id())
            .unwrap_or(0)
    }

    fn stats(&self) -> FrameStats {
        GLOBAL_FRAMES
            .with(|frames| frames.stats())
            .unwrap_or(FrameStats {
                total: 0,
                allocated: 0,
                free: 0,
            })
    }
}

/// virtio queue/request領域。frame poolの1 pageを所有し、dropで返す。
/// managed RAMは恒等map済みなので、frameの物理アドレスがそのまま
/// deviceへ渡すDMAアドレスになる。
#[cfg(target_arch = "riscv64")]
struct VirtioRegionPage {
    frame: Option<PhysFrame>,
}

#[cfg(target_arch = "riscv64")]
impl VirtioRegionPage {
    fn new(frame: PhysFrame) -> Self {
        // Safety: frame poolが管理する物理pageは`KernelMapPlan`で恒等map
        // 済み。used.idx等の初期値が不定だと完了判定を誤るため、page全域を
        // 0で初期化してから`VirtioRegion`として使う。
        unsafe { core::ptr::write_bytes(frame.start() as *mut u8, 0, PAGE_SIZE) };
        Self { frame: Some(frame) }
    }

    fn start(&self) -> usize {
        self.frame
            .as_ref()
            .expect("region frame is held until drop")
            .start()
    }
}

#[cfg(target_arch = "riscv64")]
impl core::ops::Deref for VirtioRegionPage {
    type Target = minios_kernel::storage::virtio_blk::VirtioRegion;

    fn deref(&self) -> &Self::Target {
        // Safety: `frame.start()`は4 KiB整列の恒等map済みpageであり、
        // `new`で全byteを初期化済み。`VirtioRegion`はpage全域を占有する。
        unsafe { &*(self.start() as *const Self::Target) }
    }
}

#[cfg(target_arch = "riscv64")]
impl core::ops::DerefMut for VirtioRegionPage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: `deref`と同じ領域。`VirtioBlk`が生存中はこのwrapperが
        // 領域の唯一のaliasである。
        unsafe { &mut *(self.start() as *mut Self::Target) }
    }
}

#[cfg(target_arch = "riscv64")]
impl Drop for VirtioRegionPage {
    fn drop(&mut self) {
        // `VirtioBlk::init`が失敗した場合と、確立済みsessionをrun終了時に
        // 閉じる場合にframeをpoolへ返す。
        if let Some(frame) = self.frame.take() {
            let _ = GlobalFrames.deallocate(frame);
        }
    }
}

/// virtioのqueue/buffer領域の汎用wrapper。frame poolの1 pageを所有し、
/// dropで返す。`T`は`align(4096)`を持つ4 KiB以内のregion型
/// (`NetQueueRegion`や`NetRxBufPage`) で、managed RAMは恒等map済み
/// なのでframeの物理アドレスがそのままdeviceへ渡すDMAアドレスになる。
#[cfg(target_arch = "riscv64")]
struct FrameRegion<T> {
    frame: Option<PhysFrame>,
    _target: core::marker::PhantomData<T>,
}

#[cfg(target_arch = "riscv64")]
impl<T> FrameRegion<T> {
    fn new(frame: PhysFrame) -> Self {
        // Safety: frame poolが管理する物理pageは`KernelMapPlan`で恒等map
        // 済み。queue/bufferの初期値が不定だと完了判定を誤るため、page全域を
        // 0で初期化してから`T`として使う。
        unsafe { core::ptr::write_bytes(frame.start() as *mut u8, 0, PAGE_SIZE) };
        Self {
            frame: Some(frame),
            _target: core::marker::PhantomData,
        }
    }

    fn start(&self) -> usize {
        self.frame
            .as_ref()
            .expect("region frame is held until drop")
            .start()
    }
}

#[cfg(target_arch = "riscv64")]
impl<T> core::ops::Deref for FrameRegion<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: `frame.start()`は4 KiB整列の恒等map済みpageであり、
        // `new`で全byteを初期化済み。`T`は`align(4096)`かつ1 page以内。
        unsafe { &*(self.start() as *const T) }
    }
}

#[cfg(target_arch = "riscv64")]
impl<T> core::ops::DerefMut for FrameRegion<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: `deref`と同じ領域。driverが生存中はこのwrapperが領域の
        // 唯一のaliasである。
        unsafe { &mut *(self.start() as *mut T) }
    }
}

#[cfg(target_arch = "riscv64")]
impl<T> Drop for FrameRegion<T> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            let _ = GlobalFrames.deallocate(frame);
        }
    }
}

#[cfg(target_arch = "riscv64")]
// Safety: `GlobalAlloc`の契約により、返すポインターはlayoutの整列を満たす
// 有効な領域であり、`dealloc`は`alloc`が返したポインターだけを受け取る。
// OOM時はnullを返し、`alloc`crateの既定handlerがpanicへ変換する。
unsafe impl core::alloc::GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.with(|heap| {
            if let Ok(ptr) = heap.alloc(layout) {
                return ptr.as_ptr();
            }
            // 空きが尽きたらframe poolの最上位（heap領域の直下）を
            // 取り込んで下端を伸ばし、要求が収まるまで繰り返す。
            // 隣接pageが取れなければpoolは実質枯渇しておりOOMとなる。
            let mut frames = GlobalFrames;
            loop {
                let Some(new_start) = heap.start().checked_sub(PAGE_SIZE) else {
                    return core::ptr::null_mut();
                };
                let Some(frame) = frames.allocate_at(new_start) else {
                    return core::ptr::null_mut();
                };
                if heap.extend_down(frame.start(), PAGE_SIZE).is_err() {
                    // 隣接検査は必ず通る想定だが、万一の場合はframeを返してOOMとする。
                    let _ = frames.deallocate(frame);
                    return core::ptr::null_mut();
                }
                if let Ok(ptr) = heap.alloc(layout) {
                    return ptr.as_ptr();
                }
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: core::alloc::Layout) {
        let Some(ptr) = core::ptr::NonNull::new(ptr) else {
            return;
        };
        // spanの検証失敗（領域外・二重解放）は安全側の見逃しとして
        // 解放せずに残す。破壊よりリークを選ぶ。
        let _ = self.with(|heap| unsafe { heap.dealloc(ptr) });
        // 成長分の下端がpage単位で空いたらframe poolへ返す。
        // 初期領域はpoolの管理範囲外なので`init_start`を境に返さない。
        // lock順序はallocと同じheap→framesである。
        let mut frames = GlobalFrames;
        self.with(|heap| {
            heap.release_grown_pages(|start| {
                // Safety: heapが切り離したpageは成長時にpoolから借りた
                // frameであり、管理外になった今、所有者はここだけである。
                if let Ok(frame) = unsafe { PhysFrame::from_start(start) } {
                    let _ = frames.deallocate(frame);
                }
            })
        });
    }
}

#[cfg(target_arch = "riscv64")]
#[global_allocator]
static KERNEL_HEAP: KernelHeap = KernelHeap::empty();

// handlerへ渡す実行中imageのaddress spaceとframe store。test probeと
// production payload pathの両方が使う。
// Safety: `__run_user`の直前にrunnerが設定し、handlerだけが解参照する。
// kernelへ戻った直後にrunnerが0へ戻す。
// user-mode実行基盤はRV64のQEMU運用の仕組みであり、RV32実機では使わない。
#[cfg(target_arch = "riscv64")]
static mut USER_SYSCALL_PROBE_SPACE: usize = 0;
#[cfg(target_arch = "riscv64")]
static mut USER_SYSCALL_PROBE_MEMORY: usize = 0;

#[cfg(target_arch = "riscv64")]
static USER_EXIT_CODE: AtomicUsize = AtomicUsize::new(usize::MAX);
#[cfg(target_arch = "riscv64")]
static USER_RUN_OUTCOME: AtomicUsize = AtomicUsize::new(USER_RUN_OUTCOME_NONE);
#[cfg(target_arch = "riscv64")]
static USER_FATAL_SCAUSE: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "riscv64")]
static USER_FATAL_STVAL: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_NONE: usize = 0;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_EXIT: usize = 1;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_FATAL_TRAP: usize = 2;
/// manifest v2のuser例外終了code。Rust guestのpanicと同じ失敗codeを使う。
#[cfg(target_arch = "riscv64")]
const USER_FAULT_EXIT_CODE: u32 = 70;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_SINK_FAILURE: usize = 3;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_SOURCE_FAILURE: usize = 4;
/// timer割り込みによるpreempt。contextはtrap stack上のslotへ残り、
/// schedulerが次回dispatch時に再開する。
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_PREEMPTED: usize = 5;

/// `read`が入力未到着で中断した。`sepc`はecallへ戻してあるため、入力が
/// 届いて再開されると同じsyscallがやり直される。
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_BLOCKED: usize = 6;

/// `exec`がimageを差し替えた。trap slotには新imageの初期contextが
/// 書き込まれており、`reload_context`がそのまま拾う。旧imageは
/// `retired_image`へ退避済みで、kernel satpへ戻った今ここで解放する。
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_EXEC: usize = 7;
/// run単位のstdin staging。resetはrunnerが実行窓の前に行い、handlerだけが
/// assemblyの実行窓で借りる。runnerが待機中のため同時にaliasしない。
#[cfg(target_arch = "riscv64")]
static mut USER_STDIN_STAGING: StdinStaging = StdinStaging::new();

#[cfg(target_arch = "riscv64")]
// Safety: `linker.ld`がRISC-Vカーネルイメージの末尾に必ず定義するC ABIシンボルである。
// Rust側は`addr_of!`でアドレスを作るだけで、外部staticの内容を読み書きしない。
unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __bss_end: u8;
    static __boot_stack_start: u8;
    static __boot_stack_end: u8;
    static __kernel_end: u8;
}

#[cfg(target_arch = "riscv64")]
// `entry.S`は、外部C ABIシンボル`kernel_main`を呼び出す。
// OpenSBIの`a0/a1`を`hart_id/dtb`としてそのまま受け取るため、この名前とABIを変えてはならない。
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(hart_id: usize, dtb: usize) -> ! {
    // パニック診断が起動ハートを識別できるよう、ほかの初期化より前に記録する。
    BOOT_HART_ID.store(hart_id, Ordering::Relaxed);
    // OpenSBIがa1で渡すDTBからRAM、UART、timebaseを発見する。
    // 失敗時はQEMU `virt`既定値のUART経由でpanic診断を出して停止する。
    let machine = match machine::discover(dtb) {
        Ok(spec) => spec,
        Err(error) => panic!("MiniOS fdt: {error:?}"),
    };
    let managed_memory_start = kernel_memory_start();
    if machine.managed_end() <= managed_memory_start || !machine.ram.contains(&managed_memory_start)
    {
        panic!("MiniOS fdt: machine RAM does not contain the kernel image");
    }
    // ターゲット固有のコンソール初期化。QEMUでは何もしない。
    console::init();
    arch::riscv64::trap::init();
    crate::println!("[ok] traps");

    // managed RAMの末尾をヒープ領域として切り出し、FrameAllocatorの
    // 管理上端をヒープの直下へ下げる。
    let heap_start = machine
        .managed_end()
        .checked_sub(KERNEL_HEAP_LEN)
        .filter(|start| *start > managed_memory_start)
        .unwrap_or_else(|| panic!("MiniOS heap: managed RAM too small"));
    // Safety: `heap_start..machine.managed_end()`はmachine記述が導くRAM内の
    // 排他的領域であり、FrameAllocator、payload窓、FDT予約とは重ならない。
    unsafe { KERNEL_HEAP.init(heap_start, KERNEL_HEAP_LEN) }
        .unwrap_or_else(|error| panic!("MiniOS heap: invalid region: {error:?}"));

    // Safety: OpenSBIが使う`0x8000_0000..0x8020_0000`と、リンカーが配置する
    // `0x8020_0000..managed_memory_start`のカーネルイメージを除外している。
    // machine記述が導くヒープ領域の直下までを所有するのは、この大域アロケーターだけである。
    // このアロケーターが生存している間は、同じ範囲を管理する別の所有者を作らない。
    // heapがOOM時に`allocate_at`で上端から成長するため、供給元は大域化する。
    //
    // bitmap容量を超えるRAMは下端側を管理対象から外し、管理範囲をheapの
    // 直下へ寄せる。範囲外のframeは誰にも割り当たらず、`allocate_at`による
    // heap成長は常に管理内へ届く。
    let frames_base = managed_memory_start
        .max(heap_start.saturating_sub(FrameAllocator::<512>::CAPACITY_FRAMES * PAGE_SIZE));
    if let Err(error) = unsafe { GLOBAL_FRAMES.init(frames_base, heap_start) } {
        fatal_memory_error(error);
    }
    let mut frames = GlobalFrames;

    let sections = kernel_sections();
    // 予約窓はこの時点ではまだaddress spaceをactivateしていないbare mode
    // (VA==PA) にあるため、物理addressから直接検証できる。loaderがpayloadを
    // 置いていない場合はNoneとなり、shellへ抜ける。
    let payload =
        if unsafe { BootPayload::reserved_window_has_bundle(machine.payload_window().start) } {
            // Safety: `-m 128M`と`-device loader`が予約窓を有効RAMとして配置する。
            match unsafe { BootPayload::from_reserved_window(machine.payload_window()) } {
                Ok(payload) => Some(payload),
                Err(error) => fatal_payload_error(format_args!(
                    "MiniOS payload: invalid bundle, {error:?}\r\n"
                )),
            }
        } else {
            None
        };
    let plan = match KernelMapPlan::new(
        &sections,
        managed_memory_start,
        machine.managed_end(),
        machine.uart_base,
    ) {
        Ok(plan) => plan,
        Err(error) => panic!("invalid kernel mapping plan: {error:?}"),
    };
    // payloadが存在するときだけ、使用page (切り上げ) をS-mode read-onlyで
    // kernel空間とuser空間のborrowed mappingへ加える。窓全体はmapしない。
    let plan = match payload.as_ref() {
        Some(payload) => plan
            .with_payload_pages(machine.payload_window(), payload.total_len() as usize)
            .unwrap_or_else(|error| panic!("invalid payload mapping plan: {error:?}")),
        None => plan,
    };
    // FDTが報告したvirtio-mmio領域をS-mode R+Wで1pageずつmapする。
    // deviceが存在しないslotはprobeで弾かれるだけなので全slotをmapしてよい。
    let plan = plan
        .with_device_pages(&machine.virtio_mmio[..machine.virtio_mmio_count])
        .unwrap_or_else(|error| panic!("invalid device mapping plan: {error:?}"));
    // PLICのMMIO pageはUART割り込みのclaim/completeに必要なため、device
    // slotとは別の専用slotでS-mode R+Wとしてmapする。
    let plan = if machine.plic_base != 0 {
        plan.with_plic_page(machine.plic_base)
            .unwrap_or_else(|error| panic!("invalid plic mapping plan: {error:?}"))
    } else {
        plan
    };
    // Safety: QEMU virt exposes managed_memory_start..machine.managed_end()
    // as valid RAM. Bare translation reaches it by identity before satp
    // changes, and `plan` identity-maps the complete range afterward. This
    // boot hart creates the only IdentityFrameStore, and all byte access to
    // allocator-issued frames goes through it for the rest of kernel_main.
    // The allocator only tracks/assigns frames and never dereferences their
    // memory itself.
    let mut memory =
        match unsafe { IdentityFrameStore::new(managed_memory_start, machine.managed_end()) } {
            Ok(memory) => memory,
            Err(error) => panic!("invalid identity frame-store range: {error:?}"),
        };
    // builderが所有権台帳をheap上に内部所有し、kernel spaceがそのまま
    // 引き継ぐ。静的arenaは要らない。
    let mut builder = match AddressSpaceBuilder::new(&mut frames, &mut memory) {
        Ok(builder) => builder,
        Err(error) => panic!("kernel address-space root allocation failed: {error:?}"),
    };
    for mapping in plan.mappings() {
        if let Err(error) =
            builder.map_borrowed(mapping.page(), mapping.physical(), mapping.flags())
        {
            panic!("kernel identity mapping failed: {error:?}");
        }
    }
    let kernel_space = builder.finish();
    let root = match PhysPageNum::from_start(kernel_space.root().as_u64()) {
        Ok(root) => root,
        Err(error) => panic!("kernel root page number is invalid: {error:?}"),
    };
    // Safety: the completed plan identity-maps the executing kernel text, boot
    // stack, trap vector, UART, and allocator-managed RAM that owns every page
    // table. `root` belongs to this address space and remains live forever.
    unsafe { arch::riscv64::csr::activate_sv39(root) };

    if let Err(error) = time::init(machine.timebase_hz) {
        fatal_timer_error("initial schedule", error);
    }
    // PLICを持つmachine記述ではUARTの受信割り込みをS-modeへ配送し、
    // 待機窓を`wfi`で眠らせられるようにする。PLICの無いmachineでは
    // 割り込み経路を立てず、全てpollingのまま動く。
    if machine.plic_base != 0 {
        drivers::plic::init(machine.plic_base);
        drivers::plic::enable(drivers::plic::UART_IRQ);
        drivers::uart::Uart::for_target().enable_rx_interrupt();
        const SIE_SEIE: usize = 1 << 9;
        // Safety: claim/dispatchの経路は`trap::init`のkernel handlerと
        // `handle_user_trap`のExternal分岐に揃っている。既存ビットを保った
        // ままSEIE（ビット9）だけを有効にする。
        unsafe {
            let sie = arch::riscv64::csr::read_sie();
            arch::riscv64::csr::write_sie(sie | SIE_SEIE);
        }
    }
    #[cfg(feature = "qemu-test-irq-uart")]
    {
        // PLIC経路のread-back診断。base=0はFDTでPLIC未検出、enable/priority/
        // ierの値は割り込み経路が実際に立ったかを示す。
        let (base, enable, threshold, priority) = drivers::plic::debug_state();
        let ier = drivers::uart::Uart::for_target().ier();
        crate::println!(
            "[MINIOS_TEST] plic: base={base:#x} enable={enable:#x} threshold={threshold} priority={priority} ier={ier:#x}"
        );
    }
    crate::println!("[ok] timer");
    crate::println!("[ok] memory");

    #[cfg(feature = "qemu-test-vm")]
    {
        qemu_test::run_vm_test(&kernel_space, &memory);
    }

    #[cfg(any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall"
    ))]
    {
        user_probe::run_user_mode_probe_test(&kernel_space, &plan, &mut frames, &mut memory);
    }

    #[cfg(feature = "qemu-test-user-exit")]
    {
        user_probe::run_user_exit_test(&kernel_space, &plan, &mut frames, &mut memory);
    }

    #[cfg(feature = "qemu-test-elf")]
    {
        qemu_test::run_elf_test(&kernel_space, &mut frames, &mut memory);
    }

    #[cfg(feature = "qemu-test-memory")]
    {
        qemu_test::run_memory_test(&mut frames);
    }

    #[cfg(feature = "qemu-test-fdt")]
    {
        qemu_test::run_fdt_test(machine);
    }

    #[cfg(feature = "qemu-test-heap")]
    {
        qemu_test::run_heap_test();
    }

    #[cfg(feature = "qemu-test-virtio")]
    {
        qemu_test::run_virtio_test(machine, &mut frames);
    }

    #[cfg(feature = "qemu-test-net-arp")]
    {
        qemu_test::run_net_arp_test(machine, &mut frames);
    }

    #[cfg(feature = "qemu-test-net-ping")]
    {
        qemu_test::run_net_ping_test(machine, &mut frames);
    }

    if let Some(payload) = payload {
        payload::run_boot_payload(&kernel_space, &plan, &mut frames, &mut memory, payload);
    }

    crate::println!("MiniOS booting...");
    crate::println!("hart id: {hart_id}");

    #[cfg(feature = "qemu-test-boot")]
    {
        crate::println!("[MINIOS_TEST] boot: ok");
        arch::riscv64::sbi::system_reset(
            arch::riscv64::sbi::ResetType::Shutdown,
            arch::riscv64::sbi::ResetReason::NoReason,
        );
    }

    #[cfg(feature = "qemu-test-trap")]
    {
        // Safety: `ebreak`はメモリーとスタックを変更せず、同期ブレークポイント例外を発生させる。
        // `trap::init`は完了しており、ハンドラーがテスト成功としてリセットする規約になっている。
        unsafe { core::arch::asm!("ebreak", options(nomem, nostack)) };
    }

    #[cfg(feature = "qemu-test-timer")]
    {
        while time::ticks() < 3 {
            core::hint::spin_loop();
        }
        crate::println!("[MINIOS_TEST] timer: ok");
        arch::riscv64::sbi::system_reset(
            arch::riscv64::sbi::ResetType::Shutdown,
            arch::riscv64::sbi::ResetReason::NoReason,
        );
    }

    shell::run(hart_id, &mut frames)
}

#[cfg(target_arch = "riscv64")]
fn kernel_memory_start() -> usize {
    // `__kernel_end`は、`linker.ld`がカーネルイメージの実在する境界として定義する。
    // `addr_of!`はアドレスを作るだけで、外部staticのメモリー内容を読み取らない。
    let kernel_end = core::ptr::addr_of!(__kernel_end) as usize;
    // `linker.ld`はこのシンボルを4 KiB境界にそろえるが、将来リンカーを変更してもイメージを保護できるよう、ここでも上向きに丸める。
    align_up_to_page(kernel_end)
}

#[cfg(target_arch = "riscv64")]
fn kernel_sections() -> KernelSections {
    let text = core::ptr::addr_of!(__text_start) as usize..core::ptr::addr_of!(__text_end) as usize;
    let rodata =
        core::ptr::addr_of!(__rodata_start) as usize..core::ptr::addr_of!(__rodata_end) as usize;
    let writable =
        core::ptr::addr_of!(__data_start) as usize..core::ptr::addr_of!(__bss_end) as usize;
    let boot_stack = core::ptr::addr_of!(__boot_stack_start) as usize
        ..core::ptr::addr_of!(__boot_stack_end) as usize;
    let kernel_end = core::ptr::addr_of!(__kernel_end) as usize;

    match KernelSections::new(text, rodata, writable, boot_stack, kernel_end) {
        Ok(sections) => sections,
        Err(error) => panic!("invalid linker-provided kernel sections: {error:?}"),
    }
}

#[cfg(target_arch = "riscv64")]
fn align_up_to_page(address: usize) -> usize {
    // カーネルイメージは`0x8800_0000`より十分低い位置にあるため、`PAGE_SIZE - 1`を加えても`usize`を超えない。
    (address + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1)
}

/// run単位のstdin stagingを借りる。
///
/// # Safety
///
/// assemblyの実行窓の中のhandlerからのみ呼び、借用をtrapの外へ持ち出さないこと。
/// runnerは実行窓で待機中のため、借用が同時にaliasすることはない。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
unsafe fn borrow_stdin_staging() -> &'static mut StdinStaging {
    unsafe { &mut *&raw mut USER_STDIN_STAGING }
}

#[cfg(target_arch = "riscv64")]
fn fatal_payload_error(arguments: core::fmt::Arguments<'_>) -> ! {
    // payload pathの失敗はhost (minictr) が即座に検出できるよう異常shutdownする。
    crate::console::emergency_print(arguments);
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-vm",
        feature = "qemu-test-elf",
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit",
        feature = "qemu-test-fdt",
        feature = "qemu-test-heap",
        feature = "qemu-test-virtio",
        feature = "qemu-test-net-arp",
        feature = "qemu-test-net-ping"
    )
))]
fn successful_qemu_test_shutdown() -> ! {
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::NoReason,
    )
}

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-vm",
        feature = "qemu-test-elf",
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit",
        feature = "qemu-test-heap",
        feature = "qemu-test-virtio",
        feature = "qemu-test-net-arp",
        feature = "qemu-test-net-ping"
    )
))]
fn fatal_qemu_test(arguments: core::fmt::Arguments<'_>) -> ! {
    crate::console::emergency_print(format_args!("[MINIOS_TEST] failed: {arguments}\r\n"));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(target_arch = "riscv64")]
fn fatal_timer_error(operation: &str, error: minios_kernel::sbi::SbiError) -> ! {
    // タイマを再設定できなければ次の割り込み時刻を保証できず回復規約もないため、
    // ロック不要の診断経路で SBI エラーを残し、失敗理由で停止する。
    crate::console::emergency_print(format_args!(
        "MiniOS timer: {operation} failed with SBI error {}\r\n",
        error.0
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(target_arch = "riscv64")]
fn fatal_memory_error(error: FrameError) -> ! {
    // リンカーの範囲とビットマップ容量が一致しなければ、安全な未使用ページ集合を作れないため、診断を出して異常終了する。
    crate::console::emergency_print(format_args!("MiniOS memory: {error:?}\r\n"));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(target_arch = "riscv32")]
// `entry.S`はスタックとBSSを整えてからこのABI境界を呼ぶ。
// NEORV32は単一ハートのMモード起動であり、OpenSBIやDTBは存在しない。
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main32() -> ! {
    BOOT_HART_ID.store(0, Ordering::Relaxed);
    console::init();
    crate::println!("MiniOS/RV32 booting...");
    crate::println!("hart id: 0");
    shell::run32(0)
}

#[cfg(target_arch = "riscv32")]
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let hart_id = BOOT_HART_ID.load(Ordering::Relaxed);
    match (info.location(), hart_id != UNKNOWN_HART_ID) {
        (Some(location), true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\nhart id: {}\r\n",
            info.message(),
            location.file(),
            location.line(),
            hart_id
        )),
        (Some(location), false) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\n",
            info.message(),
            location.file(),
            location.line()
        )),
        (None, true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nhart id: {}\r\n",
            info.message(),
            hart_id
        )),
        (None, false) => {
            crate::console::emergency_print(format_args!("MiniOS panic: {}\r\n", info.message()))
        }
    }
    // NEORV32にSBIはないため、割り込み待ちで停止する。
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[cfg(target_arch = "riscv64")]
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let hart_id = BOOT_HART_ID.load(Ordering::Relaxed);
    match (info.location(), hart_id != UNKNOWN_HART_ID) {
        (Some(location), true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\nhart id: {}\r\n",
            info.message(),
            location.file(),
            location.line(),
            hart_id
        )),
        (Some(location), false) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\n",
            info.message(),
            location.file(),
            location.line()
        )),
        (None, true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nhart id: {}\r\n",
            info.message(),
            hart_id
        )),
        (None, false) => {
            crate::console::emergency_print(format_args!("MiniOS panic: {}\r\n", info.message()))
        }
    }
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}
