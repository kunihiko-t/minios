//! MiniOS guestの`waitpid`サンプル。
//!
//! `spawn`で`DOCS/CHILD.ELF`を起動し、`waitpid`がchildの終了code 42を
//! 返すこと、reap済みpid・自分自身・存在しないpidが`ECHILD`を返すことを
//! 確認して42で終了する。childは`spawn-child`をstdoutへ書き`pid + 41` =
//! 42で終了する（disk image fixtureの`DOCS/CHILD.ELF`参照）。parentが
//! `waitpid`でblockするため、childのstdoutとexit frameは必ずparentの
//! `waitpid verified`より先に出る＝確定的なframe列になる。
//! 失敗時は70で終了する。E2Eのfile-waitpid検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{ECHILD, STDOUT};
use minios_guest::sys::{sys_exit, sys_spawn, sys_waitpid, sys_write};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: u32 = 42;
/// 単一imageのmanifestではこのguestが最初のprocessなのでpid 0。
const SELF_PID: usize = 0;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// disk image fixtureの`DOCS/CHILD.ELF`。childはpid 1 + 41 = 42で終了する。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
const MESSAGE: &[u8] = b"waitpid verified\n";

/// `_start`から呼ばれるRust本体。`spawn`→`waitpid`の往復とerrno経路を
/// 確認して42で終了する。
extern "C" fn guest_main() -> ! {
    // CHILD.ELFを新processとして起動する。childはpid 1を採番し、
    // `spawn-child`をstdoutへ書いて42で終了する。
    if sys_spawn(CHILD_PATH.as_ptr(), CHILD_PATH.len(), core::ptr::null(), 0) != CHILD_PID as isize
    {
        sys_exit(FAILURE_EXIT);
    }

    // childはpid+41=42で終了する。parentはここでblockされ、childの
    // stdoutとexit frameが先に出てからcode 42を回収する。
    if sys_waitpid(CHILD_PID) != SUCCESS_EXIT as isize {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：reap済みのpid、自分自身、存在しないpidはECHILD。
    if sys_waitpid(CHILD_PID) != ECHILD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_waitpid(SELF_PID) != ECHILD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_waitpid(99) != ECHILD {
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
