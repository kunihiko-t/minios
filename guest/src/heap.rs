//! `sbrk`の上に載せたfree-list allocator。
//!
//! programは`extern crate alloc;`と
//! `#[global_allocator] static ALLOC: SbrkAllocator = SbrkAllocator::new();`
//! で有効にし、`Vec`や`String`を使える。libは自分ではglobal allocatorを
//! 登録しないため、heapを使わないprogramには影響しない。
//!
//! 空き領域はaddress順に並べた単方向listで持つ。各空きblockの先頭16 byteに
//! 自分の大きさと次の空きblockのaddressを書くため、管理用の領域を別に持たない。
//! 割り当てたblockにはheaderを付けない。`dealloc`は割り当て時と同じ`Layout`を
//! 受け取るので、blockの大きさは同じ式（16 byte単位への切り上げ）で復元できる。
//! 解放したblockは前後の空きblockと結合し、heap末尾の空きが`TRIM_THRESHOLD`
//! 以上になれば`sbrk`の負のincrementでkernelへ返す。

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::null_mut,
};

use crate::sys::sys_sbrk;

/// blockの大きさと整列の単位。空きblockのnode（大きさとnext）が収まる。
const UNIT: usize = 16;
/// heapを伸ばすときの最小単位。小さな割り当てのたびに`sbrk`を呼ばない。
const GROW_CHUNK: usize = 4096;
/// heap末尾の空きがこれ以上になったら`sbrk`で返す。返してすぐ伸ばす往復を
/// 避けるため、1 page単位ではなく大きめにとる。
const TRIM_THRESHOLD: usize = 64 * 1024;

/// 空きblockの先頭に置くnode。
#[repr(C)]
struct FreeNode {
    size: usize,
    next: usize,
}

/// `layout`のblockが占めるbyte数。`alloc`と`dealloc`が同じ値を得る。
fn block_size(layout: Layout) -> Option<usize> {
    layout.size().max(1).checked_next_multiple_of(UNIT)
}

/// free-listの本体。`sbrk`は引数で受け取るため、host上のtestでも動かせる。
#[derive(Debug, Default)]
pub struct FreeList {
    /// address順で先頭の空きblock。0はlistが空であること。
    head: usize,
}

impl FreeList {
    pub const fn new() -> Self {
        Self { head: 0 }
    }

    /// `layout`を満たす領域を返す。空きblockで足りなければ`sbrk`でheapを
    /// 伸ばす。確保できなければnull。
    ///
    /// # Safety
    ///
    /// `sbrk`はこのlistだけが使うbreakを動かし、返すaddress範囲は書き込み
    /// 可能でなければならない。listの空きblockはすべてそのheapの中にある。
    pub unsafe fn alloc(
        &mut self,
        layout: Layout,
        sbrk: &mut dyn FnMut(isize) -> isize,
    ) -> *mut u8 {
        let Some(size) = block_size(layout) else {
            return null_mut();
        };
        let align = layout.align().max(UNIT);
        if let Some(address) = unsafe { self.take_fit(size, align) } {
            return address as *mut u8;
        }
        // 空きblockで足りないので、整列の余白も見込んでheapを伸ばす。
        let Some(request) = size
            .checked_add(align - UNIT)
            .and_then(|bytes| bytes.max(GROW_CHUNK).checked_next_multiple_of(UNIT))
        else {
            return null_mut();
        };
        let Ok(increment) = isize::try_from(request) else {
            return null_mut();
        };
        let old = sbrk(increment);
        if old < 0 {
            return null_mut();
        }
        let start = old as usize;
        // 初期breakはpage境界なので、以後のbreakも`UNIT`境界に揃っている。
        debug_assert!(start.is_multiple_of(UNIT));
        unsafe { self.insert(start, request) };
        unsafe { self.take_fit(size, align) }.map_or(null_mut(), |address| address as *mut u8)
    }

    /// `alloc`が返した`ptr`を空きへ戻す。heap末尾の空きが`TRIM_THRESHOLD`
    /// 以上になれば`sbrk`でkernelへ返す。
    ///
    /// # Safety
    ///
    /// `ptr`と`layout`は、同じlistの`alloc`が返したものと割り当て時の
    /// `Layout`でなければならない。`sbrk`は`alloc`と同じbreakを動かす。
    pub unsafe fn dealloc(
        &mut self,
        ptr: *mut u8,
        layout: Layout,
        sbrk: &mut dyn FnMut(isize) -> isize,
    ) {
        let Some(size) = block_size(layout) else {
            return;
        };
        unsafe { self.insert(ptr as usize, size) };
        unsafe { self.trim(sbrk) };
    }

