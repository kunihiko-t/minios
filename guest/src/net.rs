//! UDP socketの操作。
//!
//! `socket` 1回がkernel stackのsocket tableへ1 entryを割り当て、
//! `sendto`/`recvfrom`がdatagramを運ぶ。`recvfrom`は未受信なら
//! datagram到着までcaller processをblockする。

use minios_abi::syscall::SOCKADDR_LEN;

use crate::{Result, check, sys};

/// `sendto`/`recvfrom`が受け渡すIPv4 socket address。`ip`はdotted-quad順を
/// 整数で保持し（`10.0.2.15`なら`0x0A00020F`）、wire上はLE 8 byte
/// (`SOCKADDR_LEN`) としてkernelとやり取りする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SockAddr {
    pub ip: u32,
    pub port: u16,
}

impl SockAddr {
    /// `a.b.c.d`のbyte列からaddressを作る。
    pub const fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            ip: u32::from_be_bytes(ip),
            port,
        }
    }

    /// kernel ABIのLE 8 byteへserializeする。
    fn to_le_bytes(self) -> [u8; SOCKADDR_LEN] {
        let ip = self.ip.to_le_bytes();
        let port = self.port.to_le_bytes();
        [ip[0], ip[1], ip[2], ip[3], port[0], port[1], 0, 0]
    }

    /// `to_le_bytes`が書いた形式から読み戻す。
    fn from_le_bytes(bytes: [u8; SOCKADDR_LEN]) -> Self {
        Self {
            ip: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            port: u16::from_le_bytes([bytes[4], bytes[5]]),
        }
    }

    /// `ip`をdotted-quadのbyte列として返す。
    pub fn ip_bytes(self) -> [u8; 4] {
        self.ip.to_be_bytes()
    }
}

/// `socket`が返すUDP socket。socket indexはfile fdとは別の名前空間で、
/// 開いたprocessに所有される。明示的な`close` APIはなく、process終了時に
/// kernelが自動的に回収する。
#[derive(Debug)]
pub struct UdpSocket {
    index: usize,
}

impl UdpSocket {
    /// UDP socketを開く。
    pub fn new() -> Result<Self> {
        check(sys::sys_socket()).map(|index| Self { index })
    }

    /// `data`を`dst`へ1 datagramとして送る。戻り値は送ったbyte数。
    pub fn send_to(&self, data: &[u8], dst: SockAddr) -> Result<usize> {
        let addr = dst.to_le_bytes();
        check(sys::sys_sendto(
            self.index,
            data.as_ptr(),
            data.len(),
            addr.as_ptr(),
        ))
    }

    /// 1 datagramを`buf`へ最大`buf.len()` byte受け取り、受信byte数と
    /// 送信元を返す。未受信ならdatagram到着までblockする。
    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SockAddr)> {
        let mut addr = [0u8; SOCKADDR_LEN];
        let received = check(sys::sys_recvfrom(
            self.index,
            buf.as_mut_ptr(),
            buf.len(),
            addr.as_mut_ptr(),
        ))?;
        Ok((received, SockAddr::from_le_bytes(addr)))
    }
}
