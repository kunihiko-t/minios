//! `sbrk`の上に載せたbump allocator。
//!
//! programは`extern crate alloc;`と
//! `#[global_allocator] static ALLOC: SbrkAllocator = SbrkAllocator;`
//! で有効にし、`Vec`や`String`を使える。libは自分ではglobal allocatorを
//! 登録しないため、heapを使わないprogramには影響しない。

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::null_mut,
};

use crate::sys::sys_sbrk;

/// 現在のbreakを境界へ切り上げ、要求長だけbreakを進めて領域を返す。
/// 状態はkernelのbreakだけであり、guest側に変数を持たない。
pub struct SbrkAllocator;

// Safety: 返す領域は`sbrk`が新たに確保した、他のどの割り当てとも重ならない
// zero済みのU+R+W領域であり、`layout`の整列を満たす。guestは単一thread
// なので、`sbrk(0)`と`sbrk(increment)`の間にbreakは動かない。
unsafe impl GlobalAlloc for SbrkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let current = sys_sbrk(0);
        if current < 0 {
            return null_mut();
        }
        let current = current as usize;
        let Some(start) = current.checked_next_multiple_of(layout.align()) else {
            return null_mut();
        };
        let Some(end) = start.checked_add(layout.size()) else {
            return null_mut();
        };
        let Ok(increment) = isize::try_from(end - current) else {
            return null_mut();
        };
        if sys_sbrk(increment) != current as isize {
            return null_mut();
        }
        start as *mut u8
    }

    // ponytail: bump allocatorなので解放は何もしない。長く動くprogramで
    // 再利用が要るなら、free listを持つallocatorへ置き換える。
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
