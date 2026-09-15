//! 固定領域上のfirst-fit free-listヒープ。
//!
//! `FrameAllocator`が扱う4 KiBページより細かいkernel objectを収めるため、
//! managed RAMの末尾から切り出した固定領域を16バイト単位で分割・併合する。
//! ヒープ自身はメモリーを所有せず、初期化時に与えられた領域だけを管理する。
//!
//! 各割り当ては16バイトの`Span` headerを伴う。headerはこの割り当てが占有する
//! 領域全体（先頭のalignment端数や最小ブロック未満の末尾端数を吸収した分を
//! 含む）の先頭と長さを記録し、`dealloc`は返されたポインターの直前から
//! 読み戻す。空きブロックはaddress昇順の単方向リストで管理し、挿入時に
//! 前後と連続するブロックを併合する。

use core::alloc::Layout;
use core::ptr::NonNull;

/// headerと空きブロックnodeの最小サイズを兼ねる割り当て粒度。
/// `Span`も`FreeNode`も2ワードを要するため16バイトが下限である。
const GRANULE: usize = 16;
const SPAN_LEN: usize = core::mem::size_of::<Span>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapError {
    /// 領域の先頭または長さが粒度にそろっていない。
    Misaligned,
    /// 領域が最小ブロックより小さい。
    EmptyRange,
    /// 要求を収める連続した空きブロックがない。
    OutOfMemory,
    /// `dealloc`へヒープ領域外か、内容が壊れたポインターが渡された。
    InvalidPointer,
    /// 解放しようとしたspanが既存の空きブロックと重なる＝二重解放。
    DoubleFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// 管理対象の総バイト数。
    pub total: usize,
    /// 割り当て済みspanの合計バイト数（header・端数込み）。
    pub allocated: usize,
    /// 空きブロックの合計バイト数。
    pub free: usize,
    /// 空きブロックの個数。併合が働くほど小さくなる。
    pub free_blocks: usize,
    /// 最大の連続空きブロックのバイト数。
    pub largest_free: usize,
}

/// 割り当ての直前に置く2ワードのheader。
/// `start`/`len`はこの割り当てが占有する領域全体を指す。
#[repr(C)]
struct Span {
    start: usize,
    len: usize,
}

/// 空きブロックの先頭に埋め込むlist node。
/// `size`はこのブロック全体のバイト数を指す。
#[repr(C)]
struct FreeNode {
    size: usize,
    next: Option<NonNull<FreeNode>>,
}

pub struct Heap {
    head: Option<NonNull<FreeNode>>,
    start: usize,
    end: usize,
    allocated: usize,
}

impl Heap {
    /// 管理領域を持たない空のヒープを作る。`init`するまで`alloc`は失敗する。
    pub const fn empty() -> Self {
        Self {
            head: None,
            start: 0,
            end: 0,
            allocated: 0,
        }
    }

    /// `[start, start + len)`をヒープ領域として登録し、初期状態を空き
    /// ブロック一つにする。起動直後の一度だけ呼ぶこと。
    ///
    /// # Safety
    ///
    /// 呼び出し側は、その領域をこのヒープが排他的に使い、ほかの所有者や
    /// エイリアスする参照と共有しないことを保証する。
    pub unsafe fn init(&mut self, start: usize, len: usize) -> Result<(), HeapError> {
        if !start.is_multiple_of(GRANULE) || !len.is_multiple_of(GRANULE) {
            return Err(HeapError::Misaligned);
        }
        if len < GRANULE {
            return Err(HeapError::EmptyRange);
        }
        let end = start.checked_add(len).ok_or(HeapError::Misaligned)?;
        // Safety: 呼び出し側の契約により`start`は`len`バイトの排他的領域を指す。
        unsafe {
            (start as *mut FreeNode).write(FreeNode {
                size: len,
                next: None,
            });
        }
        self.head = NonNull::new(start as *mut FreeNode);
        self.start = start;
        self.end = end;
        self.allocated = 0;
        Ok(())
    }

