pub const PAGE_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysFrame(usize);

impl PhysFrame {
    pub fn from_start(start: usize) -> Result<Self, FrameError> {
        // 物理ページは MMU とハードウェアの 4 KiB 境界でしか表せないため、下位 bit を検査する。
        if start % PAGE_SIZE != 0 {
            return Err(FrameError::Unaligned);
        }
        Ok(Self(start))
    }

    pub const fn start(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    EmptyRange,
    Unaligned,
    CapacityExceeded,
    OutOfRange,
    DoubleFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub total: usize,
    pub allocated: usize,
    pub free: usize,
}

/// 物理 frame の所有権は一つの bitmap にだけ対応付ける。
///
/// ```compile_fail
/// use minios_kernel::memory::frame::FrameAllocator;
///
/// let allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
/// // 複製すると同じ物理ページを二つの allocator が返せるため、所有者は複製できない。
/// let duplicate = allocator.clone();
/// let _ = duplicate;
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct FrameAllocator<const WORDS: usize> {
    base: usize,
    frame_count: usize,
    allocated: usize,
    bitmap: [u64; WORDS],
}

impl<const WORDS: usize> FrameAllocator<WORDS> {
    pub fn new(base: usize, end: usize) -> Result<Self, FrameError> {
        // base と end は 4 KiB ページ境界でなければ、bitmap の一 bit を一物理ページへ安全に対応付けられない。
        if base % PAGE_SIZE != 0 || end % PAGE_SIZE != 0 {
            return Err(FrameError::Unaligned);
        }
        if base >= end {
            return Err(FrameError::EmptyRange);
        }

        // 上端は base より大きいことを確認済みなので、この差分はオーバーフローせずページ数を表す。
        let frame_count = (end - base) / PAGE_SIZE;
        let capacity = WORDS.checked_mul(u64::BITS as usize).unwrap_or(usize::MAX);
        if frame_count > capacity {
            return Err(FrameError::CapacityExceeded);
        }

        Ok(Self {
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
                // frame_index は frame_count 未満で、new が検証した範囲内なのでこの物理アドレスは有効範囲から出ない。
                let start = self.base + frame_index * PAGE_SIZE;
                return Some(PhysFrame(start));
            }
        }
        None
    }

    pub fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        let start = frame.start();
        // PhysFrame は通常 from_start で生成されるが、型の将来変更にも備えて解放境界でも 4 KiB 整列を再確認する。
        if start % PAGE_SIZE != 0 {
            return Err(FrameError::Unaligned);
        }
        if start < self.base {
            return Err(FrameError::OutOfRange);
        }

        // start >= base を確認済みなので、この差分は安全に bitmap 用のページ番号へ変換できる。
        let frame_index = (start - self.base) / PAGE_SIZE;
        if frame_index >= self.frame_count {
            return Err(FrameError::OutOfRange);
        }

        let word_index = frame_index / u64::BITS as usize;
        let bit_index = frame_index % u64::BITS as usize;
        let bit = 1_u64 << bit_index;
        if self.bitmap[word_index] & bit == 0 {
            return Err(FrameError::DoubleFree);
        }
        self.bitmap[word_index] &= !bit;
        self.allocated -= 1;
        Ok(())
    }

    pub const fn stats(&self) -> FrameStats {
        FrameStats {
            total: self.frame_count,
            allocated: self.allocated,
            free: self.frame_count - self.allocated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameAllocator, FrameError, PhysFrame};

    #[test]
    fn allocates_and_reuses_a_frame() {
        let mut allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
        let first = allocator.allocate().unwrap();
        let second = allocator.allocate().unwrap();
        assert_eq!(first.start(), 0x4000);
        assert_eq!(second.start(), 0x5000);
        allocator.deallocate(first).unwrap();
        assert_eq!(allocator.allocate().unwrap(), first);
    }

    #[test]
    fn returns_none_after_exhaustion() {
        let mut allocator = FrameAllocator::<1>::new(0x4000, 0x6000).unwrap();
        assert_eq!(allocator.allocate().unwrap().start(), 0x4000);
        assert_eq!(allocator.allocate().unwrap().start(), 0x5000);
        assert_eq!(allocator.allocate(), None);
    }

    #[test]
    fn rejects_a_double_free() {
        let mut allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
        let frame = allocator.allocate().unwrap();
        allocator.deallocate(frame).unwrap();
        assert_eq!(allocator.deallocate(frame), Err(FrameError::DoubleFree));
    }

    #[test]
    fn rejects_an_out_of_range_frame() {
        let mut allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
        assert_eq!(
            allocator.deallocate(PhysFrame::from_start(0x9000).unwrap()),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn rejects_an_unaligned_frame_start() {
        assert_eq!(PhysFrame::from_start(0x4001), Err(FrameError::Unaligned));
    }

    #[test]
    fn rejects_unaligned_bounds() {
        assert_eq!(
            FrameAllocator::<1>::new(0x4001, 0x8000),
            Err(FrameError::Unaligned)
        );
    }

    #[test]
    fn rejects_an_empty_range() {
        assert_eq!(
            FrameAllocator::<1>::new(0x4000, 0x4000),
            Err(FrameError::EmptyRange)
        );
    }

    #[test]
    fn rejects_a_range_larger_than_its_bitmap() {
        assert_eq!(
            FrameAllocator::<1>::new(0x4000, 0x45000),
            Err(FrameError::CapacityExceeded)
        );
    }
}
