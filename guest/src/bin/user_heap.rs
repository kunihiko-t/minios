//! MiniOS guestの`sbrk`/heapサンプル。
//!
//! `sbrk(0)`の初期breakがpage境界かつ`USER_START`以上であることを確かめ、
//! `SbrkAllocator`をglobal allocatorとして`Vec<u32>`へ複数page分を積んで
//! 中身を検証する。`alloc::format!`で組んだ`String`をstdoutへ書き、
//! `sbrk(-1)`の`EINVAL`と巨大な要求の`ENOMEM`がbreakを動かさないことも
//! 確かめて42で終了する。失敗時はpanicし、70で終了する。E2Eのuser-heap検査が使う。

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec::Vec};
use minios_abi::syscall::{EINVAL, ENOMEM};
use minios_guest::{Args, heap::SbrkAllocator, print, println, sys::sys_sbrk};

#[global_allocator]
static ALLOC: SbrkAllocator = SbrkAllocator;

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

    // heap上の`String`を1回の`write`で出す。
    let line = format!("heap vec len={} sum={}\n", words.len(), sum);
    print!("{line}");

    // 拒否された要求はbreakを動かさない。
    let before = sys_sbrk(0);
    assert!(sys_sbrk(-1) == EINVAL && sys_sbrk(isize::MAX) == ENOMEM && sys_sbrk(0) == before);

    println!("heap verified");
    42
}
