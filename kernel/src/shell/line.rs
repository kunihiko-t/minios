#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineError {
    Full,
    NonPrintable,
}

pub struct LineBuffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> LineBuffer<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            overflowed: false,
        }
    }

    pub fn push(&mut self, byte: u8) -> Result<(), LineError> {
        // 制御文字とASCII以外の値を格納しないため、公開された安全な操作だけでは
        // `bytes[..len]`が正しいUTF-8であるという不変条件を壊せない。
        if !(b' '..=b'~').contains(&byte) {
            return Err(LineError::NonPrintable);
        }
        if self.overflowed || self.len == N {
            self.overflowed = true;
            return Err(LineError::Full);
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        Ok(())
    }

    pub fn backspace(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.bytes[self.len])
    }

    pub fn as_str(&self) -> &str {
        // `push`は印字可能なASCIIだけを格納し、`len`は格納済みの範囲だけを指す。
        // 未検査の変換を使わないため、将来この不変条件が崩れた場合も安全に失敗する。
        core::str::from_utf8(&self.bytes[..self.len])
            .expect("LineBuffer must contain printable ASCII")
    }

    pub fn finish(&self) -> Result<&str, LineError> {
        if self.overflowed {
            Err(LineError::Full)
        } else {
            Ok(self.as_str())
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.overflowed = false;
    }
}

impl<const N: usize> Default for LineBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{LineBuffer, LineError};

    #[test]
    fn removing_the_last_byte_restores_the_previous_text() {
        let mut line = LineBuffer::<3>::new();
        assert_eq!(line.push(b'a'), Ok(()));
        assert_eq!(line.push(b'b'), Ok(()));
        assert_eq!(line.backspace(), Some(b'b'));
        assert_eq!(line.as_str(), "a");
    }

    #[test]
    fn pushing_past_capacity_marks_the_line_full() {
        let mut line = LineBuffer::<3>::new();
        line.push(b'a').unwrap();
        assert_eq!(line.push(b'c'), Ok(()));
        assert_eq!(line.push(b'd'), Ok(()));
        assert_eq!(line.push(b'e'), Err(LineError::Full));
    }

    #[test]
    fn non_printable_input_cannot_enter_the_ascii_line() {
        let mut line = LineBuffer::<3>::new();

        assert_eq!(line.push(0x80), Err(LineError::NonPrintable));
        assert_eq!(line.as_str(), "");
    }

    #[test]
    fn overflow_keeps_the_submitted_line_invalid_until_clear() {
        let mut line = LineBuffer::<1>::new();
        assert_eq!(line.push(b'a'), Ok(()));
        assert_eq!(line.push(b'b'), Err(LineError::Full));
        assert_eq!(line.finish(), Err(LineError::Full));

        line.clear();

        assert_eq!(line.finish(), Ok(""));
    }

    #[test]
    fn backspace_after_overflow_keeps_the_submitted_line_invalid() {
        let mut line = LineBuffer::<1>::new();
        assert_eq!(line.push(b'a'), Ok(()));
        assert_eq!(line.push(b'b'), Err(LineError::Full));

        assert_eq!(line.backspace(), Some(b'a'));

        assert_eq!(line.finish(), Err(LineError::Full));
    }
}
