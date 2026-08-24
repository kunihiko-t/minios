pub const READY_PAYLOAD_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyPayload {
    pub abi_major: u16,
    pub abi_minor: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyPayloadError {
    WrongLength,
}

impl ReadyPayload {
    pub fn decode(bytes: &[u8]) -> Result<Self, ReadyPayloadError> {
        if bytes.len() != READY_PAYLOAD_LEN {
            return Err(ReadyPayloadError::WrongLength);
        }

        Ok(Self {
            abi_major: u16::from_le_bytes([bytes[0], bytes[1]]),
            abi_minor: u16::from_le_bytes([bytes[2], bytes[3]]),
        })
    }

    pub fn encode(self) -> [u8; READY_PAYLOAD_LEN] {
        let mut bytes = [0; READY_PAYLOAD_LEN];
        bytes[0..2].copy_from_slice(&self.abi_major.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.abi_minor.to_le_bytes());
        bytes
    }
}
