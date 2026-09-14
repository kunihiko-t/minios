//! host→guestのstdin byte stream受信。pull型でframeを順に消費する。
//!
//! guestの`read`はこのstagingから取り出す。stagingが空でEOF未達のときだけ
//! UARTから次のframeを1個読む。長さ0のStdin frameがEOFであり、一度EOFに
//! 達したらreaderへ二度と触れない。frame長上限（4 KiB）はABIのdecoderが
//! 強制するため、ここでは`FrameHeader::decode`の判定をそのまま使う。

use minios_abi::{
    control::{FRAME_HEADER_LEN, FrameHeader, FrameKind},
    syscall::MAX_READ_LEN,
};

/// stdinのbyte供給元。UARTもhost testのsliceもこの形で読む。
pub trait ByteReader {
    fn read_byte(&mut self) -> u8;

    fn read_exact(&mut self, output: &mut [u8]) {
        for slot in output.iter_mut() {
            *slot = self.read_byte();
        }
    }
}

/// 未配達のStdin frame断片。1 frame分（最大4 KiB）だけ保持する。
pub struct StdinStaging {
    bytes: [u8; MAX_READ_LEN],
    start: usize,
    end: usize,
    eof: bool,
}

impl Default for StdinStaging {
    fn default() -> Self {
        Self::new()
    }
}

/// Stdin frameの受信が続けられない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinError {
    /// Stdin以外のframe種別が届いた。
    UnexpectedKind(FrameKind),
    /// headerのdecodeに失敗した。
    Decode(minios_abi::control::ControlError),
}

impl StdinStaging {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_READ_LEN],
            start: 0,
            end: 0,
            eof: false,
        }
    }

    pub const fn is_eof(&self) -> bool {
        self.eof
    }

    /// 未配達byteを`output`へ移す。空でEOF未達なら1 frame読む。
    ///
    /// 戻り値は移したbyte数であり、0はEOFを意味する。`output`が空のときは
    /// readerへ触れず0を返す。
    pub fn read<R: ByteReader>(
        &mut self,
        reader: &mut R,
        output: &mut [u8],
    ) -> Result<usize, StdinError> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.start == self.end && !self.eof {
            self.pull_frame(reader)?;
        }
        let available = &self.bytes[self.start..self.end];
        let count = core::cmp::min(available.len(), output.len());
        output[..count].copy_from_slice(&available[..count]);
        self.start += count;
        Ok(count)
    }

    fn pull_frame<R: ByteReader>(&mut self, reader: &mut R) -> Result<(), StdinError> {
        let mut header_bytes = [0u8; FRAME_HEADER_LEN];
        reader.read_exact(&mut header_bytes);
        let header = FrameHeader::decode(&header_bytes).map_err(StdinError::Decode)?;
        if header.kind != FrameKind::Stdin {
            return Err(StdinError::UnexpectedKind(header.kind));
        }
        let len = header.payload_len as usize;
        if len == 0 {
            self.eof = true;
            self.start = 0;
            self.end = 0;
            return Ok(());
        }
        // decodeがMAX_READ_LEN以下を保証するため、範囲外書き込みは起きない。
        reader.read_exact(&mut self.bytes[..len]);
        self.start = 0;
        self.end = len;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{ByteReader, StdinError, StdinStaging};
    use minios_abi::{
        control::{ControlError, FrameHeader, FrameKind},
        syscall::MAX_READ_LEN,
    };

    struct SliceReader<'a> {
        bytes: &'a [u8],
        position: usize,
    }

    impl<'a> SliceReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self { bytes, position: 0 }
        }

        fn consumed(&self) -> usize {
            self.position
        }
    }

    impl ByteReader for SliceReader<'_> {
        fn read_byte(&mut self) -> u8 {
            let byte = self.bytes[self.position];
            self.position += 1;
            byte
        }
    }

    fn stdin_frame(payload: &[u8]) -> Vec<u8> {
        let header = FrameHeader {
            kind: FrameKind::Stdin,
            payload_len: payload.len() as u32,
        }
        .encode();
        let mut frame = Vec::from(header);
        frame.extend_from_slice(payload);
        frame
    }

    fn read_all(staging: &mut StdinStaging, reader: &mut SliceReader<'_>) -> Vec<u8> {
        let mut collected = Vec::new();
        let mut chunk = [0u8; 3];
        loop {
            match staging.read(reader, &mut chunk).unwrap() {
                0 => return collected,
                count => collected.extend_from_slice(&chunk[..count]),
            }
        }
    }

    // Catches dropping bytes, reordering frames, or missing the EOF terminator.
    #[test]
    fn delivers_frames_in_order_and_stops_at_eof() {
        let mut stream = stdin_frame(b"ab");
        stream.extend_from_slice(&stdin_frame(b"cdef"));
        stream.extend_from_slice(&stdin_frame(b""));
        let mut reader = SliceReader::new(&stream);
        let mut staging = StdinStaging::new();

        assert_eq!(read_all(&mut staging, &mut reader), b"abcdef");
        assert!(staging.is_eof());
        assert_eq!(reader.consumed(), stream.len());
    }

    // Catches touching the reader after EOF or after an empty output request.
    #[test]
    fn empty_and_post_eof_reads_never_touch_the_reader() {
        let stream = stdin_frame(b"");
        let mut reader = SliceReader::new(&stream);
        let mut staging = StdinStaging::new();

        let mut empty = [];
        assert_eq!(staging.read(&mut reader, &mut empty), Ok(0));
        assert_eq!(reader.consumed(), 0);

        let mut output = [0xaa; 4];
        assert_eq!(staging.read(&mut reader, &mut output), Ok(0));
        assert!(staging.is_eof());
        let consumed_at_eof = reader.consumed();
        assert_eq!(staging.read(&mut reader, &mut output), Ok(0));
        assert_eq!(reader.consumed(), consumed_at_eof);
    }

    // Catches staging more than one frame or losing the leftover across reads.
    #[test]
    fn leftover_bytes_survive_across_reads() {
        let stream = stdin_frame(b"abcdef");
        let mut reader = SliceReader::new(&stream);
        let mut staging = StdinStaging::new();

        let mut first = [0u8; 2];
        let mut second = [0u8; 4];
        assert_eq!(staging.read(&mut reader, &mut first), Ok(2));
        assert_eq!(first, *b"ab");
        assert_eq!(staging.read(&mut reader, &mut second), Ok(4));
        assert_eq!(second, *b"cdef");
        assert_eq!(reader.consumed(), stream.len());
    }

    // Catches accepting a non-Stdin frame or an oversized Stdin payload.
    #[test]
    fn rejects_non_stdin_frames_and_oversized_payloads() {
        let stdout_header = FrameHeader {
            kind: FrameKind::Stdout,
            payload_len: 1,
        }
        .encode();
        let mut wrong_kind = Vec::from(stdout_header);
        wrong_kind.push(b'x');
        let mut reader = SliceReader::new(&wrong_kind);
        let mut staging = StdinStaging::new();
        let mut output = [0u8; 4];
        assert_eq!(
            staging.read(&mut reader, &mut output),
            Err(StdinError::UnexpectedKind(FrameKind::Stdout))
        );

        let oversized = FrameHeader {
            kind: FrameKind::Stdin,
            payload_len: MAX_READ_LEN as u32 + 1,
        }
        .encode();
        let mut reader = SliceReader::new(&oversized);
        let mut staging = StdinStaging::new();
        assert_eq!(
            staging.read(&mut reader, &mut output),
            Err(StdinError::Decode(ControlError::StdinFrameTooLarge))
        );
    }
}
