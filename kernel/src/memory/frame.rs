use core::sync::atomic::{AtomicU64, Ordering};

pub const PAGE_SIZE: usize = 4096;

const FIRST_ALLOCATOR_ID: u64 = 1;
static NEXT_ALLOCATOR_ID: AtomicU64 = AtomicU64::new(FIRST_ALLOCATOR_ID);

const fn advance_allocator_id(current: u64) -> Option<u64> {
    current.checked_add(1)
}

fn reserve_allocator_id() -> Result<u64, FrameError> {
    NEXT_ALLOCATOR_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, advance_allocator_id)
        .map_err(|_| FrameError::ProvenanceExhausted)
}

/// アロケーターから払い出された物理ページを一意に表す所有権の値。
///
/// ```compile_fail
/// use minios_kernel::memory::frame::PhysFrame;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<PhysFrame>();
/// ```
///
/// ```compile_fail
/// use minios_kernel::memory::frame::PhysFrame;
///
/// fn require_copy<T: Copy>() {}
/// require_copy::<PhysFrame>();
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct PhysFrame(usize);

impl PhysFrame {
    /// 指定した物理アドレスの所有権を表す値を作る。
    ///
    /// ```compile_fail
    /// use minios_kernel::memory::frame::PhysFrame;
    ///
    /// let frame = PhysFrame::from_start(0x4000).unwrap();
    /// let _ = frame;
    /// ```
    ///
    /// # Safety
    ///
    /// `start`がページ境界にそろい`Ok`を返す場合、呼び出し側はその物理ページを排他的に所有し、
    /// 同じアドレスを表す生存中の`PhysFrame`がほかにないことを保証しなければならない。
    /// 境界にそろっていないアドレスでは所有権を作らず、`Unaligned`を返す。
    pub unsafe fn from_start(start: usize) -> Result<Self, FrameError> {
        // 物理ページはMMUとハードウェアが定める4 KiB境界で始まるため、下位ビットを検査する。
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Unaligned);
        }
        Ok(Self(start))
    }

    pub const fn start(&self) -> usize {
        self.0
    }
}

/// frameの供給元を抽象化する。`FrameAllocator`は物理bitmapを直接管理し、
/// globalな供給元はlock済みのallocatorへ委譲する。利用側はtrait object経由で
/// どちらも同じ手順で扱える。
pub trait FrameSource {
    /// 空きframeを一つ払い出す。枯渇時は`None`。
    fn allocate(&mut self) -> Option<PhysFrame>;
    /// `start`で指定したframeが空きなら占有する。隣接成長のように
    /// 特定の番地だけが意味を持つ供給要求に使う。
    fn allocate_at(&mut self, start: usize) -> Option<PhysFrame>;
    /// `frame`を供給元へ返す。
    fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError>;
    /// `deallocate`と同じだが、失敗時にframeの所有権を呼び出し側へ戻す。
    /// 巻き戻し経路が所有権を保持し続けるために使う。
    fn deallocate_recoverable(&mut self, frame: PhysFrame) -> Result<(), (FrameError, PhysFrame)>;
    /// 払い出し元を識別するid。frameとpage tableのprovenance照合に使う。
    fn allocator_id(&self) -> u64;
    fn stats(&self) -> FrameStats;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    EmptyRange,
    Unaligned,
    CapacityExceeded,
    OutOfRange,
    DoubleFree,
    WrongAllocator,
    ProvenanceExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub total: usize,
    pub allocated: usize,
    pub free: usize,
}

/// 物理フレームの所有権を一つのビットマップだけに対応付ける。
///
/// ```compile_fail
/// use minios_kernel::memory::frame::FrameAllocator;
///
/// // Safety: コンパイル失敗例では、仮想的なテスト範囲にほかの所有者がいないと仮定する。
/// let allocator = unsafe { FrameAllocator::<1>::new(0x4000, 0x8000) }.unwrap();
/// // 複製すると同じ物理ページを二つのアロケーターが返せるため、所有者は複製できない。
/// let duplicate = allocator.clone();
/// let _ = duplicate;
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct FrameAllocator<const WORDS: usize> {
    allocator_id: u64,
    base: usize,
    frame_count: usize,
    allocated: usize,
    bitmap: [u64; WORDS],
}

