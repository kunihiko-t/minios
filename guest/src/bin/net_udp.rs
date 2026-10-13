//! MiniOS guestの`socket`/`sendto`/`recvfrom`サンプル。
//!
//! QEMU user-netのhost alias (`10.0.2.2:7777`) へ1 datagramを
//! 送り、echoされた同じpayloadを`recvfrom`で待つ。`recvfrom`は未受信で
//! blockし、virtio-netのRX割り込みで起きたretryがdatagramを届ける。
//! 内容と送信元を検証してから42で終了する。失敗時はpanicし、70で終了する。
//! E2Eのnet-udp検査が使う。

#![no_std]
#![no_main]

use minios_guest::{
    Args,
    net::{SockAddr, UdpSocket},
    println,
};

/// QEMU user-netでは`10.0.2.2`がhost aliasで、xtaskが`127.0.0.1:7777`に
/// 立てたecho socketへ届く。
const ECHO_ADDR: SockAddr = SockAddr::new([10, 0, 2, 2], 7777);
const PAYLOAD: &[u8] = b"minios udp echo\n";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // net-sock検査ではnet-hogがtableを満杯にしている間に起きるため、
    // 解放を待つ`new_waiting`で開く。単独のnet-udp検査では直ちに取れる。
    let socket = UdpSocket::new_waiting().unwrap();
    assert_eq!(socket.send_to(PAYLOAD, ECHO_ADDR), Ok(PAYLOAD.len()));

    let mut buf = [0u8; 512];
    let (received, source) = socket.recv_from(&mut buf).unwrap();
    assert_eq!(received, PAYLOAD.len());
    assert_eq!(&buf[..received], PAYLOAD);
    assert_eq!(source, ECHO_ADDR);

    let ip = source.ip_bytes();
    println!(
        "net-udp: echo {received} bytes from {}.{}.{}.{}:{}",
        ip[0], ip[1], ip[2], ip[3], source.port
    );
    42
}
