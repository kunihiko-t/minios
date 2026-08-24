pub const FRAME_MAGIC: [u8; 4] = *b"MCF1";
pub const FRAME_HEADER_LEN: usize = 12;
pub const FRAME_MAX_PAYLOAD_LEN: u32 = 64 * 1024;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Ready = 1,
    Stdout = 2,
    Stderr = 3,
    Exit = 4,
    GuestError = 5,
    Diagnostic = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    pub payload_len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    WrongLength,
    WrongMagic,
    UnknownKind,
    NonZeroFlags,
    NonZeroReserved,
    PayloadTooLarge,
    WrongFixedPayloadLength,
}

impl FrameHeader {
    pub fn decode(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() != FRAME_HEADER_LEN {
            return Err(ControlError::WrongLength);
        }
        if bytes[0..4] != FRAME_MAGIC {
            return Err(ControlError::WrongMagic);
        }
        let kind = FrameKind::from_byte(bytes[4]).ok_or(ControlError::UnknownKind)?;
        if bytes[5] != 0 {
            return Err(ControlError::NonZeroFlags);
        }
        if bytes[6..8] != [0; 2] {
            return Err(ControlError::NonZeroReserved);
        }
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if payload_len > FRAME_MAX_PAYLOAD_LEN {
            return Err(ControlError::PayloadTooLarge);
        }
        if matches!(kind, FrameKind::Ready | FrameKind::Exit) && payload_len != 4 {
            return Err(ControlError::WrongFixedPayloadLength);
        }

        Ok(Self { kind, payload_len })
    }

    pub fn encode(self) -> [u8; FRAME_HEADER_LEN] {
        let mut bytes = [0; FRAME_HEADER_LEN];
        bytes[0..4].copy_from_slice(&FRAME_MAGIC);
        bytes[4] = self.kind as u8;
        bytes[8..12].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes
    }
}

impl FrameKind {
    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Ready),
            2 => Some(Self::Stdout),
            3 => Some(Self::Stderr),
            4 => Some(Self::Exit),
            5 => Some(Self::GuestError),
            6 => Some(Self::Diagnostic),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_header_bytes() -> [u8; FRAME_HEADER_LEN] {
        let mut bytes = [0; FRAME_HEADER_LEN];
        bytes[0..4].copy_from_slice(b"MCF1");
        bytes[4] = FrameKind::Stdout as u8;
        bytes[8..12].copy_from_slice(&4096_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn frame_header_round_trips() {
        let header = FrameHeader {
            kind: FrameKind::Stdout,
            payload_len: 4096,
        };

        let encoded = header.encode();
        let decoded = FrameHeader::decode(&encoded).unwrap();

        assert_eq!(decoded, header);
        assert_eq!(&encoded[0..4], b"MCF1");
    }

    #[test]
    fn rejects_headers_that_are_not_exactly_12_bytes() {
        let short = [0; FRAME_HEADER_LEN - 1];
        let long = [0; FRAME_HEADER_LEN + 1];

        for bytes in [&short[..], &long[..]] {
            assert_eq!(FrameHeader::decode(bytes), Err(ControlError::WrongLength));
        }
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = valid_header_bytes();
        bytes[0] = b'X';

        assert_eq!(FrameHeader::decode(&bytes), Err(ControlError::WrongMagic));
    }

    #[test]
    fn rejects_unknown_kind() {
        let mut bytes = valid_header_bytes();
        bytes[4] = 7;

        assert_eq!(FrameHeader::decode(&bytes), Err(ControlError::UnknownKind));
    }

    #[test]
    fn rejects_non_zero_flags() {
        let mut bytes = valid_header_bytes();
        bytes[5] = 1;

        assert_eq!(FrameHeader::decode(&bytes), Err(ControlError::NonZeroFlags));
    }

    #[test]
    fn rejects_non_zero_reserved() {
        let mut bytes = valid_header_bytes();
        bytes[6] = 1;

        assert_eq!(
            FrameHeader::decode(&bytes),
            Err(ControlError::NonZeroReserved)
        );
    }

    #[test]
    fn rejects_payload_larger_than_64_kibibytes() {
        let mut bytes = valid_header_bytes();
        bytes[8..12].copy_from_slice(&(FRAME_MAX_PAYLOAD_LEN + 1).to_le_bytes());

        assert_eq!(
            FrameHeader::decode(&bytes),
            Err(ControlError::PayloadTooLarge)
        );
    }

    #[test]
    fn rejects_ready_and_exit_with_wrong_fixed_payload_length() {
        let mut ready = valid_header_bytes();
        ready[4] = FrameKind::Ready as u8;
        ready[8..12].copy_from_slice(&3_u32.to_le_bytes());

        let mut exit = valid_header_bytes();
        exit[4] = FrameKind::Exit as u8;
        exit[8..12].copy_from_slice(&5_u32.to_le_bytes());

        for bytes in [ready, exit] {
            assert_eq!(
                FrameHeader::decode(&bytes),
                Err(ControlError::WrongFixedPayloadLength)
            );
        }
    }
}