    /// `size`と`align`を満たす最初の空きblockから領域を切り出す。前後の
    /// 余りは空きのまま残す。
    unsafe fn take_fit(&mut self, size: usize, align: usize) -> Option<usize> {
        let mut previous = 0usize;
        let mut current = self.head;
        while current != 0 {
            let node = unsafe { &*(current as *const FreeNode) };
            let (block_size, next) = (node.size, node.next);
            let start = current.checked_next_multiple_of(align)?;
            let end = current + block_size;
            if start
                .checked_add(size)
                .is_some_and(|used_end| used_end <= end)
            {
                let used_end = start + size;
                // まずこのblockをlistから外し、前後の余りを入れ直す。
                self.set_next(previous, next);
                if start > current {
                    unsafe { self.insert(current, start - current) };
                }
                if used_end < end {
                    unsafe { self.insert(used_end, end - used_end) };
                }
                return Some(start);
            }
            previous = current;
            current = next;
        }
        None
    }

    /// `[start, start + size)`を空きとしてaddress順の位置へ入れ、隣接する
    /// 空きblockと結合する。
    unsafe fn insert(&mut self, start: usize, size: usize) {
        let mut previous = 0usize;
        let mut current = self.head;
        while current != 0 && current < start {
            previous = current;
            current = unsafe { (*(current as *const FreeNode)).next };
        }
        let mut node = FreeNode {
            size,
            next: current,
        };
        // 後ろの空きblockと接していれば吸収する。
        if current != 0 && start + size == current {
            let following = unsafe { &*(current as *const FreeNode) };
            node.size += following.size;
            node.next = following.next;
        }
        // 前の空きblockと接していれば、そちらへ吸収させる。
        if previous != 0 {
            let before = unsafe { &mut *(previous as *mut FreeNode) };
            if previous + before.size == start {
                before.size += node.size;
                before.next = node.next;
                return;
            }
        }
        unsafe { (start as *mut FreeNode).write(node) };
        self.set_next(previous, start);
    }

    /// `previous`（0ならlistの先頭）の次を`next`にする。
    fn set_next(&mut self, previous: usize, next: usize) {
        if previous == 0 {
            self.head = next;
        } else {
            unsafe { (*(previous as *mut FreeNode)).next = next };
        }
    }

    /// address順で最後の空きblockがheap末尾に接し、`TRIM_THRESHOLD`以上なら
    /// `sbrk`でその分だけbreakを戻す。
    unsafe fn trim(&mut self, sbrk: &mut dyn FnMut(isize) -> isize) {
        let mut previous = 0usize;
        let mut current = self.head;
        if current == 0 {
            return;
        }
        loop {
            let next = unsafe { (*(current as *const FreeNode)).next };
            if next == 0 {
                break;
            }
            previous = current;
            current = next;
        }
        let size = unsafe { (*(current as *const FreeNode)).size };
        // 閾値に届かなければ`sbrk(0)`も呼ばない。breakはprogramが直接
        // `sbrk`で動かすこともあるため、手元に覚えず毎回kernelに尋ねる。
        if size < TRIM_THRESHOLD {
            return;
        }
        let brk = sbrk(0);
        if brk < 0 || current + size != brk as usize {
            return;
        }
        let Ok(decrement) = isize::try_from(size) else {
            return;
        };
        if sbrk(-decrement) == brk {
            self.set_next(previous, 0);
        }
    }

    /// 空きblockの`(address, size)`をaddress順に数える。testと診断用。
    pub fn free_blocks(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let mut current = self.head;
        core::iter::from_fn(move || {
            if current == 0 {
                return None;
            }
            let node = unsafe { &*(current as *const FreeNode) };
            let item = (current, node.size);
            current = node.next;
            Some(item)
        })
    }
}

/// guestのglobal allocator。`FreeList`をkernelの`sbrk`で動かす。
pub struct SbrkAllocator {
    list: UnsafeCell<FreeList>,
}

impl SbrkAllocator {
    pub const fn new() -> Self {
        Self {
            list: UnsafeCell::new(FreeList::new()),
        }
    }
}

impl Default for SbrkAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// Safety: guestは単一threadで、割り込みhandlerもuser modeでは走らないため、
// `list`へ同時に触れる経路はない。
unsafe impl Sync for SbrkAllocator {}

