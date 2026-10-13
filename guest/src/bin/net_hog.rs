//! MiniOS guestのsocket所有権検査用サンプル。
//!
//! socket tableを満杯にしてすぐ終了する。終了時に所有socketが自動回収
//! されれば、後続process（net-udp検査の`net_udp`）は同じindexを再利用
//! できる。回収されなければ後続の`socket`は`EMFILE`で失敗する。
//! E2Eのnet-sock検査がこのbinaryを走らせる。
//!
//! table獲得の順序はschedulerのdispatch次第なので、net-udpが先に
//! slotを掴むrunもある。その場合hogは`new_waiting`でnet-udpの終了と
//! socket回収を待つ。net-udpが終わればhogの4個目は必ず取れるので、
//! table満杯の状態は必ず作れる。

#![no_std]
#![no_main]

use minios_guest::{Args, net::UdpSocket, println};

/// kernelのsocket table上限 (`stack.rs`の`MAX_SOCKETS`)。
const MAX_SOCKETS: usize = 4;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut held = [None, None, None, None];
    for (index, slot) in held.iter_mut().enumerate() {
        match UdpSocket::new_waiting() {
            Ok(sock) => *slot = Some(sock),
            Err(errno) => {
                println!("net-hog: socket {index} failed errno={}", errno.0);
                return 70;
            }
        }
    }
    // hogが4個保持している時点でtableは必ず満杯なので、5個目は
    // 他processの動きに関わらず`EMFILE`で失敗する。
    match UdpSocket::new() {
        Err(_) => {}
        Ok(_) => {
            println!("net-hog: fifth socket unexpectedly opened");
            return 70;
        }
    }
    println!("net-hog: held {MAX_SOCKETS} sockets");
    42
}