    /// `layout`を満たす最初の空きブロックを分割して割り当てる。
    pub fn alloc(&mut self, layout: Layout) -> Result<NonNull<u8>, HeapError> {
        let size = layout.size().max(1);
        let align = layout.align().max(GRANULE);
        if !align.is_power_of_two() {
            return Err(HeapError::Misaligned);
        }

        let mut previous: Option<NonNull<FreeNode>> = None;
        let mut current = self.head;
        while let Some(node_ptr) = current {
            // Safety: list上のnodeはいずれもヒープ領域内の有効な空きブロックを指す。
            let node = unsafe { node_ptr.as_ref() };
            let block_start = node_ptr.as_ptr() as usize;
            let block_end = block_start + node.size;
            // headerは返すポインターの直前に置くため、payloadは
            // `block_start + SPAN_LEN`以降で`align`へそろえる。
            // spanの終端は粒度へ切り上げ、`dealloc`の検査が常に通る形にする。
            let payload = (block_start + SPAN_LEN)
                .checked_add(align - 1)
                .map(|v| v & !(align - 1));
            let span_end = payload
                .and_then(|p| p.checked_add(size))
                .and_then(|p_end| p_end.checked_add(GRANULE - 1))
                .map(|v| v & !(GRANULE - 1));
            let fits = span_end.is_some_and(|end| end <= block_end);
            if let (true, Some(payload), Some(span_end)) = (fits, payload, span_end) {
                // Safety: 上の条件によりspanはブロック内に収まる。
                return Ok(unsafe { self.carve(previous, node_ptr, payload, span_end, block_end) });
            }
            previous = current;
            current = node.next;
        }
        Err(HeapError::OutOfMemory)
    }

    /// `ptr`が指す割り当てを解放し、隣接する空きブロックと併合する。
    ///
    /// # Safety
    ///
    /// `ptr`はこのヒープの`alloc`が返したポインターであり、まだ解放されて
    /// いないことを呼び出し側が保証する。spanが領域内かつ既存の空き
    /// ブロックと重ならないかは実行時に検査し、違反は`Err`で拒否する。
    pub unsafe fn dealloc(&mut self, ptr: NonNull<u8>) -> Result<(), HeapError> {
        let header_at = ptr.as_ptr() as usize - SPAN_LEN;
        if header_at < self.start || header_at + SPAN_LEN > self.end {
            return Err(HeapError::InvalidPointer);
        }
        // Safety: `ptr`が`alloc`由来なら直前にheaderがある。範囲外は先に拒否した。
        let span = unsafe { (header_at as *const Span).read() };
        // headerはspan内にあり、spanは領域内で粒度へそろっている必要がある。
        // 最小のspanでは`span.start == header_at`となる。
        let valid = span.start.is_multiple_of(GRANULE)
            && span.len.is_multiple_of(GRANULE)
            && span.start >= self.start
            && span.start <= header_at
            && span
                .start
                .checked_add(span.len)
                .is_some_and(|end| end <= self.end && header_at + SPAN_LEN <= end);
        if !valid {
            return Err(HeapError::InvalidPointer);
        }
        self.insert_free(span.start, span.start + span.len)
    }

    pub fn stats(&self) -> HeapStats {
        let mut free = 0;
        let mut free_blocks = 0;
        let mut largest_free = 0;
        let mut current = self.head;
        while let Some(node_ptr) = current {
            // Safety: listはヒープ領域内の有効な空きブロックだけを指す。
            let node = unsafe { node_ptr.as_ref() };
            free += node.size;
            free_blocks += 1;
            largest_free = largest_free.max(node.size);
            current = node.next;
        }
        HeapStats {
            total: self.end - self.start,
            allocated: self.allocated,
            free,
            free_blocks,
            largest_free,
        }
    }