// Safety: 返す領域は`sbrk`で確保したU+R+Wのheapの中にあり、`layout`の大きさと
// 整列を満たし、他の割り当てと重ならない（空きlistから外してから返す）。
unsafe impl GlobalAlloc for SbrkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let list = unsafe { &mut *self.list.get() };
        unsafe { list.alloc(layout, &mut |increment| sys_sbrk(increment)) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let list = unsafe { &mut *self.list.get() };
        unsafe { list.dealloc(ptr, layout, &mut |increment| sys_sbrk(increment)) }
    }
}

#[cfg(test)]
mod tests {
    use super::{FreeList, GROW_CHUNK, TRIM_THRESHOLD, UNIT};
    use core::alloc::Layout;
    use std::{boxed::Box, vec::Vec};

    /// host上で`sbrk`を模すheap。page境界から始まる領域でbreakを動かし、
    /// 呼び出しを記録する。
    struct FakeHeap {
        _memory: Box<[u8]>,
        start: usize,
        brk: usize,
        limit: usize,
        calls: Vec<isize>,
    }

    impl FakeHeap {
        fn new(len: usize) -> Self {
            let memory = std::vec![0u8; len + 4096].into_boxed_slice();
            let start = (memory.as_ptr() as usize).next_multiple_of(4096);
            Self {
                _memory: memory,
                start,
                brk: start,
                limit: start + len,
                calls: Vec::new(),
            }
        }

        fn sbrk(&mut self, increment: isize) -> isize {
            self.calls.push(increment);
            let old = self.brk;
            let Some(new) = old.checked_add_signed(increment) else {
                return -12;
            };
            if new < self.start || new > self.limit {
                return -12;
            }
            self.brk = new;
            old as isize
        }
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    fn alloc(list: &mut FreeList, heap: &mut FakeHeap, layout: Layout) -> *mut u8 {
        unsafe { list.alloc(layout, &mut |increment| heap.sbrk(increment)) }
    }

    fn dealloc(list: &mut FreeList, heap: &mut FakeHeap, ptr: *mut u8, layout: Layout) {
        unsafe { list.dealloc(ptr, layout, &mut |increment| heap.sbrk(increment)) }
    }

    // Catches a freed block never being handed out again, which is what the
    // old bump allocator did: an equal-sized request after a free must reuse
    // the same address without growing the heap.
    #[test]
    fn freed_blocks_are_reused_without_growing_the_heap() {
        let mut heap = FakeHeap::new(1 << 20);
        let mut list = FreeList::new();
        let small = layout(40, 8);
        let a = alloc(&mut list, &mut heap, small);
        let b = alloc(&mut list, &mut heap, small);
        assert!(!a.is_null() && !b.is_null() && a != b);
        assert_eq!(heap.calls, [GROW_CHUNK as isize]);
        unsafe { a.write_bytes(0xaa, 40) };
        unsafe { b.write_bytes(0xbb, 40) };

        dealloc(&mut list, &mut heap, a, small);
        let c = alloc(&mut list, &mut heap, small);
        assert_eq!(c, a);
        assert_eq!(heap.calls.len(), 1);
        // 再利用したblockの隣は壊れていない。
        assert!(
            unsafe { core::slice::from_raw_parts(b, 40) }
                .iter()
                .all(|&x| x == 0xbb)
        );
    }

    // Catches fragmentation never healing: freeing neighbours in any order
    // must coalesce them into one block that a larger request can use.
    #[test]
    fn adjacent_free_blocks_coalesce() {
        let mut heap = FakeHeap::new(1 << 20);
        let mut list = FreeList::new();
        let unit = layout(UNIT * 4, UNIT);
        let blocks: Vec<*mut u8> = (0..4).map(|_| alloc(&mut list, &mut heap, unit)).collect();
        let grown = heap.calls.len();
        // 真ん中、先頭、2番目、末尾の順に解放しても1個にまとまる。
        for index in [2, 0, 1, 3] {
            dealloc(&mut list, &mut heap, blocks[index], unit);
        }
        let free: Vec<_> = list.free_blocks().collect();
        assert_eq!(free, [(blocks[0] as usize, GROW_CHUNK)]);
        let big = alloc(&mut list, &mut heap, layout(UNIT * 16, UNIT));
        assert_eq!(big, blocks[0]);
        assert_eq!(heap.calls.len(), grown);
    }

    // Catches over-aligned requests returning misaligned pointers or losing
    // the padding in front of them.
    #[test]
    fn over_aligned_requests_are_aligned_and_keep_the_padding_free() {
        let mut heap = FakeHeap::new(1 << 20);
        let mut list = FreeList::new();
        let _first = alloc(&mut list, &mut heap, layout(UNIT, UNIT));
        let aligned = alloc(&mut list, &mut heap, layout(256, 256));
        assert!((aligned as usize).is_multiple_of(256));
        // 先頭の16 byteと256境界の間の余白は空きとして残る。
        let padding = list
            .free_blocks()
            .find(|&(address, _)| address < aligned as usize);
        assert!(padding.is_some());
        let reused = alloc(&mut list, &mut heap, layout(UNIT, UNIT));
        assert!((reused as usize) < aligned as usize);
    }

