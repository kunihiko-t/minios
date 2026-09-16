//! `write`、`read`、`exit`、未知のsystem callを純粋なdispatch結果へ変換する。

use crate::{
    user::{
        context::UserContext,
        memory::{check_user_writable_range, copy_from_user},
    },
    vm::{AddressSpace, FrameStore},
};
use minios_abi::{
    control::FrameKind,
    syscall::{
        EBADF, EFAULT, EINVAL, ENOSYS, FIRST_FILE_FD, MAX_OPEN_FILES, MAX_PATH_LEN, MAX_READ_LEN,
        MAX_WRITE_LEN, STDERR, STDIN, STDOUT, SyscallNumber,
    },
};

/// syscall結果の受け先。kernelはUARTへframeを載せ、host testは記録する。
pub trait ControlSink {
    type Error;

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error>;
}

/// stdin byteの供給元。kernelはUARTのStdin frameから引き、host testは用意した列を返す。
pub trait ControlSource {
    type Error;

    /// 次の入力を`output`へ移す。`Ok(Some(n))`は受信byte数であり、0はEOF。
    /// `Ok(None)`は現時点で入力がないことを意味し、呼び出し側はprocessを
    /// `Blocked`へ回して後でやり直す。この契約によりsourceは受信待ちで
    /// 停まってはならない。
    fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error>;

    /// `path`のfile内容を`output`の先頭へ最大`output.len()` byte書き、
    /// 書いたbyte数を返す。戻り値の`Err`はそのまま`a0`へ返すerrnoである。
    /// default実装は`ENOSYS`であり、storageを持たないsourceは実装不要。
    fn read_file(&mut self, path: &str, output: &mut [u8]) -> Result<usize, isize> {
        let _ = (path, output);
        Err(ENOSYS)
    }

    /// `path`のfileを開き、割り当てたfd（`FIRST_FILE_FD`以上）を返す。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn open_file(&mut self, path: &str) -> Result<usize, isize> {
        let _ = path;
        Err(ENOSYS)
    }

    /// `fd`のfileから`output`へ最大`output.len()` byte読み、読んだbyte数
    /// （EOFは0）を返す。成功時はfdのoffsetを進める。default実装は`ENOSYS`。
    fn read_fd(&mut self, fd: usize, output: &mut [u8]) -> Result<usize, isize> {
        let _ = (fd, output);
        Err(ENOSYS)
    }

    /// `fd`のfileを閉じる。`Err`はそのまま`a0`へ返すerrnoである。
    /// default実装は`ENOSYS`。
    fn close_fd(&mut self, fd: usize) -> Result<(), isize> {
        let _ = fd;
        Err(ENOSYS)
    }

    /// `path`のfileを作成またはwritableに開き、割り当てたfdを返す。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn create_file(&mut self, path: &str) -> Result<usize, isize> {
        let _ = path;
        Err(ENOSYS)
    }

    /// `fd`のfileへ`data`を現在offsetから書き、書いたbyte数を返す。
    /// read-onlyのfdは`EBADF`で拒否すること。default実装は`ENOSYS`。
    fn write_fd(&mut self, fd: usize, data: &[u8]) -> Result<usize, isize> {
        let _ = (fd, data);
        Err(ENOSYS)
    }

    /// `path`のfileを削除し、そのentryを指す全processのfdを失効させる。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn unlink(&mut self, path: &str) -> Result<(), isize> {
        let _ = path;
        Err(ENOSYS)
    }
}

/// 1個のsystem callを処理した後の継続種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallFlow<E, SE = E> {
    /// guest実行へ戻る。戻り値は`context`の`a0`へ書き込み済みである。
    Resume,
    /// guestが`exit`を要求した。codeは`a0`の下位32bitである。
    Exit(u32),
    /// 継続できない送信失敗。sinkのerrorをそのまま保持する。
    Fatal(E),
    /// 継続できない受信失敗。sourceのerrorをそのまま保持する。
    SourceFatal(SE),
    /// `read`の受信が済み、userへのcopy待ちである。受信byteは呼び出し側の
    /// scratch bufferにあり、handlerが先頭`len` byteを`start`へ移してから
    /// `a0`へ`len`を書いて戻る。4 KiBを値で返さないのは、trap stackの
    /// 多重frameでoverflowさせないためである。
    ReadComplete { start: u64, len: usize },
    /// `read`の検証は通ったが入力が未到着である。`sepc`はecallへ戻してあり、
    /// 入力到着後の再開で同じsyscallがやり直される。schedulerはこのprocessを
    /// stdin待ちへ回し、他のrunnable processを動かせる。
    Blocked,
}

