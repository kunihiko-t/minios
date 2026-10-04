//! MiniOS guestの`sbrk`/heapサンプル。
//!
//! `sbrk(0)`の初期breakがpage境界かつ`USER_START`以上であることを確かめ、
//! `SbrkAllocator`をglobal allocatorとして`Vec<u32>`へ複数page分を積んで
//! 中身を検証する。`alloc::format!`で組んだ`String`をstdoutへ書き、
//! `sbrk(-1)`の`EINVAL`と巨大な要求の`ENOMEM`がbreakを動かさないことも
//! 確かめて42で終了する。失敗時は70で終了する。E2Eのuser-heap検査が使う。

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec::Vec};
use core::arch::naked_asm;
use minios_abi::syscall::{EINVAL, ENOMEM, STDOUT};
use minios_guest::{
    heap::SbrkAllocator,
    sys::{sys_exit, sys_sbrk, sys_write},
};

#[global_allocator]
static ALLOC: SbrkAllocator = SbrkAllocator;

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// kernelのuser image下限（`USER_START`）。guest linker scriptの配置先と同じ。
const USER_START: isize = 0x0010_0000;
const PAGE_SIZE: isize = 4096;
/// 4 byte × 4096 = 16 KiB。Vecの再確保も含めて複数pageへまたがる。
const WORDS: u32 = 4096;
const MESSAGE: &[u8] = b"heap verified\n";

fn write_all(bytes: &[u8]) {
    if sys_write(STDOUT, bytes.as_ptr(), bytes.len()) != bytes.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
}

/// `_start`から呼ばれるRust本体。sbrkとheap割り当ての契約を順に確かめる。
extern "C" fn guest_main() -> ! {
    // 初期breakはimage末尾のpage境界にある。
    let start = sys_sbrk(0);
    if start < USER_START || start % PAGE_SIZE != 0 {
        sys_exit(FAILURE_EXIT);
    }

    let mut words = Vec::new();
    for index in 0..WORDS {
        words.push(index * 3 + 1);
    }
    let mut sum = 0u64;
    for (index, word) in words.iter().enumerate() {
        if *word != index as u32 * 3 + 1 {
            sys_exit(FAILURE_EXIT);
        }
        sum += u64::from(*word);
    }
    let grown = sys_sbrk(0);
    if grown < start + WORDS as isize * 4 {
        sys_exit(FAILURE_EXIT);
    }

    let line = format!("heap vec len={} sum={}\n", words.len(), sum);
    write_all(line.as_bytes());

    // 拒否された要求はbreakを動かさない。
    let before = sys_sbrk(0);
    if sys_sbrk(-1) != EINVAL || sys_sbrk(isize::MAX) != ENOMEM || sys_sbrk(0) != before {
        sys_exit(FAILURE_EXIT);
    }

    write_all(MESSAGE);
    sys_exit(SUCCESS_EXIT);
}

/// 初期`sp`はkernelが16 byte整列済み。
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    naked_asm!(
        "call {entry}",
        "j .",
        entry = sym guest_main,
    )
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    sys_exit(FAILURE_EXIT);
}