    // Catches large frees never shrinking the heap or shrinking it while a
    // live block still sits above the freed region.
    #[test]
    fn a_large_free_tail_is_returned_with_sbrk() {
        let mut heap = FakeHeap::new(1 << 20);
        let mut list = FreeList::new();
        let big = layout(TRIM_THRESHOLD * 2, UNIT);
        let small = layout(UNIT, UNIT);
        let start = heap.start;
        let first = alloc(&mut list, &mut heap, big);
        let guard = alloc(&mut list, &mut heap, small);
        // 上に生きたblockがあるうちは返さない。
        dealloc(&mut list, &mut heap, first, big);
        assert!(heap.calls.iter().all(|&call| call >= 0));
        dealloc(&mut list, &mut heap, guard, small);
        // 末尾まで空いたので、空きをまとめてkernelへ返す。
        assert_eq!(heap.brk, start);
        assert_eq!(list.free_blocks().count(), 0);
        // 返した後も同じ大きさを再び確保できる。
        let again = alloc(&mut list, &mut heap, big);
        assert_eq!(again as usize, start);
    }

    // Catches allocation failure being reported as a dangling pointer: when
    // sbrk refuses to grow, alloc must return null and leave the list intact.
    #[test]
    fn exhaustion_returns_null_and_keeps_the_list_consistent() {
        let mut heap = FakeHeap::new(GROW_CHUNK);
        let mut list = FreeList::new();
        let small = layout(UNIT, UNIT);
        let a = alloc(&mut list, &mut heap, small);
        assert!(!a.is_null());
        assert!(alloc(&mut list, &mut heap, layout(GROW_CHUNK * 2, UNIT)).is_null());
        dealloc(&mut list, &mut heap, a, small);
        assert_eq!(
            list.free_blocks().collect::<Vec<_>>(),
            [(heap.start, GROW_CHUNK)]
        );
    }

    // Catches corruption that only shows up after many mixed operations:
    // random allocations and frees must never overlap a live block, must keep
    // every live block's contents, and must leave the free list sorted,
    // non-adjacent, and inside the heap.
    #[test]
    fn random_alloc_and_free_keep_blocks_disjoint_and_intact() {
        let mut heap = FakeHeap::new(4 << 20);
        let mut list = FreeList::new();
        let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for step in 0..2500u32 {
            if live.is_empty() || next() % 3 != 0 {
                let size = 1 + (next() % 3000) as usize;
                let align = 1usize << (next() % 8);
                let request = layout(size, align);
                let ptr = alloc(&mut list, &mut heap, request);
                assert!(!ptr.is_null(), "step {step}");
                assert!((ptr as usize).is_multiple_of(align));
                let tag = step as u8;
                unsafe { ptr.write_bytes(tag, size) };
                live.push((ptr, request, tag));
            } else {
                let index = (next() % live.len() as u64) as usize;
                let (ptr, request, tag) = live.swap_remove(index);
                let contents = unsafe { core::slice::from_raw_parts(ptr, request.size()) };
                assert!(contents.iter().all(|&byte| byte == tag), "step {step}");
                dealloc(&mut list, &mut heap, ptr, request);
            }

            // 生きているblock同士は重ならない。
            let mut spans: Vec<(usize, usize)> = live
                .iter()
                .map(|&(ptr, request, _)| (ptr as usize, ptr as usize + request.size()))
                .collect();
            spans.sort_unstable();
            assert!(
                spans.windows(2).all(|pair| pair[0].1 <= pair[1].0),
                "step {step}"
            );
            // 空きlistはaddress順で、隣と接さず、heapの中にあり、生きたblockと重ならない。
            let free: Vec<_> = list.free_blocks().collect();
            assert!(
                free.windows(2)
                    .all(|pair| pair[0].0 + pair[0].1 < pair[1].0)
            );
            for &(start, size) in &free {
                assert!(start >= heap.start && start + size <= heap.brk);
                assert!(spans.iter().all(|&(a, b)| b <= start || start + size <= a));
            }
        }
        for (ptr, request, _) in live.drain(..) {
            dealloc(&mut list, &mut heap, ptr, request);
        }
        // 全部返すと、heap末尾の空きはkernelへ返っている。
        assert!(heap.brk - heap.start < TRIM_THRESHOLD);
    }
}