/// `a7`のsystem call番号に従って`context`を処理する。
///
/// guest pointerをRust参照として解することなく、`write`は1回の検証付きcopyと
/// 1回の`sink.frame`で処理し、戻り値を`a0`へ書き込む。descriptorは1と2だけを
/// 許可し、4,096 byteを超える長さは拒否する。`read`はdescriptor 0だけを許可し、
/// 書き込み検証を通してから1回の`source.read_stdin`で`read_scratch`へ受信する。
/// sourceが`Ok(None)`を返す未到着では`Blocked`を返し、`sepc`をecallへ戻して
/// 再開時の再実行に委ねる。
/// userへのcopyは呼び出し側の`ReadComplete`処理へ委ねる。scratchは呼び出し側が
/// 1個だけ持ち、多重frameへ4 KiBを複製しない。
pub fn dispatch_syscall<M: FrameStore, S: ControlSink, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    sink: &mut S,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<S::Error, R::Error> {
    let number = context.register(17);
    if number == SyscallNumber::Write as usize {
        dispatch_write(context, space, memory, sink, source)
    } else if number == SyscallNumber::Read as usize {
        dispatch_read(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::ReadFile as usize {
        dispatch_read_file(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Open as usize {
        dispatch_open(context, space, memory, source, false)
    } else if number == SyscallNumber::Create as usize {
        dispatch_open(context, space, memory, source, true)
    } else if number == SyscallNumber::Unlink as usize {
        dispatch_unlink(context, space, memory, source)
    } else if number == SyscallNumber::Close as usize {
        dispatch_close(context, source)
    } else if number == SyscallNumber::Exit as usize {
        SyscallFlow::Exit(context.register(10) as u32)
    } else {
        context.set_register(10, ENOSYS as usize);
        SyscallFlow::Resume
    }
}

fn dispatch_read<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    let is_file_fd = (FIRST_FILE_FD..FIRST_FILE_FD + MAX_OPEN_FILES).contains(&fd);
    if fd != STDIN && !is_file_fd {
        context.set_register(10, EBADF as usize);
        return SyscallFlow::Resume;
    }
    let len = context.register(12);
    if len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }
    let start = context.register(11) as u64;
    // stdinの消費は不可逆なため、sourceへ触れる前にEFAULTを確定させる。
    if check_user_writable_range(space, memory, start, len).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    if fd == STDIN {
        // `Ok(None)`は入力未到着である。`sepc`はclassifyがecallの次へ進めて
        // あるため、4 byte戻して再開時に同じecallをやり直す。
        return match source.read_stdin(&mut read_scratch[..len]) {
            Ok(Some(count)) => SyscallFlow::ReadComplete { start, len: count },
            Ok(None) => {
                context.set_sepc(context.sepc() - 4);
                SyscallFlow::Blocked
            }
            Err(error) => SyscallFlow::SourceFatal(error),
        };
    }
    match source.read_fd(fd, &mut read_scratch[..len]) {
        Ok(count) => SyscallFlow::ReadComplete {
            start,
            len: count.min(len),
        },
        Err(errno) => {
            context.set_register(10, errno as usize);
            SyscallFlow::Resume
        }
    }
}

/// `a0/a1`が指すuser memoryのpathを`path_buf`へ検証付きcopyし、長さを
/// 返す。長さの範囲違反は`EINVAL`、user range外は`EFAULT`を`a0`へ書いて
/// `None`を返す。fd割り当てやdir entry変更のside effectより先に
/// EFAULT/EINVALを確定する規約をopen/create/unlinkで共有する。
fn copy_user_path<M: FrameStore>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    path_buf: &mut [u8; MAX_PATH_LEN],
) -> Option<usize> {
    let path_len = context.register(11);
    if path_len == 0 || path_len > MAX_PATH_LEN {
        context.set_register(10, EINVAL as usize);
        return None;
    }
    if copy_from_user(
        space,
        memory,
        context.register(10) as u64,
        &mut path_buf[..path_len],
    )
    .is_err()
    {
        context.set_register(10, EFAULT as usize);
        return None;
    }
    Some(path_len)
}

/// `open`と`create` (`a0=path_ptr, a1=path_len`)。検証規約は`read_file`と
/// 同じで、fd割り当てやdir entry作成のside effectより先にpathの
/// EFAULT/EINVALを確定する。`create`はfileをwritableに開く。
/// 成功時は`a0`へfdを返す。
fn dispatch_open<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    create: bool,
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    let result = if create {
        source.create_file(path)
    } else {
        source.open_file(path)
    };
    match result {
        Ok(fd) => context.set_register(10, fd),
        Err(errno) => context.set_register(10, errno as usize),
    }
    SyscallFlow::Resume
}

/// `unlink` (`a0=path_ptr, a1=path_len`)。検証規約は`open`と同じ。
/// 成功時は`a0`へ0を返す。
fn dispatch_unlink<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    match source.unlink(path) {
        Ok(()) => context.set_register(10, 0),
        Err(errno) => context.set_register(10, errno as usize),
    }
    SyscallFlow::Resume
}

/// `close` (`a0=fd`)。標準streamのfdは`EBADF`として拒否し、file fdは
/// sourceへ委譲する。成功時は`a0`へ0を返す。
fn dispatch_close<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    if !(FIRST_FILE_FD..FIRST_FILE_FD + MAX_OPEN_FILES).contains(&fd) {
        context.set_register(10, EBADF as usize);
        return SyscallFlow::Resume;
    }
    match source.close_fd(fd) {
        Ok(()) => context.set_register(10, 0),
        Err(errno) => context.set_register(10, errno as usize),
    }
    SyscallFlow::Resume
}

/// `ReadComplete`の受信済みbyteを検証済みuser rangeへ移し、`a0`へ長さを書く。
///
/// # Safety
///
/// 呼び出し側は`dispatch_read`へ渡したscratchの先頭`len` byteと、返却された
/// `start`と`len`をそのまま渡さなければならない。user satpが有効でguestが
/// 停止中のtrap handlerからのみ呼び、copyの間`sstatus.SUM`を立てておくこと。
/// 検証時からpage tableは不変であり、このcopyは正確に届く。
pub unsafe fn complete_read(context: &mut UserContext, start: u64, len: usize, data: &[u8]) {
    // Safety: 呼び出し側の契約により、検証済みuser rangeへのcopyである。
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), start as *mut u8, len);
    }
    context.set_register(10, len);
}

/// `read_file` (`a0=path_ptr, a1=path_len, a2=buf_ptr, a3=buf_len`)。
/// mountでframeを確保し得るside effectの前に、path copyとbuf検証を
/// 済ませて`EFAULT`を確定する（`dispatch_read`と同じ規約）。成功時は
/// `ReadComplete`経由でscratchの先頭`n` byteがbufへcopyされる。
fn dispatch_read_file<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let path_len = context.register(11);
    let buf_len = context.register(13);
    if path_len == 0 || path_len > MAX_PATH_LEN || buf_len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if buf_len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }

    let mut path_buf = [0u8; MAX_PATH_LEN];
    if copy_from_user(
        space,
        memory,
        context.register(10) as u64,
        &mut path_buf[..path_len],
    )
    .is_err()
    {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    let buf_start = context.register(12) as u64;
    if check_user_writable_range(space, memory, buf_start, buf_len).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };

    match source.read_file(path, &mut read_scratch[..buf_len]) {
        // sourceの契約は`output.len()`以下のcountだが、逸脱しても
        // scratchの範囲外をReadCompleteへ渡さないよう打ち切る。
        Ok(count) => SyscallFlow::ReadComplete {
            start: buf_start,
            len: count.min(buf_len),
        },
        Err(errno) => {
            context.set_register(10, errno as usize);
            SyscallFlow::Resume
        }
    }
}

