//! `socket`/`sendto`/`recvfrom`のdispatch test。user address
//! 検証、`SockAddr`のLE変換、`RecvComplete`の二領域copy layout、
//! 未受信時の`Blocked`巻き戻しをfixtureで確かめる。

use super::*;

const SOCKET_NUMBER: usize = SyscallNumber::Socket as usize;
const SENDTO_NUMBER: usize = SyscallNumber::SendTo as usize;
const RECVFROM_NUMBER: usize = SyscallNumber::RecvFrom as usize;
/// `sendto`/`recvfrom`の`a3` (sockaddr pointer) が指すpage内offset。
const ADDR_OFFSET: usize = 0x800;

/// socket操作をscript化したsource。実際のnetwork stackは持たず、
/// 呼び出し引数を記録して決められた応答を返す。
struct NetSource {
    socket_result: Result<usize, isize>,
    send_result: Result<usize, isize>,
    recv_result: Result<Option<([u8; 4], u16, usize)>, isize>,
    sent: Option<(usize, Vec<u8>, [u8; 4], u16)>,
    recv_sock: Option<usize>,
}

impl Default for NetSource {
    /// 全opが`ENOSYS`で、未受信だけ`Ok(None)`を返す既定値。
    fn default() -> Self {
        Self {
            socket_result: Err(ENOSYS),
            send_result: Err(ENOSYS),
            recv_result: Ok(None),
            sent: None,
            recv_sock: None,
        }
    }
}

impl ControlSource for NetSource {
    type Error = SinkError;

    fn read_stdin(&mut self, _output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    fn socket(&mut self) -> Result<usize, isize> {
        self.socket_result
    }

    fn send_to(
        &mut self,
        sock: usize,
        data: &[u8],
        dst_ip: [u8; 4],
        dst_port: u16,
    ) -> Result<usize, isize> {
        if self.send_result.is_ok() {
            self.sent = Some((sock, Vec::from(data), dst_ip, dst_port));
        }
        self.send_result
    }

    fn recv_from(
        &mut self,
        sock: usize,
        _output: &mut [u8],
    ) -> Result<Option<([u8; 4], u16, usize)>, isize> {
        self.recv_sock = Some(sock);
        self.recv_result
    }
}

/// `a3`も設定できるfixture。`dispatch_fixture_at`が`a0`〜`a2`と番号を
/// 再設定するため、先に作ったcontextで`a3`を残せる。
fn net_fixture<R: ControlSource<Error = SinkError>>(
    number: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    sink: &mut FakeSink,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> (UserContext, SyscallFlow<SinkError>) {
    let mut context = syscall_context(number, a0, a1, a2);
    context.set_register(13, a3);
    dispatch_fixture_at(context, number, a0, a1, a2, sink, source, read_scratch)
}

#[test]
fn socket_returns_source_index() {
    let mut sink = FakeSink::default();
    let mut source = NetSource {
        socket_result: Ok(0),
        ..NetSource::default()
    };
    let mut scratch = scratch();
    let (context, flow) =
        dispatch_fixture(SOCKET_NUMBER, 0, 0, 0, &mut sink, &mut source, &mut scratch);
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 0);
}

#[test]
fn socket_maps_default_source_to_enosys() {
    // FakeSourceはsocketを実装しないため、trait defaultのENOSYSが返る。
    let mut sink = FakeSink::default();
    let mut source = FakeSource::scripted(b"");
    let mut scratch = scratch();
    let (context, flow) =
        dispatch_fixture(SOCKET_NUMBER, 0, 0, 0, &mut sink, &mut source, &mut scratch);
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10) as isize, ENOSYS);
}