impl<const WORDS: usize> FrameAllocator<WORDS> {
    /// 指定した物理アドレス範囲を管理するアロケーターを作る。
    ///
    /// ```compile_fail
    /// use minios_kernel::memory::frame::FrameAllocator;
    ///
    /// let allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
    /// let _ = allocator;
    /// ```
    ///
    /// ```compile_fail
    /// use minios_kernel::memory::frame::FrameAllocator;
    ///
    /// let first = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
    /// let overlapping = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
    /// let _ = (first, overlapping);
    /// ```
    ///
    /// # Safety
    ///
    /// 検証に成功した場合、呼び出し側は`base..end`の全物理ページがほかのアロケーターや
    /// サブシステムに所有または管理されていない排他的な範囲であることを保証しなければならない。
    /// 返したアロケーターが生存している間は、別の所有者も同じ範囲を取得してはならない。
    /// アラインメント、空の範囲、容量超過で`Err`を返す場合、この関数は範囲を取得しない。
    pub unsafe fn new(base: usize, end: usize) -> Result<Self, FrameError> {
        // `base`と`end`が4 KiB境界にそろわなければ、ビットマップの1ビットを1物理ページへ安全に対応付けられない。
        if !base.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Unaligned);
        }
        if base >= end {
            return Err(FrameError::EmptyRange);
        }

        // 上端が`base`より大きいことを確認済みなので、この差分はオーバーフローせずページ数を表す。
        let frame_count = (end - base) / PAGE_SIZE;
        let capacity = WORDS.saturating_mul(u64::BITS as usize);
        if frame_count > capacity {
            return Err(FrameError::CapacityExceeded);
        }
        let allocator_id = reserve_allocator_id()?;

        Ok(Self {
            allocator_id,
            base,
            frame_count,
            allocated: 0,
            bitmap: [0; WORDS],
        })
    }

    pub fn allocate(&mut self) -> Option<PhysFrame> {
        for frame_index in 0..self.frame_count {
            let word_index = frame_index / u64::BITS as usize;
            let bit_index = frame_index % u64::BITS as usize;
            let bit = 1_u64 << bit_index;
            if self.bitmap[word_index] & bit == 0 {
                self.bitmap[word_index] |= bit;
                self.allocated += 1;
                // `frame_index`は`frame_count`未満であり、`new`が検証した物理アドレス範囲から外れない。
                let start = self.base + frame_index * PAGE_SIZE;
                return Some(PhysFrame(start));
            }
        }
        None
    }

    /// `start`のframeだけを占有する。heapが管理領域の直下へ連続して
    /// 成長するなど、特定番地のみが意味を持つ供給に使う。
    /// 範囲外・未整列・占有済みでは`None`を返し、bitmapを変えない。
    pub fn allocate_at(&mut self, start: usize) -> Option<PhysFrame> {
        if !start.is_multiple_of(PAGE_SIZE) || start < self.base {
            return None;
        }
        let frame_index = (start - self.base) / PAGE_SIZE;
        if frame_index >= self.frame_count {
            return None;
        }
        let word_index = frame_index / u64::BITS as usize;
        let bit = 1_u64 << (frame_index % u64::BITS as usize);
        if self.bitmap[word_index] & bit != 0 {
            return None;
        }
        self.bitmap[word_index] |= bit;
        self.allocated += 1;
        // `start`は整列・範囲内で、占有bitを立てたので排他所有が成り立つ。
        Some(PhysFrame(start))
    }

    /// 所有権を表す値を消費し、対応するページをアロケーターへ返す。
    ///
    /// ```compile_fail
    /// use minios_kernel::memory::frame::FrameAllocator;
    ///
    /// // Safety: コンパイル失敗例では、仮想的なテスト範囲にほかの所有者がいないと仮定する。
    /// let mut allocator = unsafe { FrameAllocator::<1>::new(0x4000, 0x8000) }.unwrap();
    /// let frame = allocator.allocate().unwrap();
    /// allocator.deallocate(frame).unwrap();
    /// let _ = allocator.deallocate(frame);
    /// ```
    pub fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        self.deallocate_recoverable(frame)
            .map_err(|(error, _frame)| error)
    }

    pub(crate) fn deallocate_recoverable(
        &mut self,
        frame: PhysFrame,
    ) -> Result<(), (FrameError, PhysFrame)> {
        let start = frame.start();
        // `PhysFrame`は通常`from_start`で作るが、型を将来変更しても安全性を保てるよう、解放時にも4 KiB境界を確認する。
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err((FrameError::Unaligned, frame));
        }
        if start < self.base {
            return Err((FrameError::OutOfRange, frame));
        }

        // `start >= base`を確認済みなので、この差分は安全にビットマップ用のページ番号へ変換できる。
        let frame_index = (start - self.base) / PAGE_SIZE;
        if frame_index >= self.frame_count {
            return Err((FrameError::OutOfRange, frame));
        }

        let word_index = frame_index / u64::BITS as usize;
        let bit_index = frame_index % u64::BITS as usize;
        let bit = 1_u64 << bit_index;
        if self.bitmap[word_index] & bit == 0 {
            return Err((FrameError::DoubleFree, frame));
        }
        self.bitmap[word_index] &= !bit;
        self.allocated -= 1;
        Ok(())
    }

    pub(crate) const fn allocator_id(&self) -> u64 {
        self.allocator_id
    }

    pub const fn stats(&self) -> FrameStats {
        FrameStats {
            total: self.frame_count,
            allocated: self.allocated,
            free: self.frame_count - self.allocated,
        }
    }
}

