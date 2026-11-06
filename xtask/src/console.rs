//! `cargo xtask shell`がhost端末とQEMUのUARTをつなぐための変換。
//!
//! boot payloadとして起動したguestは、UARTをcontrol frame（`MCF1`）の
//! 通り道として使う。guestの標準出力・標準エラーは`Stdout`/`Stderr` frame
//! として届き、guestの標準入力は`Stdin` frameで渡す（長さ0はEOF）。
//! `FrameStream`はUARTのbyte列をframe単位の`ConsoleEvent`へ分け、
//! `stdin_frames`は端末から読んだbyte列を`Stdin` frameへ包む。どちらも
//! 入出力を持たない純粋な変換なので、host testで確かめられる。

use minios_abi::control::{FRAME_HEADER_LEN, FrameHeader, FrameKind};
use minios_abi::syscall::MAX_READ_LEN;

/// UARTから取り出した1単位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleEvent {
    /// frameの外にあるbyte。Ready frameより前のfirmwareとkernelのboot logや、
    /// frameとして読めなかったbyte。
    Text(Vec<u8>),
    /// 1個のcontrol frameとそのpayload。
    Frame(FrameKind, Vec<u8>),
}

/// UARTのbyte列をframeへ分ける再開可能なdecoder。chunkの境界がframeの
/// 途中にあっても、残りが届くまでbyteを持ち越す。
#[derive(Debug, Default)]
pub struct FrameStream {
    pending: Vec<u8>,
    ready: bool,
}

/// Ready frameの先頭。これより前のbyteはframeとして解釈しない。
const READY_PREFIX: [u8; 5] = [b'M', b'C', b'F', b'1', FrameKind::Ready as u8];

impl FrameStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ready frameを受け取った後なら`true`。guestへ`Stdin`を送ってよい。
    pub const fn is_ready(&self) -> bool {
        self.ready
    }

    /// `bytes`を取り込み、完成したeventを順に返す。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<ConsoleEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        if !self.ready {
            match find(&self.pending, &READY_PREFIX) {
                Some(start) => {
                    if start > 0 {
                        events.push(ConsoleEvent::Text(self.pending[..start].to_vec()));
                    }
                    self.pending.drain(..start);
                    self.ready = true;
                }
                None => {
                    // Readyの先頭がchunk境界で切れている場合に備えて末尾を残す。
                    let keep = READY_PREFIX.len() - 1;
                    let emit = self.pending.len().saturating_sub(keep);
                    if emit > 0 {
                        events.push(ConsoleEvent::Text(self.pending[..emit].to_vec()));
                        self.pending.drain(..emit);
                    }
                    return events;
                }
            }
        }
        while let Some(header_bytes) = self.pending.get(..FRAME_HEADER_LEN) {
            let header = match FrameHeader::decode(header_bytes) {
                Ok(header) => header,
                Err(_) => {
                    // frameとして読めないbyteは1 byteずつtextとして流し、
                    // 次のmagicで同期し直す。
                    let skip =
                        find(&self.pending[1..], b"MCF1").map_or(self.pending.len(), |at| at + 1);
                    events.push(ConsoleEvent::Text(self.pending[..skip].to_vec()));
                    self.pending.drain(..skip);
                    continue;
                }
            };
            let end = FRAME_HEADER_LEN + header.payload_len as usize;
            if self.pending.len() < end {
                break;
            }
            let payload = self.pending[FRAME_HEADER_LEN..end].to_vec();
            self.pending.drain(..end);
            events.push(ConsoleEvent::Frame(header.kind, payload));
        }
        events
    }
}