#[test]
fn sendto_passes_sockaddr_and_payload_to_source() {
    let mut sink = FakeSink::default();
    let mut source = NetSource {
        send_result: Ok(4),
        ..NetSource::default()
    };
    let mut scratch = scratch();
    // payloadはfixtureが`MESSAGE_PAGE`へ書いた`MESSAGE` ("MK4") の先頭4 byte。
    // `a3`は同pageの未初期化領域を指し、sockaddrの8 byteは全てゼロとして
    // copyされる。copy経路の妥当性がこのtestの検査対象である。
    let (context, flow) = net_fixture(
        SENDTO_NUMBER,
        3,
        MESSAGE_PAGE,
        4,
        MESSAGE_PAGE + ADDR_OFFSET,
        &mut sink,
        &mut source,
        &mut scratch,
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10), 4);
    assert_eq!(
        source.sent,
        Some((3, Vec::from(&b"MK4\0"[..]), [0, 0, 0, 0], 0))
    );
}

#[test]
fn sendto_rejects_unmapped_sockaddr_pointer() {
    let mut sink = FakeSink::default();
    let mut source = NetSource {
        send_result: Ok(4),
        ..NetSource::default()
    };
    let mut scratch = scratch();
    let (context, flow) = net_fixture(
        SENDTO_NUMBER,
        3,
        MESSAGE_PAGE,
        4,
        0x5000, // 未mapのアドレス
        &mut sink,
        &mut source,
        &mut scratch,
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10) as isize, EFAULT);
    assert_eq!(source.sent, None);
}

#[test]
fn recvfrom_empty_blocks_and_rewinds() {
    let mut sink = FakeSink::default();
    let mut source = NetSource::default(); // recv_result = Ok(None)
    let mut scratch = scratch();
    let (context, flow) = net_fixture(
        RECVFROM_NUMBER,
        0,
        MESSAGE_PAGE,
        64,
        MESSAGE_PAGE + ADDR_OFFSET,
        &mut sink,
        &mut source,
        &mut scratch,
    );
    assert_eq!(flow, SyscallFlow::Blocked);
    // `errno_flow(EAGAIN)`はsepcを4 byte戻して同じecallをやり直す。
    assert_eq!(context.sepc(), 0x0010_0500 - 4);
    assert_eq!(source.recv_sock, Some(0));
}

#[test]
fn recvfrom_received_returns_recv_complete_with_sockaddr() {
    let mut sink = FakeSink::default();
    let mut source = NetSource {
        recv_result: Ok(Some(([10, 0, 2, 99], 7777, 5))),
        ..NetSource::default()
    };
    let mut scratch = scratch();
    let (_context, flow) = net_fixture(
        RECVFROM_NUMBER,
        0,
        MESSAGE_PAGE,
        64,
        MESSAGE_PAGE + ADDR_OFFSET,
        &mut sink,
        &mut source,
        &mut scratch,
    );
    let SyscallFlow::RecvComplete {
        start,
        len,
        addr_start,
    } = flow
    else {
        panic!("expected RecvComplete, actual {flow:?}");
    };
    assert_eq!(start, MESSAGE_PAGE as u64);
    assert_eq!(len, 5);
    assert_eq!(addr_start, (MESSAGE_PAGE + ADDR_OFFSET) as u64);
    // payloadの直後に`{ip u32 LE, port u16 LE, pad}`が置かれる。
    // ip=10.0.2.99は整数`0x0A000263`としてLEで格納される。
    assert_eq!(scratch[5..9], 0x0A00_0263u32.to_le_bytes());
    assert_eq!(scratch[9..11], 7777u16.to_le_bytes());
    assert_eq!(scratch[11..13], [0, 0]);
}

#[test]
fn recvfrom_bad_socket_maps_badf() {
    let mut sink = FakeSink::default();
    let mut source = NetSource {
        recv_result: Err(EBADF),
        ..NetSource::default()
    };
    let mut scratch = scratch();
    let (context, flow) = net_fixture(
        RECVFROM_NUMBER,
        9,
        MESSAGE_PAGE,
        64,
        MESSAGE_PAGE + ADDR_OFFSET,
        &mut sink,
        &mut source,
        &mut scratch,
    );
    assert_eq!(flow, SyscallFlow::Resume);
    assert_eq!(context.register(10) as isize, EBADF);
}
