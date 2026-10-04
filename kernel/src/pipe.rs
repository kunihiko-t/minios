//! `pipe` syscallが生成するkernel所有のbyte channel。
//!
//! `Pipe`はboundedなring bufferであり、両端はprocessのfd tableが
//! `FdEntry::Pipe`として指す。`PipeTable`はslot配列を保持するだけで
//! 参照計数を持たない。あるpipe idのliveな端数は`ProcessTable`が
//! 全processのfd tableを走査して派生する。端を持つprocessがcloseや
//! exitでfdを手放すとscan結果が減り、live端が0になったslotは次の
//! `ProcessTable::pipe_alloc`で再利用される。この方式では
//! increment/decrementの取りこぼしが構造的に起きない。

/// 1本のpipeがkernel内に保持するbyte数の上限。
pub const PIPE_CAPACITY: usize = 256;
/// 同時にopenできるpipeの本数。使い回しが効くためslotは再利用される。
pub const MAX_PIPES: usize = 4;

/// pipe 1本分のbounded ring buffer。`head`は次に読む位置、`len`は
/// 有効byte数。書き込み位置は`head + len`から都度求め、wrapは
/// `PIPE_CAPACITY`のmod演算で処理する。
#[derive(Debug)]
pub struct Pipe {
    head: usize,
    len: usize,
    data: [u8; PIPE_CAPACITY],
}

impl Pipe {
    const fn new() -> Self {
        Self {
            head: 0,
            len: 0,
            data: [0; PIPE_CAPACITY],
        }
    }

    /// バッファ内の有効byte数。
    pub const fn len(&self) -> usize {
        self.len
    }

    /// バッファが空なら`true`。
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 書き込める残りbyte数。
    pub const fn free(&self) -> usize {
        PIPE_CAPACITY - self.len
    }

    /// `bytes`を`free()`を上限に書き込み、書いたbyte数を返す。
    /// 呼び出し側が`free() == 0`の扱い（block）を先に決めるため、
    /// ここではpartial writeを許す。
    pub fn write(&mut self, bytes: &[u8]) -> usize {
        let count = self.free().min(bytes.len());
        let tail = (self.head + self.len) % PIPE_CAPACITY;
        let first = count.min(PIPE_CAPACITY - tail);
        self.data[tail..tail + first].copy_from_slice(&bytes[..first]);
        if count > first {
            self.data[..count - first].copy_from_slice(&bytes[first..count]);
        }
        self.len += count;
        count
    }

    /// バッファの先頭から`out`へ`min(len, out.len())` byte読み出し、
    /// 読んだbyte数を返す。
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let count = self.len.min(out.len());
        let first = count.min(PIPE_CAPACITY - self.head);
        out[..first].copy_from_slice(&self.data[self.head..self.head + first]);
        if count > first {
            out[first..count].copy_from_slice(&self.data[..count - first]);
        }
        self.head = (self.head + count) % PIPE_CAPACITY;
        self.len -= count;
        count
    }
}

/// `ProcessTable`が所有するpipeの格納域。`claim`/`get_mut`/`is_vacant`
/// のみを提供し、pipe idの生死判定（fd走査）は呼び出し側が行う。
pub struct PipeTable {
    slots: [Option<Pipe>; MAX_PIPES],
}

impl PipeTable {
    pub const fn new() -> Self {
        Self {
            slots: [None, None, None, None],
        }
    }

    /// `id`のpipeへのアクセス。端を持つfdが存在する限り`Some`を返す
    /// 不変条件は`ProcessTable`側が維持する。
    pub fn get_mut(&mut self, id: usize) -> Option<&mut Pipe> {
        self.slots.get_mut(id)?.as_mut()
    }

    /// `id`のslotが未使用なら`true`。死んだpipe（live端0）のslotも
    /// ここでは`false`のままで、再利用判定は`ProcessTable`が行う。
    pub const fn is_vacant(&self, id: usize) -> bool {
        id < MAX_PIPES && self.slots[id].is_none()
    }

    /// `id`のslotへ新しいpipeを置く。使用中のslotへ呼んではならない。
    pub fn claim(&mut self, id: usize) {
        self.slots[id] = Some(Pipe::new());
    }
}

impl Default for PipeTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_write_then_read_preserves_order() {
        let mut pipe = Pipe::new();
        assert_eq!(pipe.write(b"hello"), 5);
        assert_eq!(pipe.len(), 5);
        let mut out = [0u8; 8];
        assert_eq!(pipe.read(&mut out), 5);
        assert_eq!(&out[..5], b"hello");
        assert_eq!(pipe.len(), 0);
    }

    #[test]
    fn pipe_read_and_write_are_partial_at_bounds() {
        let mut pipe = Pipe::new();
        assert_eq!(pipe.read(&mut [0u8; 4]), 0);
        // 満杯までは書けるが溢れ分は残る。
        let big = [0xABu8; PIPE_CAPACITY + 10];
        assert_eq!(pipe.write(&big), PIPE_CAPACITY);
        assert_eq!(pipe.free(), 0);
        assert_eq!(pipe.write(&big), 0);
        // 読み出しはout.len()まで。
        let mut out = [0u8; 10];
        assert_eq!(pipe.read(&mut out), 10);
        assert!(out.iter().all(|&b| b == 0xAB));
        assert_eq!(pipe.len(), PIPE_CAPACITY - 10);
    }

    #[test]
    fn pipe_wraps_around_the_ring() {
        let mut pipe = Pipe::new();
        // headを後半へ進めてからwrapするwrite/readを行う。
        assert_eq!(pipe.write(&[1u8; PIPE_CAPACITY - 8]), PIPE_CAPACITY - 8);
        let mut drain = [0u8; PIPE_CAPACITY];
        assert_eq!(
            pipe.read(&mut drain[..PIPE_CAPACITY - 16]),
            PIPE_CAPACITY - 16
        );
        // head = CAPACITY-16、len = 8。ここへ64 byte書くとwrapする。
        let payload: alloc::vec::Vec<u8> = (0..64u8).collect();
        assert_eq!(pipe.write(&payload), 64);
        let mut out = [0u8; 80];
        let n = pipe.read(&mut out);
        assert_eq!(n, 72);
        assert_eq!(&out[..8], &[1u8; 8]);
        assert_eq!(&out[8..72], &payload[..]);
    }

    #[test]
    fn pipe_table_claims_and_vacates_slots() {
        let mut table = PipeTable::new();
        assert!(table.is_vacant(0));
        table.claim(0);
        assert!(!table.is_vacant(0));
        assert!(table.get_mut(0).is_some());
        assert!(table.get_mut(1).is_none());
        assert!(!table.is_vacant(MAX_PIPES));
    }
}
