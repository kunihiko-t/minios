//! host→guestのstdin byte stream受信。pull型でframeを順に消費する。
//!
//! guestの`read`はこのstagingから取り出す。stagingが空でEOF未達のときだけ
//! UARTからframeを引く。長さ0のStdin frameがEOFであり、一度EOFに達したら
//! readerへ二度と触れない。frame長上限（4 KiB）はABIのdecoderが強制する
//! ため、ここでは`FrameHeader::decode`の判定をそのまま使う。
//!
//! frameの受信は再開可能なstate machineとして保持する。`feed`は供給元から
//! 読めるbyteだけを流し込み、途中でbyteが尽きても蓄積済みの断片を残すため、
//! 到着を待つ間にprocessを`Blocked`へ回せる。`pull_frame`は完全なframeが
//! stagingされるまで`feed`を繰り返し、供給元がnon-blockingで尽きた場合は
//! `WouldBlock`を返す。

use minios_abi::{
    control::{FRAME_HEADER_LEN, FrameHeader, FrameKind},
    syscall::MAX_READ_LEN,
};

/// stdinのbyte供給元。UARTもhost testのsliceもこの形で読む。
pub trait ByteReader {
    fn read_byte(&mut self) -> u8;

    /// 受信待ちで停まらずに次のbyteを返す。`None`は「現時点でbyteがない」
    /// であり、streamの終端ではない。blockingしかできない供給元は
    /// default実装（常に`Some`）のまま使える。
    fn try_read_byte(&mut self) -> Option<u8> {
        Some(self.read_byte())
    }
}

/// 未配達のStdin frame断片。payloadは1 frame分（最大4 KiB）だけ保持する。
/// `header_len < FRAME_HEADER_LEN`の間はheader蓄積中、それ以降は
/// `have < want`の間payload蓄積中であり、どちらも`feed`で再開できる。
pub struct StdinStaging {
    bytes: [u8; MAX_READ_LEN],
    start: usize,
    end: usize,
    eof: bool,
    header: [u8; FRAME_HEADER_LEN],
    header_len: usize,
    want: usize,
    have: usize,
    error: Option<StdinError>,
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
    /// frame途中で供給元のbyteが尽きた。蓄積済みのstateは保持されるため、
    /// byte到着後の再試行でdecodeは続きから進む。
    WouldBlock,
}

