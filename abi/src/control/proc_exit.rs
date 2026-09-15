pub const PROC_EXIT_PAYLOAD_LEN: usize = 8;

/// 複数image bundleの1 processが終了したことをhostへ通知するpayload。
/// `pid`はmanifest内のimage index (0始まり) で、終了codeと対にして送る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcExitPayload {
    pub pid: u32,
    pub code: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcExitPayloadError {
    WrongLength,
}

impl ProcExitPayload {
    pub fn decode(bytes: &[u8]) -> Result<Self, ProcExitPayloadError> {
        if bytes.len() != PROC_EXIT_PAYLOAD_LEN {
            return Err(ProcExitPayloadError::WrongLength);
        }

        Ok(Self {
            pid: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            code: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }

    pub fn encode(self) -> [u8; PROC_EXIT_PAYLOAD_LEN] {
        let mut bytes = [0; PROC_EXIT_PAYLOAD_LEN];
        bytes[0..4].copy_from_slice(&self.pid.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.code.to_le_bytes());
        bytes
    }
}