/// `write` (`a0=fd, a1=ptr, a2=len`)。fd 1/2はframe sinkへ、file fd
/// （3以上）は`source.write_fd`へ委譲する。どちらの経路でも、副作用の
/// 前にuser buffer全体の`EFAULT`を確定する規約は同じである。
fn dispatch_write<M: FrameStore, S: ControlSink, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    sink: &mut S,
    source: &mut R,
) -> SyscallFlow<S::Error, R::Error> {
    let fd = context.register(10);
    let is_file_fd = (FIRST_FILE_FD..FIRST_FILE_FD + MAX_OPEN_FILES).contains(&fd);
    let kind = match fd {
        STDOUT => Some(FrameKind::Stdout),
        STDERR => Some(FrameKind::Stderr),
        _ if is_file_fd => None,
        _ => {
            context.set_register(10, EBADF as usize);
            return SyscallFlow::Resume;
        }
    };
    let len = context.register(12);
    if len > MAX_WRITE_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }

    // kernel stackに置く一次bufferは最大4,096 byteとする規約である。
    // 検証付きcopyが拒絶した場合、kernel panicではなくEFAULTとして返す。
    let mut buffer = [0u8; MAX_WRITE_LEN];
    match copy_from_user(
        space,
        memory,
        context.register(11) as u64,
        &mut buffer[..len],
    ) {
        Ok(()) => {}
        Err(_) => {
            context.set_register(10, EFAULT as usize);
            return SyscallFlow::Resume;
        }
    }

    match kind {
        Some(kind) => match sink.frame(kind, &buffer[..len]) {
            Ok(()) => {
                context.set_register(10, len);
                SyscallFlow::Resume
            }
            Err(error) => SyscallFlow::Fatal(error),
        },
        None => match source.write_fd(fd, &buffer[..len]) {
            Ok(count) => {
                context.set_register(10, count.min(len));
                SyscallFlow::Resume
            }
            Err(errno) => {
                context.set_register(10, errno as usize);
                SyscallFlow::Resume
            }
        },
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

    use super::{ControlSink, ControlSource, SyscallFlow, dispatch_syscall};
    use crate::{
        memory::frame::{FrameAllocator, PAGE_SIZE},
        user::context::UserContext,
        vm::{AddressSpaceBuilder, FrameStore, PageFlags, VirtPage},
    };
    use minios_abi::{
        control::FrameKind,
        syscall::{
            EBADF, EFAULT, EINVAL, EISDIR, ENODEV, ENOENT, ENOSYS, ENOTDIR, FIRST_FILE_FD,
            MAX_OPEN_FILES, MAX_PATH_LEN, MAX_READ_LEN, MAX_WRITE_LEN, STDERR, STDIN, STDOUT,
            SyscallNumber,
        },
    };

    const MESSAGE: &[u8] = b"MK4";
    const MESSAGE_PAGE: usize = 0x0010_1000;
    const WRITE_NUMBER: usize = SyscallNumber::Write as usize;
    const READ_NUMBER: usize = SyscallNumber::Read as usize;
    const READ_FILE_NUMBER: usize = SyscallNumber::ReadFile as usize;
    const OPEN_NUMBER: usize = SyscallNumber::Open as usize;
    const CLOSE_NUMBER: usize = SyscallNumber::Close as usize;
    const CREATE_NUMBER: usize = SyscallNumber::Create as usize;
    const UNLINK_NUMBER: usize = SyscallNumber::Unlink as usize;
    const EXIT_NUMBER: usize = SyscallNumber::Exit as usize;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        RangeOutOfBounds,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; PAGE_SIZE]>>,
    }

    impl TestFrameStore {
        fn frame(&self, frame_start: usize) -> Result<&[u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get(&frame_start)
                .map(Box::as_ref)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn frame_mut(
            &mut self,
            frame_start: usize,
        ) -> Result<&mut [u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get_mut(&frame_start)
                .map(Box::as_mut)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn range(offset: usize, len: usize) -> Result<core::ops::Range<usize>, TestStoreError> {
            let end = offset
                .checked_add(len)
                .ok_or(TestStoreError::RangeOutOfBounds)?;
            if end > PAGE_SIZE {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            Ok(offset..end)
        }
    }

    impl FrameStore for TestFrameStore {
        type Error = TestStoreError;

        fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
            self.frames.insert(frame_start, Box::new([0; PAGE_SIZE]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            let offset = index * 8;
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&self.frame(frame_start)?[offset..offset + 8]);
            Ok(u64::from_le_bytes(bytes))
        }

        fn write_u64(
            &mut self,
            frame_start: usize,
            index: usize,
            value: u64,
        ) -> Result<(), Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            let offset = index * 8;
            self.frame_mut(frame_start)?[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }

        fn copy_into(
            &mut self,
            frame_start: usize,
            offset: usize,
            bytes: &[u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, bytes.len())?;
            self.frame_mut(frame_start)?[range].copy_from_slice(bytes);
            Ok(())
        }

        fn copy_out(
            &self,
            frame_start: usize,
            offset: usize,
            output: &mut [u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, output.len())?;
            output.copy_from_slice(&self.frame(frame_start)?[range]);
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SinkError {
        Injected,
    }

    #[derive(Default)]
    struct FakeSink {
        frames: Vec<(FrameKind, Vec<u8>)>,
        fail: bool,
    }

    impl ControlSink for FakeSink {
        type Error = SinkError;

        fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error> {
            if self.fail {
                return Err(SinkError::Injected);
            }
            self.frames.push((kind, Vec::from(payload)));
            Ok(())
        }
    }

    struct FakeSource {
        script: Vec<u8>,
        position: usize,
        fail: bool,
        reads: usize,
        ready: bool,
    }

    impl FakeSource {
        fn scripted(script: &[u8]) -> Self {
            Self {
                script: Vec::from(script),
                position: 0,
                fail: false,
                reads: 0,
                ready: true,
            }
        }

        fn not_ready() -> Self {
            Self {
                ready: false,
                ..Self::scripted(b"")
            }
        }
    }

    impl ControlSource for FakeSource {
        type Error = SinkError;

        fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            self.reads += 1;
            if self.fail {
                return Err(SinkError::Injected);
            }
            if !self.ready {
                return Ok(None);
            }
            let available = &self.script[self.position..];
            let count = core::cmp::min(available.len(), output.len());
            output[..count].copy_from_slice(&available[..count]);
            self.position += count;
            Ok(Some(count))
        }
    }

    fn syscall_context(number: usize, a0: usize, a1: usize, a2: usize) -> UserContext {
        let mut context = UserContext::patterned_for_test(0x0010_0500);
        context.set_register(17, number);
        context.set_register(10, a0);
        context.set_register(11, a1);
        context.set_register(12, a2);
        context
    }

    // MESSAGE page 1枚だけをmapした空間でdispatchを実行する。
    fn dispatch_fixture<R: ControlSource<Error = SinkError>>(
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        sink: &mut FakeSink,
        source: &mut R,
        read_scratch: &mut [u8; MAX_READ_LEN],
    ) -> (UserContext, SyscallFlow<SinkError>) {
        let mut allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut memory).unwrap();
        let page = builder
            .map_new_zeroed(
                VirtPage::from_start(MESSAGE_PAGE as u64).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        builder.copy_into(page, 0, MESSAGE).unwrap();
        let space = builder.finish();
        let mut context = syscall_context(number, a0, a1, a2);
        let flow = dispatch_syscall(&mut context, &space, &memory, sink, source, read_scratch);
        (context, flow)
    }

    /// `dispatch_fixture`と同じ空間を組み、既存のcontextを引き継いでdispatchする。
    /// `Blocked`からの再開 (同じecallの再実行) を再現するテストが使う。
    fn dispatch_fixture_at<R: ControlSource<Error = SinkError>>(
        mut context: UserContext,
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        sink: &mut FakeSink,
        source: &mut R,
        read_scratch: &mut [u8; MAX_READ_LEN],
    ) -> (UserContext, SyscallFlow<SinkError>) {
        let mut allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut memory).unwrap();
        let page = builder
            .map_new_zeroed(
                VirtPage::from_start(MESSAGE_PAGE as u64).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        builder.copy_into(page, 0, MESSAGE).unwrap();
        let space = builder.finish();
        context.set_register(17, number);
        context.set_register(10, a0);
        context.set_register(11, a1);
        context.set_register(12, a2);
        let flow = dispatch_syscall(&mut context, &space, &memory, sink, source, read_scratch);
        (context, flow)
    }

    fn scratch() -> [u8; MAX_READ_LEN] {
        [0xaa; MAX_READ_LEN]
    }

    /// `read_file`の返答を組み込んだsource。stdin側は未使用で、呼ばれた
    /// pathを記録する。
    struct FileSource {
        script: Vec<u8>,
        result: Result<(), isize>,
        seen_path: Option<Vec<u8>>,
        reads: usize,
        open_fd: Result<usize, isize>,
        fd_reads: usize,
        closes: usize,
        creates: usize,
        writes: usize,
        written: Vec<u8>,
        unlinks: usize,
    }

    impl FileSource {
        fn serving(bytes: &[u8]) -> Self {
            Self {
                script: Vec::from(bytes),
                result: Ok(()),
                seen_path: None,
                reads: 0,
                open_fd: Ok(FIRST_FILE_FD),
                fd_reads: 0,
                closes: 0,
                creates: 0,
                writes: 0,
                written: Vec::new(),
                unlinks: 0,
            }
        }

        fn failing(errno: isize) -> Self {
            Self {
                result: Err(errno),
                ..Self::serving(b"")
            }
        }
    }

    impl ControlSource for FileSource {
        type Error = SinkError;

        fn read_stdin(&mut self, _output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            self.reads += 1;
            Ok(None)
        }

        fn read_file(&mut self, path: &str, output: &mut [u8]) -> Result<usize, isize> {
            self.reads += 1;
            self.seen_path = Some(Vec::from(path.as_bytes()));
            self.result?;
            let count = core::cmp::min(self.script.len(), output.len());
            output[..count].copy_from_slice(&self.script[..count]);
            Ok(count)
        }

        fn open_file(&mut self, path: &str) -> Result<usize, isize> {
            self.seen_path = Some(Vec::from(path.as_bytes()));
            self.open_fd
        }

        fn read_fd(&mut self, _fd: usize, output: &mut [u8]) -> Result<usize, isize> {
            self.fd_reads += 1;
            self.result?;
            let count = core::cmp::min(self.script.len(), output.len());
            output[..count].copy_from_slice(&self.script[..count]);
            Ok(count)
        }

        fn close_fd(&mut self, fd: usize) -> Result<(), isize> {
            self.closes += 1;
            if fd == FIRST_FILE_FD {
                Ok(())
            } else {
                Err(EBADF)
            }
        }

        fn create_file(&mut self, path: &str) -> Result<usize, isize> {
            self.creates += 1;
            self.seen_path = Some(Vec::from(path.as_bytes()));
            self.open_fd
        }

        fn write_fd(&mut self, _fd: usize, data: &[u8]) -> Result<usize, isize> {
            self.writes += 1;
            self.result?;
            self.written.extend_from_slice(data);
            Ok(data.len())
        }

        fn unlink(&mut self, path: &str) -> Result<(), isize> {
            self.unlinks += 1;
            self.seen_path = Some(Vec::from(path.as_bytes()));
            self.result
        }
    }

    /// `read_file`用のdispatch fixture。`page_content`をMESSAGE_PAGEへ書き、
    /// `a3`を含む4引数のcontextを組む。
    #[allow(clippy::too_many_arguments)]
    fn dispatch_file_fixture<R: ControlSource<Error = SinkError>>(
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        page_content: &[u8],
        sink: &mut FakeSink,
        source: &mut R,
        read_scratch: &mut [u8; MAX_READ_LEN],
    ) -> (UserContext, SyscallFlow<SinkError>) {
        dispatch_numbered_file_fixture(
            READ_FILE_NUMBER,
            a0,
            a1,
            a2,
            a3,
            page_content,
            sink,
            source,
            read_scratch,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_numbered_file_fixture<R: ControlSource<Error = SinkError>>(
        number: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        page_content: &[u8],
        sink: &mut FakeSink,
        source: &mut R,
        read_scratch: &mut [u8; MAX_READ_LEN],
    ) -> (UserContext, SyscallFlow<SinkError>) {
        let mut allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut builder = AddressSpaceBuilder::new(&mut allocator, &mut memory).unwrap();
        let page = builder
            .map_new_zeroed(
                VirtPage::from_start(MESSAGE_PAGE as u64).unwrap(),
                PageFlags::new(true, true, false, true).unwrap(),
            )
            .unwrap();
        builder.copy_into(page, 0, page_content).unwrap();
        let space = builder.finish();
        let mut context = syscall_context(number, a0, a1, a2);
        context.set_register(13, a3);
        let flow = dispatch_syscall(&mut context, &space, &memory, sink, source, read_scratch);
        (context, flow)
    }

    // Catches missing frames, duplicated frames, wrong frame kinds, wrong
    // payloads, or a return value other than the written byte count.
    #[test]
    fn write_stdout_and_stderr_deliver_one_frame_each_and_return_the_length() {
        for (descriptor, expected_kind) in
            [(STDOUT, FrameKind::Stdout), (STDERR, FrameKind::Stderr)]
        {
            let mut sink = FakeSink::default();
            let mut source = FakeSource::scripted(b"untouched");
            let (context, flow) = dispatch_fixture(
                WRITE_NUMBER,
                descriptor,
                MESSAGE_PAGE,
                MESSAGE.len(),
                &mut sink,
                &mut source,
                &mut scratch(),
            );

            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(sink.frames, vec![(expected_kind, Vec::from(MESSAGE))]);
            assert_eq!(context.register(10), MESSAGE.len());
            assert_eq!(context.register(17), WRITE_NUMBER);
            assert_eq!(context.register(11), MESSAGE_PAGE);
            assert_eq!(context.register(12), MESSAGE.len());
            assert_eq!(context.register(8), 0x5150_0000_0000_0008);
            assert_eq!(source.reads, 0);
        }
    }

    // Catches accepting a descriptor other than 1 or 2.
    #[test]
    fn write_reports_unknown_descriptors_with_ebadf() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            FIRST_FILE_FD + MAX_OPEN_FILES,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches copying more than one kernel-page worth of bytes.
    #[test]
    fn write_reports_oversized_lengths_with_einval() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            STDOUT,
            MESSAGE_PAGE,
            MAX_WRITE_LEN + 1,
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EINVAL as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches dereferencing an unmapped guest pointer or copying before the
    // page walk completes.
    #[test]
    fn write_reports_guest_faults_with_efault() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            STDOUT,
            0x0,
            4,
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches resuming a guest that called an unknown syscall without the
    // ENOSYS return value.
    #[test]
    fn unknown_numbers_report_enosys_and_resume() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"");
        let (context, flow) =
            dispatch_fixture(999, 0, 0, 0, &mut sink, &mut source, &mut scratch());

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOSYS as usize);
        assert!(sink.frames.is_empty());
    }

    // Catches losing the exit code or treating exit as a resumable call.
    #[test]
    fn exit_returns_the_code_without_touching_the_context() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"untouched");
        let (context, flow) = dispatch_fixture(
            EXIT_NUMBER,
            42,
            0,
            0,
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Exit(42));
        assert_eq!(context.register(10), 42);
        assert!(sink.frames.is_empty());
        assert_eq!(source.reads, 0);
    }

    // Catches resuming after a sink failure or clobbering the guest context.
    #[test]
    fn sink_failure_is_fatal_and_preserves_the_context() {
        let mut sink = FakeSink {
            fail: true,
            ..FakeSink::default()
        };
        let mut source = FakeSource::scripted(b"");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            STDOUT,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Fatal(SinkError::Injected));
        assert_eq!(context.register(10), STDOUT);
        assert!(sink.frames.is_empty());
    }

    fn read_completion(flow: SyscallFlow<SinkError>) -> (u64, usize) {
        match flow {
            SyscallFlow::ReadComplete { start, len } => (start, len),
            other => panic!("expected ReadComplete, actual {other:?}"),
        }
    }

    // Catches wiring mistakes between dispatch and the real Stdin frame parser.
    #[test]
    fn read_serves_real_stdin_frames_through_staging() {
        use crate::user::stdin::{ByteReader, StdinError, StdinStaging};

        struct VecReader<'a> {
            bytes: &'a [u8],
            position: usize,
        }

        impl ByteReader for VecReader<'_> {
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

        struct StagingSource<'a> {
            staging: StdinStaging,
            reader: VecReader<'a>,
        }

        impl ControlSource for StagingSource<'_> {
            type Error = SinkError;

            fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
                match self.staging.read(&mut self.reader, output) {
                    Ok(count) => Ok(Some(count)),
                    Err(StdinError::WouldBlock) => Ok(None),
                    Err(_) => Err(SinkError::Injected),
                }
            }
        }

        let header = |len: u32| {
            minios_abi::control::FrameHeader {
                kind: minios_abi::control::FrameKind::Stdin,
                payload_len: len,
            }
            .encode()
        };
        let mut stream = Vec::from(header(2));
        stream.extend_from_slice(b"ab");
        stream.extend_from_slice(&header(0));

        let mut sink = FakeSink::default();
        let mut source = StagingSource {
            staging: StdinStaging::new(),
            reader: VecReader {
                bytes: &stream,
                position: 0,
            },
        };
        let mut received = scratch();
        let (_, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            512,
            &mut sink,
            &mut source,
            &mut received,
        );
        let (_, len) = read_completion(flow);
        assert_eq!(len, 2);
        assert_eq!(&received[..len], b"ab");
    }

    // Catches losing received bytes, misreporting the destination, or setting
    // a0 before the handler copies the bytes to the guest.
    #[test]
    fn read_delivers_source_bytes_as_read_complete() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"hello");
        let mut received = scratch();
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            b"hello".len(),
            &mut sink,
            &mut source,
            &mut received,
        );

        let (start, len) = read_completion(flow);
        assert_eq!(start, MESSAGE_PAGE as u64);
        assert_eq!(len, b"hello".len());
        assert_eq!(&received[..len], b"hello");
        assert_eq!(received[len], 0xaa);
        assert_eq!(context.register(10), STDIN);
        assert_eq!(source.reads, 1);
        assert!(sink.frames.is_empty());
    }

    // Catches treating an exhausted source as an error instead of EOF.
    #[test]
    fn read_reports_eof_with_zero_length() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"");
        let mut received = scratch();
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut received,
        );

        let (start, len) = read_completion(flow);
        assert_eq!(start, MESSAGE_PAGE as u64);
        assert_eq!(len, 0);
        assert_eq!(received[0], 0xaa);
        assert_eq!(context.register(10), STDIN);
        assert_eq!(source.reads, 1);
    }

    // Catches accepting a descriptor other than 0 or consuming input on EBADF.
    #[test]
    fn read_reports_unknown_descriptors_with_ebadf() {
        // 1,2は標準streamの書き込み側、7以降はfd範囲外で`EBADF`。
        for descriptor in [1, 2, 7, 100] {
            let mut sink = FakeSink::default();
            let mut source = FakeSource::scripted(b"hello");
            let (context, flow) = dispatch_fixture(
                READ_NUMBER,
                descriptor,
                MESSAGE_PAGE,
                MESSAGE.len(),
                &mut sink,
                &mut source,
                &mut scratch(),
            );

            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(context.register(10), EBADF as usize);
            assert_eq!(source.reads, 0);
            assert!(sink.frames.is_empty());
        }
    }

    // Catches copying more than one kernel-page worth of bytes.
    #[test]
    fn read_reports_oversized_lengths_with_einval() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"hello");
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            MAX_READ_LEN + 1,
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EINVAL as usize);
        assert_eq!(source.reads, 0);
        assert!(sink.frames.is_empty());
    }

    // Catches consuming stdin before the destination range is validated.
    #[test]
    fn read_reports_guest_faults_with_efault_without_consuming() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"hello");
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            0x0,
            4,
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert_eq!(source.reads, 0);
        assert!(sink.frames.is_empty());
    }

    // Catches validating the pointer or touching the source for a zero read.
    #[test]
    fn read_with_zero_length_returns_zero_without_touching_the_source() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"hello");
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            0x0,
            0,
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), 0);
        assert_eq!(source.reads, 0);
        assert!(sink.frames.is_empty());
    }

    // Catches a blocked read consuming input state or forgetting to rewind
    // sepc: with no input pending, dispatch must reach the source, get
    // `Ok(None)`, return Blocked, leave a0 and the script position untouched,
    // and point sepc back at the ecall so resume retries the same syscall.
    #[test]
    fn read_without_ready_input_blocks_and_rewinds_sepc() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::not_ready();
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Blocked);
        // classifyはsepcをecallの次 (+4) へ進めてからdispatchするため、
        // Blockedはその4 byte分だけ戻した位置を指す。
        assert_eq!(context.sepc(), 0x0010_0500 - 4);
        assert_eq!(context.register(10), STDIN);
        assert_eq!(source.reads, 1);
        assert_eq!(source.position, 0);
        assert!(sink.frames.is_empty());
    }

    // Catches the source call running before argument validation: an
    // invalid read must keep reporting its error even when input is pending
    // or absent, and must not mark the process blocked.
    #[test]
    fn read_validation_errors_precede_the_blocked_check() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::not_ready();
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDOUT,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
        assert_eq!(context.sepc(), 0x0010_0500);
    }

    // Catches a woken reader failing to complete: once input arrives, the
    // retried ecall must run the normal read path and finish.
    #[test]
    fn read_completes_after_input_arrives() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::not_ready();
        let mut scratch_buf = scratch();
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch_buf,
        );
        assert_eq!(flow, SyscallFlow::Blocked);

        source.ready = true;
        source.script = Vec::from(&b"hi"[..]);
        // 再開で同じecallが再実行され、sepcは再び+4された状態でdispatchへ来る。
        let mut context = context;
        context.set_sepc(context.sepc() + 4);
        let (context, flow) = dispatch_fixture_at(
            context,
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch_buf,
        );

        let (start, len) = read_completion(flow);
        assert_eq!((start, len), (MESSAGE_PAGE as u64, 2));
        assert_eq!(context.register(10), STDIN);
    }

    // Catches resuming after a source failure or clobbering the guest context.
    #[test]
    fn source_failure_is_fatal_and_preserves_the_context() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource {
            fail: true,
            ..FakeSource::scripted(b"hello")
        };
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            STDIN,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::SourceFatal(SinkError::Injected));
        assert_eq!(context.register(10), STDIN);
        assert!(sink.frames.is_empty());
    }

    // Catches missing read_file dispatch, path truncation, a missing
    // ReadComplete flow, or a byte count larger than the buffer.
    #[test]
    fn read_file_streams_into_scratch_and_reports_read_complete() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file data");
        let buf_start = MESSAGE_PAGE + 512;
        let mut scratch_buf = scratch();
        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            b"HELLO.TXT".len(),
            buf_start,
            64,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch_buf,
        );

        let (start, len) = read_completion(flow);
        assert_eq!((start, len), (buf_start as u64, 9));
        assert_eq!(&scratch_buf[..9], b"file data");
        assert_eq!(source.seen_path.as_deref(), Some(b"HELLO.TXT".as_slice()));
        assert_eq!(source.reads, 1);
        assert_eq!(context.register(10), MESSAGE_PAGE);
    }

    // Catches reads exceeding the guest buffer not being truncated.
    #[test]
    fn read_file_truncates_the_stream_at_the_guest_buffer() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(&[0x5a; 8192]);
        let buf_start = MESSAGE_PAGE + 512;
        let mut scratch_buf = scratch();
        let (_context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            b"HELLO.TXT".len(),
            buf_start,
            100,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch_buf,
        );

        let (start, len) = read_completion(flow);
        assert_eq!((start, len), (buf_start as u64, 100));
        assert!(scratch_buf[..100].iter().all(|b| *b == 0x5a));
    }

    // Catches storage errors reaching the guest as errno rather than
    // aborting the process or returning a partial count.
    #[test]
    fn read_file_maps_source_errors_to_errno() {
        for errno in [ENOENT, ENOTDIR, EISDIR, ENODEV] {
            let mut sink = FakeSink::default();
            let mut source = FileSource::failing(errno);
            let (context, flow) = dispatch_file_fixture(
                MESSAGE_PAGE,
                b"HELLO.TXT".len(),
                MESSAGE_PAGE + 512,
                64,
                b"HELLO.TXT",
                &mut sink,
                &mut source,
                &mut scratch(),
            );

            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(context.register(10), errno as usize);
        }
    }

    // Catches missing argument validation: an empty or overlong path and a
    // buffer beyond MAX_READ_LEN must be EINVAL before any source call.
    #[test]
    fn read_file_rejects_bad_arguments_before_touching_the_source() {
        for (a1, a3) in [(0, 64), (MAX_PATH_LEN + 1, 64), (9, MAX_READ_LEN + 1)] {
            let mut sink = FakeSink::default();
            let mut source = FileSource::serving(b"file data");
            let (context, flow) = dispatch_file_fixture(
                MESSAGE_PAGE,
                a1,
                MESSAGE_PAGE + 512,
                a3,
                b"HELLO.TXT",
                &mut sink,
                &mut source,
                &mut scratch(),
            );

            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(context.register(10), EINVAL as usize);
            assert_eq!(source.reads, 0);
        }

        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file data");
        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            9,
            MESSAGE_PAGE + 512,
            0,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), 0);
        assert_eq!(source.reads, 0);
    }

    // Catches EFAULT not being reported for an unreadable path pointer or an
    // unwritable destination range, and the source being touched anyway.
    #[test]
    fn read_file_reports_efault_for_bad_user_ranges() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file data");
        let (context, flow) = dispatch_file_fixture(
            0x40_000,
            9,
            MESSAGE_PAGE + 512,
            64,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert_eq!(source.reads, 0);

        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            9,
            0x40_000,
            64,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert_eq!(source.reads, 0);
    }

    // Catches a non-UTF-8 path being passed to the filesystem rather than
    // rejected with EINVAL.
    #[test]
    fn read_file_rejects_a_non_utf8_path() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file data");
        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            3,
            MESSAGE_PAGE + 512,
            64,
            b"A\xffB",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EINVAL as usize);
        assert_eq!(source.reads, 0);
    }

    // Catches the default ControlSource::read_file implementation leaking a
    // successful result: without storage the guest must see ENOSYS.
    #[test]
    fn read_file_without_storage_returns_enosys() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"stdin only");
        let (context, flow) = dispatch_file_fixture(
            MESSAGE_PAGE,
            9,
            MESSAGE_PAGE + 512,
            64,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOSYS as usize);
        assert_eq!(source.reads, 0);
    }

    // Catches open not forwarding the path, the returned fd, or the source's
    // errno, and path validation being skipped.
    #[test]
    fn open_returns_the_allocated_fd_and_validates_the_path() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file data");
        let (context, flow) = dispatch_numbered_file_fixture(
            OPEN_NUMBER,
            MESSAGE_PAGE,
            b"HELLO.TXT".len(),
            0,
            0,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), FIRST_FILE_FD);
        assert_eq!(source.seen_path.as_deref(), Some(b"HELLO.TXT".as_slice()));

        for (a0, a1, content, expected) in [
            (MESSAGE_PAGE, 0, b"HELLO.TXT".as_slice(), EINVAL),
            (
                MESSAGE_PAGE,
                MAX_PATH_LEN + 1,
                b"HELLO.TXT".as_slice(),
                EINVAL,
            ),
            (0x40_000, 9, b"HELLO.TXT".as_slice(), EFAULT),
            (MESSAGE_PAGE, 3, b"A\xffB".as_slice(), EINVAL),
        ] {
            let mut source = FileSource::serving(b"file data");
            let (context, flow) = dispatch_numbered_file_fixture(
                OPEN_NUMBER,
                a0,
                a1,
                0,
                0,
                content,
                &mut sink,
                &mut source,
                &mut scratch(),
            );
            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(context.register(10), expected as usize);
            assert!(source.seen_path.is_none());
        }

        let mut source = FileSource::failing(ENOENT);
        source.open_fd = Err(ENOENT);
        let (context, flow) = dispatch_numbered_file_fixture(
            OPEN_NUMBER,
            MESSAGE_PAGE,
            9,
            0,
            0,
            b"HELLO.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOENT as usize);
    }

    // Catches read on a file fd not reaching read_fd, not truncating at the
    // buffer, or swallowing source errno.
    #[test]
    fn read_on_a_file_fd_streams_via_read_fd() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"file contents");
        let buf_start = MESSAGE_PAGE + 512;
        let mut scratch_buf = scratch();
        let (_context, flow) = dispatch_fixture(
            READ_NUMBER,
            FIRST_FILE_FD,
            buf_start,
            64,
            &mut sink,
            &mut source,
            &mut scratch_buf,
        );

        let (start, len) = read_completion(flow);
        assert_eq!((start, len), (buf_start as u64, 13));
        assert_eq!(&scratch_buf[..13], b"file contents");
        assert_eq!(source.fd_reads, 1);
        assert_eq!(source.reads, 0);

        let mut source = FileSource::failing(EBADF);
        let (context, flow) = dispatch_fixture(
            READ_NUMBER,
            FIRST_FILE_FD,
            buf_start,
            64,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
    }

    // Catches close rejecting standard descriptors and forwarding file fds,
    // including the source's EBADF for unallocated slots.
    #[test]
    fn close_rejects_standard_fds_and_closes_file_fds() {
        let mut sink = FakeSink::default();
        for fd in [0, 1, 2, 7] {
            let mut source = FileSource::serving(b"");
            let (context, flow) = dispatch_numbered_file_fixture(
                CLOSE_NUMBER,
                fd,
                0,
                0,
                0,
                b"",
                &mut sink,
                &mut source,
                &mut scratch(),
            );
            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(context.register(10), EBADF as usize);
            assert_eq!(source.closes, 0);
        }

        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            CLOSE_NUMBER,
            FIRST_FILE_FD,
            0,
            0,
            0,
            b"",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), 0);
        assert_eq!(source.closes, 1);

        let (context, flow) = dispatch_numbered_file_fixture(
            CLOSE_NUMBER,
            FIRST_FILE_FD + 1,
            0,
            0,
            0,
            b"",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);
        assert_eq!(source.closes, 2);
    }

    // Catches create routing to create_file rather than open_file, so a
    // writable fd never reaches a guest that only asked to read.
    #[test]
    fn create_routes_to_create_file_and_validates_the_path() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            CREATE_NUMBER,
            MESSAGE_PAGE,
            b"NEW.TXT".len(),
            0,
            0,
            b"NEW.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), FIRST_FILE_FD);
        assert_eq!(source.creates, 1);
        assert_eq!(source.seen_path.as_deref(), Some(b"NEW.TXT".as_slice()));

        // path検証はopenと同じく、sourceへ触れる前にerrnoを確定する。
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            CREATE_NUMBER,
            0x40_000,
            9,
            0,
            0,
            b"NEW.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert_eq!(source.creates, 0);
    }

    // Catches unlink not forwarding the path, not returning 0 on success,
    // or swallowing the source's errno, and path validation being skipped.
    #[test]
    fn unlink_deletes_via_the_source_and_validates_the_path() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_numbered_file_fixture(
            UNLINK_NUMBER,
            MESSAGE_PAGE,
            b"OLD.TXT".len(),
            0,
            0,
            b"OLD.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), 0);
        assert_eq!(source.unlinks, 1);
        assert_eq!(source.seen_path.as_deref(), Some(b"OLD.TXT".as_slice()));

        for (a0, a1, content, expected) in [
            (MESSAGE_PAGE, 0, b"OLD.TXT".as_slice(), EINVAL),
            (
                MESSAGE_PAGE,
                MAX_PATH_LEN + 1,
                b"OLD.TXT".as_slice(),
                EINVAL,
            ),
            (0x40_000, 7, b"OLD.TXT".as_slice(), EFAULT),
            (MESSAGE_PAGE, 3, b"A\xffB".as_slice(), EINVAL),
        ] {
            let mut source = FileSource::serving(b"");
            let (context, flow) = dispatch_numbered_file_fixture(
                UNLINK_NUMBER,
                a0,
                a1,
                0,
                0,
                content,
                &mut sink,
                &mut source,
                &mut scratch(),
            );
            assert_eq!(flow, SyscallFlow::Resume);
            assert_eq!(context.register(10), expected as usize);
            assert_eq!(source.unlinks, 0);
        }

        let mut source = FileSource::failing(ENOENT);
        let (context, flow) = dispatch_numbered_file_fixture(
            UNLINK_NUMBER,
            MESSAGE_PAGE,
            7,
            0,
            0,
            b"OLD.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOENT as usize);
        assert_eq!(source.unlinks, 1);
    }

    // Catches the default ControlSource::unlink implementation leaking a
    // successful result: without storage the guest must see ENOSYS.
    #[test]
    fn unlink_without_storage_returns_enosys() {
        let mut sink = FakeSink::default();
        let mut source = FakeSource::scripted(b"stdin only");
        let (context, flow) = dispatch_numbered_file_fixture(
            UNLINK_NUMBER,
            MESSAGE_PAGE,
            7,
            0,
            0,
            b"OLD.TXT",
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), ENOSYS as usize);
    }

    // Catches write on a file fd reaching neither write_fd nor the errno
    // path, and frame output never leaking to fd 3+.
    #[test]
    fn write_on_a_file_fd_routes_to_write_fd() {
        let mut sink = FakeSink::default();
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            FIRST_FILE_FD,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );

        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), MESSAGE.len());
        assert_eq!(source.writes, 1);
        assert_eq!(source.written, MESSAGE);
        assert!(sink.frames.is_empty());

        // sourceのerrnoはそのままa0へ返る。
        let mut source = FileSource::failing(EBADF);
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            FIRST_FILE_FD,
            MESSAGE_PAGE,
            MESSAGE.len(),
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EBADF as usize);

        // 検証できないuser pointerはsourceへ届く前にEFAULT。
        let mut source = FileSource::serving(b"");
        let (context, flow) = dispatch_fixture(
            WRITE_NUMBER,
            FIRST_FILE_FD,
            0x40_000,
            64,
            &mut sink,
            &mut source,
            &mut scratch(),
        );
        assert_eq!(flow, SyscallFlow::Resume);
        assert_eq!(context.register(10), EFAULT as usize);
        assert_eq!(source.writes, 0);
    }
}