impl<const WORDS: usize> FrameSource for FrameAllocator<WORDS> {
    fn allocate(&mut self) -> Option<PhysFrame> {
        FrameAllocator::allocate(self)
    }

    fn allocate_at(&mut self, start: usize) -> Option<PhysFrame> {
        FrameAllocator::allocate_at(self, start)
    }

    fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        FrameAllocator::deallocate(self, frame)
    }

    fn deallocate_recoverable(&mut self, frame: PhysFrame) -> Result<(), (FrameError, PhysFrame)> {
        FrameAllocator::deallocate_recoverable(self, frame)
    }

    fn allocator_id(&self) -> u64 {
        FrameAllocator::allocator_id(self)
    }

    fn stats(&self) -> FrameStats {
        FrameAllocator::stats(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameAllocator, FrameError, FrameStats, PhysFrame, advance_allocator_id};

    fn allocator_fixture<const WORDS: usize>(
        base: usize,
        end: usize,
    ) -> Result<FrameAllocator<WORDS>, FrameError> {
        // Safety: ホストテストのアドレスは参照先へアクセスしない独立したビットマップモデルである。
        // 各テスト用データの有効範囲では、このアロケーターだけが対象範囲を管理する。
        unsafe { FrameAllocator::new(base, end) }
    }

    fn forged_frame_fixture(start: usize) -> Result<PhysFrame, FrameError> {
        // Safety: 不正解放の診断を確認するテストだけが、実際のメモリーへ触れない所有権の値を意図的に偽造する。
        // 本番の安全な呼び出し側には、同じ操作を公開しない。
        unsafe { PhysFrame::from_start(start) }
    }

    #[test]
    fn stats_report_all_frames_free_before_allocation() {
        let allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();

        assert_eq!(
            allocator.stats(),
            FrameStats {
                total: 4,
                allocated: 0,
                free: 4,
            }
        );
    }

    #[test]
    fn stats_report_one_used_frame_after_allocation() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        allocator.allocate().unwrap();

        assert_eq!(
            allocator.stats(),
            FrameStats {
                total: 4,
                allocated: 1,
                free: 3,
            }
        );
    }

    #[test]
    fn rejected_deallocation_does_not_change_stats() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        allocator.allocate().unwrap();

        assert_eq!(
            allocator.deallocate(forged_frame_fixture(0x9000).unwrap()),
            Err(FrameError::OutOfRange)
        );
        assert_eq!(
            allocator.stats(),
            FrameStats {
                total: 4,
                allocated: 1,
                free: 3,
            }
        );
    }

    #[test]
    fn stats_restore_free_count_after_deallocation() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        let frame = allocator.allocate().unwrap();

        allocator.deallocate(frame).unwrap();

        assert_eq!(
            allocator.stats(),
            FrameStats {
                total: 4,
                allocated: 0,
                free: 4,
            }
        );
    }

    #[test]
    fn allocates_and_reuses_a_frame() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        let first = allocator.allocate().unwrap();
        let first_start = first.start();
        let second = allocator.allocate().unwrap();
        assert_eq!(first.start(), 0x4000);
        assert_eq!(second.start(), 0x5000);
        allocator.deallocate(first).unwrap();
        assert_eq!(allocator.allocate().unwrap().start(), first_start);
    }

    #[test]
    fn allocate_at_takes_only_the_requested_frame() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();

        let frame = allocator.allocate_at(0x6000).unwrap();
        assert_eq!(frame.start(), 0x6000);
        assert_eq!(allocator.stats().allocated, 1);
        // 指定frameだけが占有され、ほかはbottom-upの割り当てへ残る。
        assert_eq!(allocator.allocate().unwrap().start(), 0x4000);
        assert_eq!(allocator.allocate().unwrap().start(), 0x5000);
        assert_eq!(allocator.allocate().unwrap().start(), 0x7000);
        assert_eq!(allocator.allocate(), None);
    }

    #[test]
    fn allocate_at_rejects_unaligned_out_of_range_and_taken_frames() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        let taken = allocator.allocate().unwrap();

        assert!(allocator.allocate_at(0x4001).is_none());
        assert!(allocator.allocate_at(0x3000).is_none());
        assert!(allocator.allocate_at(0x8000).is_none());
        assert!(allocator.allocate_at(taken.start()).is_none());
        assert_eq!(allocator.stats().allocated, 1);
    }

    #[test]
    fn allocate_at_frame_can_be_returned() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        let frame = allocator.allocate_at(0x7000).unwrap();

        allocator.deallocate(frame).unwrap();
        assert_eq!(allocator.stats().free, 4);
        assert_eq!(allocator.allocate_at(0x7000).unwrap().start(), 0x7000);
    }

    #[test]
    fn returns_none_after_exhaustion() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x6000).unwrap();
        assert_eq!(allocator.allocate().unwrap().start(), 0x4000);
        assert_eq!(allocator.allocate().unwrap().start(), 0x5000);
        assert_eq!(allocator.allocate(), None);
    }

    #[test]
    fn rejects_a_double_free() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        let frame = allocator.allocate().unwrap();
        let start = frame.start();
        allocator.deallocate(frame).unwrap();
        assert_eq!(
            allocator.deallocate(forged_frame_fixture(start).unwrap()),
            Err(FrameError::DoubleFree)
        );
    }

    #[test]
    fn rejects_an_out_of_range_frame() {
        let mut allocator = allocator_fixture::<1>(0x4000, 0x8000).unwrap();
        assert_eq!(
            allocator.deallocate(forged_frame_fixture(0x9000).unwrap()),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn rejects_an_unaligned_frame_start() {
        assert_eq!(forged_frame_fixture(0x4001), Err(FrameError::Unaligned));
    }

    #[test]
    fn rejects_unaligned_bounds() {
        assert_eq!(
            allocator_fixture::<1>(0x4001, 0x8000),
            Err(FrameError::Unaligned)
        );
    }

    #[test]
    fn rejects_an_empty_range() {
        assert_eq!(
            allocator_fixture::<1>(0x4000, 0x4000),
            Err(FrameError::EmptyRange)
        );
    }

    #[test]
    fn rejects_a_range_larger_than_its_bitmap() {
        assert_eq!(
            allocator_fixture::<1>(0x4000, 0x45000),
            Err(FrameError::CapacityExceeded)
        );
    }

    #[test]
    fn allocator_provenance_counter_refuses_to_wrap() {
        assert_eq!(advance_allocator_id(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(advance_allocator_id(u64::MAX), None);
    }
}
