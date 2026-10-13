//! MiniOS guestのsocket所有権検査用サンプル。
//!
//! socket tableを満杯にしてすぐ終了する。終了時に所有socketが自動回収
//! されれば、後続process（net-udp検査の`net_udp`）は同じindexを再利用
//! できる。回収されなければ後続の`socket`は`EMFILE`で失敗する。
//! E2Eのnet-sock検査がこのbinaryを先に走らせる。

#![no_std]
#![no_main]

use minios_guest::{Args, net::UdpSocket, println};

/// kernelのsocket table上限 (`stack.rs`の`MAX_SOCKETS`)。
const MAX_SOCKETS: usize = 4;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut held = [None, None, None, None];
    for (index, slot) in held.iter_mut().enumerate() {
        match UdpSocket::new() {
            Ok(sock) => *slot = Some(sock),
            Err(errno) => {
                println!("net-hog: socket {index} failed errno={}", errno.0);
                return 70;
            }
        }
    }
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
