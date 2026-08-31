//! UART control frameの送信経路。Ready、Stdout、Stderr、Exit、GuestError、
//! Diagnosticの各frameを、headerの直後にpayloadが続く厳密なbyte列として
//! UARTへ載せる。hostはこの列を`minicontainer-protocol`のdecoderで検証する。

use minios_abi::boot::{BOOT_ABI_MAJOR, BOOT_ABI_MINOR};
use minios_abi::control::ReadyPayload;
use minios_abi::control::{FrameHeader, FrameKind};
use minios_kernel::user::syscall::ControlSink;

/// `dispatch_syscall`へ渡すUART sink。UARTのMMIO書き込みは失敗を返さない。
pub struct UartControlSink;

impl ControlSink for UartControlSink {
    type Error = ();

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error> {
        send_frame(kind, payload);
        Ok(())
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
