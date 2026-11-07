//! user shellの`ps`（`BIN/PS.ELF`）。
//!
//! process tableのprocessを、pidの小さい順に`<pid> <状態> <名前>`の形で
//! 1行1件ずつstdoutへ書く。先頭行は見出しである。状態は`run`（dispatch待ちか
//! 実行中）、`sleep`（`sleep`の経過待ち）、`wait`（`waitpid`）、`stdin`、
//! `pipe`、`io`（block device）、`killed`（回収待ち）のどれかで、列の幅は
//! 固定する。一覧を読めなければstderrへ1行出して1で終了する。

#![no_std]
#![no_main]

use minios_abi::syscall::{
    PROC_STATE_IO, PROC_STATE_KILLED, PROC_STATE_PIPE, PROC_STATE_RUNNABLE, PROC_STATE_SLEEPING,
    PROC_STATE_STDIN, PROC_STATE_WAITING,
};
use minios_guest::{Args, Errno, eprintln, println, process::processes};

/// `PROC_STATE_*`を表示用の語へ直す。
fn state_name(state: u32) -> &'static str {
    match state {
        PROC_STATE_RUNNABLE => "run",
        PROC_STATE_SLEEPING => "sleep",
        PROC_STATE_WAITING => "wait",
        PROC_STATE_STDIN => "stdin",
        PROC_STATE_PIPE => "pipe",
        PROC_STATE_IO => "io",
        PROC_STATE_KILLED => "killed",
        _ => "?",
    }
}

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    println!("  PID STATE  NAME");
    for info in processes() {
        match info {
            Ok(info) => {
                let name = &info.name[..info.name_len as usize];
                println!(
                    "{:>5} {:<6} {}",
                    info.pid,
                    state_name(info.state),
                    core::str::from_utf8(name).unwrap_or("?")
                );
            }
            Err(Errno(errno)) => {
                eprintln!("ps: cannot read the process table (errno {errno})");
                return 1;
            }
        }
    }
    0
}
