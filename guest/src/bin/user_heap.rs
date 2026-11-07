//! MiniOS guestの`sbrk`/heapサンプル。
//!
//! `sbrk(0)`の初期breakがpage境界かつ`USER_START`以上であることを確かめ、
//! `SbrkAllocator`をglobal allocatorとして`Vec<u32>`へ複数page分を積んで
//! 中身を検証する。同じ大きさの確保と解放を繰り返してもbreakが伸びない
//! こと（解放した領域の再利用）と、大きな領域の解放でbreakが戻ること
//! （heap末尾の空きのkernelへの返却）も確かめる。`alloc::format!`で組んだ
//! `String`をstdoutへ書き、
//! heap先頭より下へ戻す`EINVAL`と巨大な要求の`ENOMEM`がbreakを動かさない
//! ことを確かめる。続けて2 page伸ばして書き込んでから負のincrementで戻し、
//! 再び伸ばしたpageがzeroに戻っている（返却したframeの代わりに新しい
//! frameがmapされた）ことも確かめて42で終了する。失敗時はpanicし、70で
//! 終了する。E2Eのuser-heap検査が使う。

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec, vec::Vec};
use minios_abi::syscall::{EINVAL, ENOMEM};
use minios_guest::{Args, heap::SbrkAllocator, print, println, sys::sys_sbrk};

#[global_allocator]
static ALLOC: SbrkAllocator = SbrkAllocator::new();

/// kernelのuser image下限（`USER_START`）。guest linker scriptの配置先と同じ。
const USER_START: isize = 0x0010_0000;
const PAGE_SIZE: isize = 4096;
/// 4 byte × 4096 = 16 KiB。Vecの再確保も含めて複数pageへまたがる。
const WORDS: u32 = 4096;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // 初期breakはimage末尾のpage境界にある。
    let start = sys_sbrk(0);
    assert!(start >= USER_START && start % PAGE_SIZE == 0);

    let mut words = Vec::new();
    for index in 0..WORDS {
        words.push(index * 3 + 1);
    }
    let mut sum = 0u64;
    for (index, word) in words.iter().enumerate() {
        assert_eq!(*word, index as u32 * 3 + 1);
        sum += u64::from(*word);
    }
    assert!(sys_sbrk(0) >= start + WORDS as isize * 4);

    // 解放した領域は再利用される。1回目の確保でheapが伸びた後は、同じ大きさの
    // 確保と解放を何度繰り返してもbreakは動かない。
    drop(vec![0u8; 8192]);
    let settled = sys_sbrk(0);
    for round in 0..64u8 {
        let buffer = vec![round; 8192];
        assert!(buffer.iter().all(|&byte| byte == round));
    }
    assert_eq!(sys_sbrk(0), settled);

    // heap末尾の大きな空きはkernelへ返り、breakが戻る。
    let big = vec![1u8; 256 * 1024];
    let grown = sys_sbrk(0);
    assert!(grown >= settled + 256 * 1024);
    drop(big);
    assert!(sys_sbrk(0) < grown);

    // heap上の`String`を1回の`write`で出す。
    let line = format!("heap vec len={} sum={}\n", words.len(), sum);
    print!("{line}");

    // 拒否された要求はbreakを動かさない。heap先頭より1 byteでも下は`EINVAL`。
    let before = sys_sbrk(0);
    let below_start = -(before - start) - 1;
    assert!(sys_sbrk(below_start) == EINVAL);
    assert!(sys_sbrk(isize::MAX) == ENOMEM && sys_sbrk(0) == before);

    // allocatorの管理外で2 page伸ばして書き、負のincrementで同じだけ戻す。
    // 戻した範囲のpageはunmapされるため、再び伸ばすと中身はzeroである。
    // 部分pageはbreakが残る限りmapされたままなので、先にpage境界まで進める。
    let pad = (PAGE_SIZE - before % PAGE_SIZE) % PAGE_SIZE;
    assert_eq!(sys_sbrk(pad), before);
    let grow = 2 * PAGE_SIZE;
    let base = sys_sbrk(grow);
    assert_eq!(base, before + pad);
    let region = base as *mut u8;
    // Safety: `[base, base + grow)`は直前の`sbrk`がmapしたばかりのheapで、
    // allocatorはこの範囲を払い出していない。
    unsafe { core::ptr::write_bytes(region, 0xab, grow as usize) };
    assert_eq!(sys_sbrk(-grow), base + grow);
    assert_eq!(sys_sbrk(0), base);
    assert_eq!(sys_sbrk(grow), base);
    // Safety: 同じ範囲を再び`sbrk`でmapした直後で、allocatorは触れていない。
    let regrown = unsafe { core::slice::from_raw_parts(region, grow as usize) };
    assert!(regrown.iter().all(|byte| *byte == 0));
    assert_eq!(sys_sbrk(-grow), base + grow);

    println!("heap verified");
    42
}