/// `input`を`Stdin` frameへ包む。1 frameはkernelのstaging上限
/// （`MAX_READ_LEN`）以下に分ける。空の`input`は長さ0のEOF frame 1個になる。
pub fn stdin_frames(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut push = |chunk: &[u8]| {
        let header = FrameHeader {
            kind: FrameKind::Stdin,
            payload_len: chunk.len() as u32,
        };
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(chunk);
    };
    if input.is_empty() {
        push(&[]);
    }
    for chunk in input.chunks(MAX_READ_LEN) {
        push(chunk);
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: FrameKind, payload: &[u8]) -> Vec<u8> {
        let mut bytes = FrameHeader {
            kind,
            payload_len: payload.len() as u32,
        }
        .encode()
        .to_vec();
        bytes.extend_from_slice(payload);
        bytes
    }

    fn ready() -> Vec<u8> {
        frame(FrameKind::Ready, &[1, 0, 2, 0])
    }

    // Catches the decoder dropping boot output or treating it as frames:
    // everything before Ready is text, and frames after it come out whole.
    #[test]
    fn boot_text_precedes_frames_and_frames_decode_whole() {
        let mut stream = FrameStream::new();
        let mut input = b"OpenSBI v1.3\r\nMCF garbage\r\n".to_vec();
        input.extend(ready());
        input.extend(frame(FrameKind::Stdout, b"$ "));
        let events = stream.push(&input);
        assert_eq!(
            events,
            [
                ConsoleEvent::Text(b"OpenSBI v1.3\r\nMCF garbage\r\n".to_vec()),
                ConsoleEvent::Frame(FrameKind::Ready, vec![1, 0, 2, 0]),
                ConsoleEvent::Frame(FrameKind::Stdout, b"$ ".to_vec()),
            ]
        );
        assert!(stream.is_ready());
    }

    // Catches chunk boundaries splitting a frame or the Ready prefix: a
    // byte-at-a-time feed must yield the same events as one big push.
    #[test]
    fn byte_at_a_time_feed_matches_a_single_push() {
        let mut input = b"boot\n".to_vec();
        input.extend(ready());
        input.extend(frame(FrameKind::Stderr, b"sh: nosuch: not found\n"));
        input.extend(frame(FrameKind::Exit, &[0, 0, 0, 0]));

        let mut whole = FrameStream::new();
        let expected = whole.push(&input);
        let mut split = FrameStream::new();
        let mut events = Vec::new();
        for byte in &input {
            events.extend(split.push(core::slice::from_ref(byte)));
        }
        // byte単位では起動textが細切れになるため、textを連結して比べる。
        let joined = |events: &[ConsoleEvent]| {
            let mut text = Vec::new();
            let mut frames = Vec::new();
            for event in events {
                match event {
                    ConsoleEvent::Text(bytes) => text.extend_from_slice(bytes),
                    ConsoleEvent::Frame(kind, payload) => frames.push((*kind, payload.clone())),
                }
            }
            (text, frames)
        };
        assert_eq!(joined(&events), joined(&expected));
    }

    // Catches undecodable bytes after Ready wedging the stream: they must
    // surface as text and decoding must resume at the next frame.
    #[test]
    fn garbage_after_ready_resynchronizes_on_the_next_magic() {
        let mut stream = FrameStream::new();
        stream.push(&ready());
        let mut input = b"\r\npanic text\r\n".to_vec();
        input.extend(frame(FrameKind::Stdout, b"ok\n"));
        let events = stream.push(&input);
        assert_eq!(
            events,
            [
                ConsoleEvent::Text(b"\r\npanic text\r\n".to_vec()),
                ConsoleEvent::Frame(FrameKind::Stdout, b"ok\n".to_vec()),
            ]
        );
    }

    // Catches stdin framing that the kernel would reject: input is split
    // at MAX_READ_LEN, and empty input becomes the zero-length EOF frame.
    #[test]
    fn stdin_frames_split_at_the_staging_limit_and_encode_eof() {
        assert_eq!(stdin_frames(b""), frame(FrameKind::Stdin, b""));
        assert_eq!(stdin_frames(b"ls\n"), frame(FrameKind::Stdin, b"ls\n"));
        let long = vec![b'x'; MAX_READ_LEN + 1];
        let mut expected = frame(FrameKind::Stdin, &long[..MAX_READ_LEN]);
        expected.extend(frame(FrameKind::Stdin, b"x"));
        assert_eq!(stdin_frames(&long), expected);
    }
}
