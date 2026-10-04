//! MiniOS guestのfd継承サンプル。
//!
//! `open`したfileのfdを`lseek`でoffset 4へ進めてから`spawn`し、childが
//! 継承したfd 3からoffset以降の13 byte（`" inside docs\n"`）をstdoutへ
//! 写すことを確認する。child側のreadがparentのoffsetを動かさないこと
//! （snapshot semantics）もparent側の再読で確かめて42で終了する。
//! childはdisk image fixtureの`DOCS/FDCHILD.ELF`で、fd 3をreadして
//! 内容をstdoutへ出す。parentが`waitpid`でblockするため、childの
//! stdoutとexit frameは必ずparentの`fd-inherit verified`より先に出る
//! ＝確定的なframe列になる。
//! 失敗時は70で終了する。E2Eのfile-fdinherit検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{FIRST_FILE_FD, SEEK_SET, STDOUT};
use minios_guest::sys::{
    sys_exit, sys_lseek, sys_open, sys_read, sys_spawn, sys_waitpid, sys_write,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: u32 = 42;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// `DOCS/NOTE.TXT`の内容（17 byte）。offset 4以降が継承fd経由で読める。
const NOTE_PATH: &[u8] = b"DOCS/NOTE.TXT";
/// `DOCS/NOTE.TXT`のbyte長（`"note inside docs\n"`）。
const NOTE_LEN: usize = 17;
const CHILD_PATH: &[u8] = b"DOCS/FDCHILD.ELF";
/// offset 4以降の13 byte。childがstdoutへ写し、parent側でも再読して照合する。
const EXPECTED: &[u8] = b" inside docs\n";
const MESSAGE: &[u8] = b"fd-inherit verified\n";

/// `_start`から呼ばれるRust本体。fd継承の往復を確認して42で終了する。
extern "C" fn guest_main() -> ! {
    // NOTE.TXTを開いてoffset 4へ進める。childはこの位置をsnapshotで引き継ぐ。
    let fd = sys_open(NOTE_PATH.as_ptr(), NOTE_PATH.len());
    if fd != FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(
        fd as usize,
        NOTE_LEN as isize - EXPECTED.len() as isize,
        SEEK_SET,
    ) != 4
    {
        sys_exit(FAILURE_EXIT);
    }

    // childを起動する。FDCHILD.ELFはfd 3をreadしてstdoutへ写し42で終了する。
    if sys_spawn(CHILD_PATH.as_ptr(), CHILD_PATH.len()) != CHILD_PID as isize {
        sys_exit(FAILURE_EXIT);
    }

    // childの完了を待つ。parentはここでblockされ、childのstdoutとexit
    // frameが先に出てからcode 42を回収する。
    if sys_waitpid(CHILD_PID) != SUCCESS_EXIT as isize {
        sys_exit(FAILURE_EXIT);
    }

    // child側のreadがparentのoffsetを動かさないこと（snapshot semantics）
    // をparent側の再読で確かめる。
    let mut buffer = [0u8; 64];
    let read = sys_read(fd as usize, buffer.as_mut_ptr(), EXPECTED.len());
    if read != EXPECTED.len() as isize || buffer[..EXPECTED.len()] != *EXPECTED {
        sys_exit(FAILURE_EXIT);
    }

    if sys_write(STDOUT, MESSAGE.as_ptr(), MESSAGE.len()) != MESSAGE.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    sys_exit(SUCCESS_EXIT);
}

/// 初期`sp`はkernelが16 byte整列済み。`a0/a1`は第一・第二引数として
/// そのまま`guest_main`へ流れるため、register操作は不要である。
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
