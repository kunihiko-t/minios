//! UART control frameの送受信経路。Ready、Stdout、Stderr、Exit、GuestError、
//! Diagnosticの各frameを、headerの直後にpayloadが続く厳密なbyte列として
//! UARTへ載せる。hostはこの列を`minicontainer-protocol`のdecoderで検証する。
//! 受信はStdin frameだけをpull型で読み、`read`の要求分だけ配る。

use minios_abi::boot::{BOOT_ABI_MAJOR, BOOT_ABI_MINOR};
use minios_abi::control::ReadyPayload;
use minios_abi::control::{FrameHeader, FrameKind};
use minios_kernel::user::stdin::{ByteReader, StdinError, StdinStaging};
use minios_kernel::user::syscall::{ControlSink, ControlSource};

/// `dispatch_syscall`へ渡すUART sink。UARTのMMIO書き込みは失敗を返さない。
pub struct UartControlSink;

impl ControlSink for UartControlSink {
    type Error = ();

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error> {
        send_frame(kind, payload);
        Ok(())
    }
}

struct UartBytes;

impl ByteReader for UartBytes {
    fn read_byte(&mut self) -> u8 {
        crate::console::read_byte()
    }

    fn try_read_byte(&mut self) -> Option<u8> {
        // 受信FIFOを空見てから読むため、この経路は決して受信待ちで停まらない。
        crate::console::stdin_pending().then(crate::console::read_byte)
    }
}

/// `dispatch_syscall`へ渡すUART source。Stdin frameをnon-blockingな
/// `try_read_byte`で引き、frame途中でbyteが尽きた場合もstagingの再開可能な
/// stateが保持される。`WouldBlock`は`Ok(None)`へ写像し、`dispatch_read`が
/// `Blocked`へ変換してprocessをstdin待ちへ回す。
/// stagingはrun単位のstaticが所有し、trapごとに借りて渡す。
pub struct UartControlSource<'a> {
    staging: &'a mut StdinStaging,
}

impl<'a> UartControlSource<'a> {
    pub const fn new(staging: &'a mut StdinStaging) -> Self {
        Self { staging }
    }
}

impl ControlSource for UartControlSource<'_> {
    type Error = StdinError;

    fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        match self.staging.read(&mut UartBytes, output) {
            Ok(count) => Ok(Some(count)),
            Err(StdinError::WouldBlock) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn send_frame(kind: FrameKind, payload: &[u8]) {
    let header = FrameHeader {
        kind,
        payload_len: payload.len() as u32,
    }
    .encode();
    // headerを送ってからpayloadを送る順序を、host側decoderの契約として守る。
    crate::console::write_bytes(&header);
    crate::console::write_bytes(payload);
}

/// guestの実行準備が整ったことをhostへ通知し、以降のUARTをcontrol frameへ限定する。
/// QEMU user testのkernelはpayload経由でだけ呼ぶため、このTaskでは未使用である。
#[allow(dead_code)]
pub fn send_ready() {
    let payload = ReadyPayload {
        abi_major: BOOT_ABI_MAJOR,
        abi_minor: BOOT_ABI_MINOR,
    }
    .encode();
    send_frame(FrameKind::Ready, &payload);
    // Ready以降はplain console textを混在させない。
    crate::console::enter_control_mode();
}

pub fn send_guest_error(message: &[u8]) {
    send_frame(FrameKind::GuestError, message);
}

pub fn send_diagnostic(message: &[u8]) {
    send_frame(FrameKind::Diagnostic, message);
}
