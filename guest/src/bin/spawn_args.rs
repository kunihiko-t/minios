//! MiniOS guestの`spawn` argvサンプル。
//!
//! disk image fixtureの`DOCS/ECHO.ELF`（argvを順にstdoutへ書いて42で
//! 終了する`minios-guest`）を起動する。先にargvの誤りを確かめる:
//! argcの上限超過と長すぎる文字列は`EINVAL`、読めないargv列や文字列は
//! `EFAULT`で、どれもchildを作らない。続いてargc=0で起動してargvが
//! basenameの`ECHO.ELF`だけであることを、argc=3で
//! `echoargs`、`alpha`、`beta gamma`がそのまま届くことを示す。
//! 失敗したspawnがpidを消費していればchildのpidがずれて70で終了する。
//! 最後に`spawn args verified`を出して42で終了する。E2Eのspawn-args検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::{
    manifest::ARG_MAX_LEN,
    syscall::{EFAULT, EINVAL, SPAWN_MAX_ARGC, STDOUT},
};
use minios_guest::sys::{sys_exit, sys_spawn, sys_waitpid, sys_write};

const FAILURE_EXIT: u32 = 70;
const SUCCESS_EXIT: u32 = 42;
const ECHO_PATH: &[u8] = b"DOCS/ECHO.ELF";
const MESSAGE: &[u8] = b"spawn args verified\n";
/// 上限を1 byte超える文字列。
static LONG: [u8; ARG_MAX_LEN + 1] = [b'a'; ARG_MAX_LEN + 1];

fn check(condition: bool) {
    if !condition {
        sys_exit(FAILURE_EXIT);
    }
}

fn arg(text: &[u8]) -> [u64; 2] {
    [text.as_ptr() as u64, text.len() as u64]
}

fn spawn(argv: &[[u64; 2]]) -> isize {
    sys_spawn(
        ECHO_PATH.as_ptr(),
        ECHO_PATH.len(),
        argv.as_ptr(),
        argv.len(),
    )
}

extern "C" fn guest_main() -> ! {
    // どの誤りもchildを作らず、pidも消費しない。
    let many = [arg(b"x"); SPAWN_MAX_ARGC + 1];
    check(spawn(&many) == EINVAL);
    check(spawn(&[arg(b"echoargs"), arg(&LONG)]) == EINVAL);
    check(sys_spawn(ECHO_PATH.as_ptr(), ECHO_PATH.len(), core::ptr::null(), 1) == EFAULT);
    check(spawn(&[arg(b"echoargs"), [0, 1]]) == EFAULT);

    // argc=0は従来どおりbasenameだけがargv[0]になる。
    check(spawn(&[]) == 1);
    check(sys_waitpid(1) == SUCCESS_EXIT as isize);

    let argv = [arg(b"echoargs"), arg(b"alpha"), arg(b"beta gamma")];
    check(spawn(&argv) == 2);
    check(sys_waitpid(2) == SUCCESS_EXIT as isize);

    check(sys_write(STDOUT, MESSAGE.as_ptr(), MESSAGE.len()) == MESSAGE.len() as isize);
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