impl StdinStaging {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_READ_LEN],
            start: 0,
            end: 0,
            eof: false,
            header: [0; FRAME_HEADER_LEN],
            header_len: 0,
            want: 0,
            have: 0,
            error: None,
        }
    }

    pub const fn is_eof(&self) -> bool {
        self.eof
    }

    /// 未配達のbyteがstaging内に残っているか。残っていれば`read`は
    /// readerへ触れず即座に返せる。
    pub const fn has_pending(&self) -> bool {
        self.start != self.end
    }

    /// 供給元から読めるbyteをframe state machineへ流し込む。
    /// 完全なframeがstagingされるかEOFに達したらその時点で打ち切るため、
    /// 1回の呼び出しが消費するのは高々1 frame分である。
    /// 戻り値は1 byteでも消費したかどうかであり、`false`は供給元の枯渇を
    /// 意味する。配達待ちのframeがあるかEOF到達済みならreaderへ触れない。
    pub fn feed<R: ByteReader>(&mut self, reader: &mut R) -> Result<bool, StdinError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let mut progressed = false;
        while !self.has_pending() && !self.eof {
            let Some(byte) = reader.try_read_byte() else {
                break;
            };
            progressed = true;
            if let Err(error) = self.push(byte) {
                // decode失敗後のbyte列はもう信用できないため、errorを終端
                // 状態として保持し、以降のreadでも同じerrorを返し続ける。
                self.error = Some(error);
                return Err(error);
            }
        }
        Ok(progressed)
    }

    /// 未配達byteを`output`へ移す。空でEOF未達なら1 frame読む。
    ///
    /// 戻り値は移したbyte数であり、0はEOFを意味する。`output`が空のときは
    /// readerへ触れず0を返す。non-blockingな供給元でframeが途中までしか
    /// 届いていないときは`WouldBlock`を返し、stateは次回へ引き継がれる。
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
        while !self.has_pending() && !self.eof {
            if !self.feed(reader)? {
                return Err(StdinError::WouldBlock);
            }
        }
        Ok(())
    }

    fn push(&mut self, byte: u8) -> Result<(), StdinError> {
        if self.header_len < FRAME_HEADER_LEN {
            self.header[self.header_len] = byte;
            self.header_len += 1;
            if self.header_len < FRAME_HEADER_LEN {
                return Ok(());
            }
            let header = FrameHeader::decode(&self.header).map_err(StdinError::Decode)?;
            if header.kind != FrameKind::Stdin {
                return Err(StdinError::UnexpectedKind(header.kind));
            }
            self.want = header.payload_len as usize;
            if self.want == 0 {
                self.eof = true;
            }
            return Ok(());
        }
        // decodeがMAX_READ_LEN以下を保証するため、範囲外書き込みは起きない。
        self.bytes[self.have] = byte;
        self.have += 1;
        if self.have == self.want {
            self.start = 0;
            self.end = self.want;
            self.header_len = 0;
            self.want = 0;
            self.have = 0;
        }
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

        fn try_read_byte(&mut self) -> Option<u8> {
            self.bytes.get(self.position).copied().inspect(|_| {
                self.position += 1;
            })
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

    // Catches losing partially received frame state across WouldBlock retries:
    // a read that stalls mid-frame must report WouldBlock without consuming
    // the decoder position, and a later read must resume where it stopped.
    #[test]
    fn would_block_preserves_partial_frame_state() {
        let stream = stdin_frame(b"abcdef");
        let split = stream.len() - 2;
        let mut head = SliceReader::new(&stream[..split]);
        let mut tail = SliceReader::new(&stream[split..]);
        let mut staging = StdinStaging::new();
        let mut output = [0u8; 8];

        assert_eq!(
            staging.read(&mut head, &mut output),
            Err(StdinError::WouldBlock)
        );
        assert_eq!(head.consumed(), split);

        // 別のreaderを差し替えてもdecoder stateはstaging側で生きている。
        assert_eq!(staging.read(&mut tail, &mut output), Ok(6));
        assert_eq!(&output[..6], b"abcdef");
    }

    // Catches WouldBlock leaving stale delivery bytes or treating a drained
    // header prefix as an error: every read before the last byte must report
    // WouldBlock, and the frame must then complete exactly once.
    #[test]
    fn drip_feed_reports_would_block_until_frame_completes() {
        let stream = stdin_frame(b"xy");
        let mut staging = StdinStaging::new();
        let mut output = [0u8; 4];

        // stagingを毎回新品にして、このsplitまでのprefixだけでreadする。
        for split in 0..stream.len() {
            let mut reader = SliceReader::new(&stream[..split]);
            let mut fresh = StdinStaging::new();
            assert_eq!(
                fresh.read(&mut reader, &mut output),
                Err(StdinError::WouldBlock)
            );
        }

        let mut reader = SliceReader::new(&stream);
        assert_eq!(staging.read(&mut reader, &mut output), Ok(2));
        assert_eq!(&output[..2], b"xy");
    }

    // Catches a decode error being forgotten on retry: the stored error must
    // come back for every subsequent read without touching the reader again.
    #[test]
    fn decode_errors_are_terminal() {
        let mut stream = Vec::from(b"BAD!");
        stream.resize(stream.len() + 8, 0);
        let mut reader = SliceReader::new(&stream);
        let mut staging = StdinStaging::new();
        let mut output = [0u8; 4];

        assert!(matches!(
            staging.read(&mut reader, &mut output),
            Err(StdinError::Decode(_))
        ));
        let consumed = reader.consumed();
        assert!(matches!(
            staging.read(&mut reader, &mut output),
            Err(StdinError::Decode(_))
        ));
        assert_eq!(reader.consumed(), consumed);
    }

    // Catches feed draining past one complete frame into the next header:
    // feed must stop as soon as a frame is staged, leaving the next frame's
    // bytes for a later call.
    #[test]
    fn feed_stops_at_frame_boundary() {
        let mut stream = stdin_frame(b"ab");
        stream.extend_from_slice(&stdin_frame(b"cd"));
        let mut reader = SliceReader::new(&stream);
        let mut staging = StdinStaging::new();

        assert_eq!(staging.feed(&mut reader), Ok(true));
        assert!(staging.has_pending());
        let first_frame_len = stdin_frame(b"ab").len();
        assert_eq!(reader.consumed(), first_frame_len);
    }
}
