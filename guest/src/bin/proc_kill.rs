//! proc-kill検査用guest。errno契約を確認した後、stdin待ちのvictim
//! 二つへkillを送り、正常終了する。
//!
//! killの前には`procinfo`でprocess tableを読み、waiter（pid 0）が`waitpid`、
//! victim二つ（pid 1と2）がstdin待ち、自分（pid 3）が実行中として、この順に
//! 並ぶことを確かめる。killの後は、victimが回収待ち（`killed`）として残るか、
//! 既に回収されて一覧から消えていることを確かめる。
#![no_std]
#![no_main]

use minios_abi::syscall::{
    ECHILD, EINVAL, PROC_STATE_KILLED, PROC_STATE_RUNNABLE, PROC_STATE_STDIN, PROC_STATE_WAITING,
};
use minios_guest::{
    Args, Errno, println,
    process::{getpid, kill, processes},
};
minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // killの前のprocess table。pidの小さい順に並ぶ。
    let mut listed = processes().map(|info| {
        let info = info.unwrap();
        (info.pid, info.state, info.name, info.name_len)
    });
    for (pid, state, name) in [
        (0, PROC_STATE_WAITING, &b"waiter"[..]),
        (1, PROC_STATE_STDIN, b"blocked1"),
        (2, PROC_STATE_STDIN, b"blocked2"),
        (3, PROC_STATE_RUNNABLE, b"killer"),
    ] {
        let (seen_pid, seen_state, seen_name, len) = listed.next().unwrap();
        assert_eq!((seen_pid, seen_state), (pid, state));
        assert_eq!(&seen_name[..len as usize], name);
    }
    assert!(listed.next().is_none());

    // 自分自身へのkillは`exit`の役目としてEINVAL、不在pidはECHILD。
    assert_eq!(kill(getpid()), Err(Errno(EINVAL)));
    assert_eq!(kill(99), Err(Errno(ECHILD)));
    // stdin待ちで止まっているvictim二つへ終了要求。tableに残る間の
    // 二度目のkillもidempotentに0を返す。
    for pid in [1, 2] {
        assert_eq!(kill(pid), Ok(()));
        assert_eq!(kill(pid), Ok(()));
    }
    // victimはrun loopが回収するまで`killed`で残る。回収済みなら一覧に無い。
    for info in processes() {
        let info = info.unwrap();
        if matches!(info.pid, 1 | 2) {
            assert_eq!(info.state, PROC_STATE_KILLED);
        }
    }
    println!("kill syscalls verified");
    7
}