    /// `node_ptr`の空きブロックから`payload`を含み`span_end`で終わるspanを
    /// 切り出す。spanは常にブロック先頭から始まるため先頭のalignment端数は
    /// span内のpaddingとなり、末尾の残りが最小ブロック以上なら新しい空き
    /// ブロックとしてlistへ残す。
    ///
    /// # Safety
    ///
    /// `node_ptr`は現在有効なlist nodeであり、spanがブロック内に収まることを
    /// `alloc`が確認済みである。
    unsafe fn carve(
        &mut self,
        previous: Option<NonNull<FreeNode>>,
        node_ptr: NonNull<FreeNode>,
        payload: usize,
        span_end: usize,
        block_end: usize,
    ) -> NonNull<u8> {
        // Safety: `node_ptr`は有効なlist nodeである。
        let node = unsafe { &mut *node_ptr.as_ptr() };
        let block_start = node_ptr.as_ptr() as usize;
        let next = node.next;
        // 末尾の残りが最小ブロック未満ならspanへ吸収する。
        let remainder = block_end - span_end;
        let (span_end, remainder) = if remainder < GRANULE {
            (block_end, 0)
        } else {
            (span_end, remainder)
        };
        let span_len = span_end - block_start;
        // Safety: span内`payload`の直前は`alloc`が確保した範囲内である。
        unsafe {
            ((payload - SPAN_LEN) as *mut Span).write(Span {
                start: block_start,
                len: span_len,
            });
        }
        if remainder >= GRANULE {
            // Safety: span_end以降はこのspanが占有しないブロック内の残りである。
            unsafe {
                (span_end as *mut FreeNode).write(FreeNode {
                    size: remainder,
                    next,
                });
            }
            let remainder_ptr = NonNull::new(span_end as *mut FreeNode);
            match previous {
                // Safety: prevは有効なlist nodeである。
                Some(prev) => unsafe { (*prev.as_ptr()).next = remainder_ptr },
                None => self.head = remainder_ptr,
            }
        } else {
            match previous {
                // Safety: prevは有効なlist nodeである。
                Some(prev) => unsafe { (*prev.as_ptr()).next = next },
                None => self.head = next,
            }
        }
        self.allocated += span_len;
        NonNull::new(payload as *mut u8).expect("payload is never null")
    }

    /// `[start, end)`をaddress昇順のlistへ挿入し、前後と連続する
    /// ブロックがあれば併合する。既存ブロックとの重複は二重解放として拒否する。
    fn insert_free(&mut self, start: usize, end: usize) -> Result<(), HeapError> {
        let mut previous: Option<NonNull<FreeNode>> = None;
        let mut current = self.head;
        while let Some(node_ptr) = current {
            let node_start = node_ptr.as_ptr() as usize;
            if node_start >= end {
                break;
            }
            // Safety: list nodeは有効な空きブロックを指す。
            let node_end = node_start + unsafe { node_ptr.as_ref() }.size;
            if node_end > start {
                // 新しいspanが既存の空きブロックと重なる＝その領域は既に空き扱い。
                return Err(HeapError::DoubleFree);
            }
            previous = current;
            current = unsafe { node_ptr.as_ref() }.next;
        }

        // 直前ブロックと連続するなら併合し、さらに直後ブロックとも連続なら続けて併合する。
        if let Some(prev) = previous {
            let prev_start = prev.as_ptr() as usize;
            // Safety: prevは有効なlist nodeである。
            let prev_node = unsafe { &mut *prev.as_ptr() };
            if prev_start + prev_node.size == start {
                prev_node.size += end - start;
                self.allocated -= end - start;
                self.coalesce_forward(prev);
                return Ok(());
            }
        }
        // 直後ブロックと連続するなら、併合済みの大きさで新しいnodeを書く。
        if let Some(next_ptr) = current
            && next_ptr.as_ptr() as usize == end
        {
            // Safety: next_ptrは有効なlist nodeである。
            let next_node = unsafe { &mut *next_ptr.as_ptr() };
            let merged = end - start + next_node.size;
            let next_next = next_node.next;
            // Safety: `start`は解放するspanの先頭であり、最小ブロックを収める。
            unsafe {
                (start as *mut FreeNode).write(FreeNode {
                    size: merged,
                    next: next_next,
                });
            }
            self.link_after(previous, start);
            self.allocated -= end - start;
            return Ok(());
        }
        // Safety: `start`は解放するspanの先頭であり、最小ブロックを収める。
        unsafe {
            (start as *mut FreeNode).write(FreeNode {
                size: end - start,
                next: current,
            });
        }
        self.link_after(previous, start);
        self.allocated -= end - start;
        Ok(())
    }

    /// `prev`ブロックと直後のブロックが連続していれば併合する。
    fn coalesce_forward(&mut self, prev: NonNull<FreeNode>) {
        // Safety: prevは有効なlist nodeである。
        let node = unsafe { &mut *prev.as_ptr() };
        let prev_end = prev.as_ptr() as usize + node.size;
        if let Some(next) = node.next
            && next.as_ptr() as usize == prev_end
        {
            // Safety: nextは有効なlist nodeである。
            let next_node = unsafe { &*next.as_ptr() };
            node.size += next_node.size;
            node.next = next_node.next;
        }
    }

    /// `previous`の直後（なければhead）に`start`のnodeを繋ぐ。
    fn link_after(&mut self, previous: Option<NonNull<FreeNode>>, start: usize) {
        let node_ptr = NonNull::new(start as *mut FreeNode);
        match previous {
            // Safety: prevは有効なlist nodeである。
            Some(prev) => unsafe { (*prev.as_ptr()).next = node_ptr },
            None => self.head = node_ptr,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::boxed::Box;
    use std::vec::Vec;

    use super::{Heap, HeapError};
    use core::alloc::Layout;
    use core::ptr::NonNull;

    /// 16バイト整列を保証するヒープ裏付け領域。
    #[repr(align(16))]
    struct Region([u8; 4096]);

    fn fresh_heap() -> (Heap, Box<Region>) {
        let mut heap = Heap::empty();
        let region = Box::new(Region([0; 4096]));
        let start = region.0.as_ptr() as usize;
        unsafe { heap.init(start, 4096) }.unwrap();
        (heap, region)
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size, 8).unwrap()
    }

    #[test]
    fn init_rejects_misaligned_or_tiny_regions() {
        let region = Box::new(Region([0; 4096]));
        let start = region.0.as_ptr() as usize;

        let mut heap = Heap::empty();
        assert_eq!(
            unsafe { heap.init(start + 8, 4096) },
            Err(HeapError::Misaligned)
        );
        let mut heap = Heap::empty();
        assert_eq!(unsafe { heap.init(start, 24) }, Err(HeapError::Misaligned));
        let mut heap = Heap::empty();
        assert_eq!(unsafe { heap.init(start, 0) }, Err(HeapError::EmptyRange));
        let mut heap = Heap::empty();
        assert_eq!(unsafe { heap.init(start, 16) }, Ok(()));
    }

    #[test]
    fn alloc_returns_aligned_writable_memory() {
        let (mut heap, _region) = fresh_heap();

        let ptr = heap.alloc(layout(24)).unwrap();
        assert!(ptr.as_ptr() as usize % 16 == 0);
        // Safety: allocが返した24バイトは有効である。
        unsafe {
            ptr.as_ptr().write_bytes(0xab, 24);
            assert_eq!(*ptr.as_ptr(), 0xab);
        }
    }

    #[test]
    fn alloc_honors_larger_alignment() {
        let (mut heap, _region) = fresh_heap();
        let layout = Layout::from_size_align(8, 64).unwrap();

        let ptr = heap.alloc(layout).unwrap();
        assert_eq!(ptr.as_ptr() as usize % 64, 0);
        // 解放しても壊れないことを確認する。
        unsafe { heap.dealloc(ptr) }.unwrap();
    }

    #[test]
    fn exhausted_heap_reports_out_of_memory() {
        let (mut heap, _region) = fresh_heap();

        let mut ptrs = Vec::new();
        while let Ok(ptr) = heap.alloc(layout(128)) {
            ptrs.push(ptr);
        }
        let stats = heap.stats();
        assert_eq!(stats.total, 4096);
        // 各spanはheader込み144バイトを占めるため、64バイトだけ余る。
        assert_eq!(stats.free, 64);
        assert_eq!(stats.allocated, 4032);
        assert!(matches!(
            heap.alloc(layout(128)),
            Err(HeapError::OutOfMemory)
        ));

        for ptr in ptrs {
            unsafe { heap.dealloc(ptr) }.unwrap();
        }
        let stats = heap.stats();
        assert_eq!(stats.free, 4096);
        // 全部解放すれば一つのブロックへ併合される。
        assert_eq!(stats.free_blocks, 1);
    }

    #[test]
    fn dealloc_coalesces_adjacent_free_blocks() {
        let (mut heap, _region) = fresh_heap();

        let a = heap.alloc(layout(64)).unwrap();
        let b = heap.alloc(layout(64)).unwrap();
        let c = heap.alloc(layout(64)).unwrap();
        assert_eq!(heap.stats().free_blocks, 1);

        // 先頭のaだけ解放: 末尾の空きとは連続しないため二つのブロックになる。
        unsafe { heap.dealloc(a) }.unwrap();
        assert_eq!(heap.stats().free_blocks, 2);

        // bはaの空きブロックと連続するため併合される。使用中のcが末尾の
        // 空きとの間を隔てるため、ブロック数は二つのままである。
        unsafe { heap.dealloc(b) }.unwrap();
        assert_eq!(heap.stats().free_blocks, 2);

        // cを解放するとa+bのブロックと末尾の空きの両方と連続し、
        // 領域全体が一つのブロックへ併合される。
        unsafe { heap.dealloc(c) }.unwrap();
        let stats = heap.stats();
        assert_eq!(stats.free_blocks, 1);
        assert_eq!(stats.free, 4096);
        assert_eq!(stats.allocated, 0);
    }

    #[test]
    fn dealloc_rejects_double_free() {
        let (mut heap, _region) = fresh_heap();
        let ptr = heap.alloc(layout(64)).unwrap();
        unsafe { heap.dealloc(ptr) }.unwrap();

        assert!(matches!(
            unsafe { heap.dealloc(ptr) },
            Err(HeapError::DoubleFree) | Err(HeapError::InvalidPointer)
        ));
    }

    #[test]
    fn dealloc_rejects_pointers_outside_the_region() {
        let (mut heap, _region) = fresh_heap();
        let outside = NonNull::new(0x1000 as *mut u8).unwrap();
        assert_eq!(
            unsafe { heap.dealloc(outside) },
            Err(HeapError::InvalidPointer)
        );
    }

    #[test]
    fn first_fit_reuses_the_earliest_sufficient_block() {
        let (mut heap, _region) = fresh_heap();
        let a = heap.alloc(layout(256)).unwrap();
        let b = heap.alloc(layout(256)).unwrap();
        let _c = heap.alloc(layout(256)).unwrap();
        unsafe { heap.dealloc(b) }.unwrap();

        // 32バイトの要求はbのブロック（先頭側の空き）から切り出される。
        let small = heap.alloc(layout(32)).unwrap();
        assert_eq!(small.as_ptr() as usize, b.as_ptr() as usize);
        unsafe { heap.dealloc(small) }.unwrap();
        unsafe { heap.dealloc(a) }.unwrap();
    }

    #[test]
    fn stats_track_every_transition() {
        let (mut heap, _region) = fresh_heap();
        assert_eq!(
            heap.stats(),
            super::HeapStats {
                total: 4096,
                allocated: 0,
                free: 4096,
                free_blocks: 1,
                largest_free: 4096,
            }
        );
        let ptr = heap.alloc(layout(64)).unwrap();
        let stats = heap.stats();
        assert_eq!(stats.total, 4096);
        assert_eq!(stats.allocated + stats.free, 4096);
        assert!(stats.largest_free <= stats.free);
        unsafe { heap.dealloc(ptr) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
    }
}
