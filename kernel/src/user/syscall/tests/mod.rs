extern crate std;

use std::{boxed::Box, collections::BTreeMap, string::String, vec, vec::Vec};

use super::{ConsoleFd, ControlSink, ControlSource, FD_TABLE_LEN, SyscallFlow, dispatch_syscall};
use crate::{
    memory::frame::{FrameAllocator, PAGE_SIZE},
    user::context::UserContext,
    vm::{AddressSpaceBuilder, FrameStore, PageFlags, VirtPage},
};
use minios_abi::{
    control::FrameKind,
    manifest::ARG_MAX_LEN,
    syscall::{
        DIRENT_LEN, DIRENT_NAME_LEN, DirEnt, EBADF, EFAULT, EINVAL, EISDIR, ENODEV, ENOENT, ENOSYS,
        ENOTDIR, ENOTEMPTY, ESPIPE, FIRST_FILE_FD, MAX_OPEN_FILES, MAX_PATH_LEN, MAX_READ_LEN,
        MAX_WRITE_LEN, SPAWN_MAX_ARGC, STAT_KIND_FILE, STAT_LEN, STDERR, STDIN, STDOUT, Stat,
        SyscallNumber,
    },
};

mod io;
mod path;
mod proc;
mod seek;
mod stat;

const MESSAGE: &[u8] = b"MK4";
const MESSAGE_PAGE: usize = 0x0010_1000;
const WRITE_NUMBER: usize = SyscallNumber::Write as usize;
const READ_NUMBER: usize = SyscallNumber::Read as usize;
const READ_FILE_NUMBER: usize = SyscallNumber::ReadFile as usize;
const OPEN_NUMBER: usize = SyscallNumber::Open as usize;
const CLOSE_NUMBER: usize = SyscallNumber::Close as usize;
const CREATE_NUMBER: usize = SyscallNumber::Create as usize;
const UNLINK_NUMBER: usize = SyscallNumber::Unlink as usize;
const RENAME_NUMBER: usize = SyscallNumber::Rename as usize;
const MKDIR_NUMBER: usize = SyscallNumber::Mkdir as usize;
const RMDIR_NUMBER: usize = SyscallNumber::Rmdir as usize;
const LSEEK_NUMBER: usize = SyscallNumber::Lseek as usize;
const PREAD_NUMBER: usize = SyscallNumber::Pread as usize;
const PIPE_NUMBER: usize = SyscallNumber::Pipe as usize;
const PWRITE_NUMBER: usize = SyscallNumber::Pwrite as usize;
const GETPID_NUMBER: usize = SyscallNumber::Getpid as usize;
const SPAWN_NUMBER: usize = SyscallNumber::Spawn as usize;
const WAITPID_NUMBER: usize = SyscallNumber::Waitpid as usize;
const KILL_NUMBER: usize = SyscallNumber::Kill as usize;
const STAT_NUMBER: usize = SyscallNumber::Stat as usize;
const FSTAT_NUMBER: usize = SyscallNumber::Fstat as usize;
const READDIR_NUMBER: usize = SyscallNumber::Readdir as usize;
const EXEC_NUMBER: usize = SyscallNumber::Exec as usize;
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

    fn frame_mut(&mut self, frame_start: usize) -> Result<&mut [u8; PAGE_SIZE], TestStoreError> {
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

/// dispatchテストがsourceの返り値に使う固定entry。
fn dirent_fixture() -> DirEnt {
    let mut name = [0u8; DIRENT_NAME_LEN];
    name[..9].copy_from_slice(b"HELLO.TXT");
    DirEnt {
        name_len: 9,
        kind: STAT_KIND_FILE,
        name,
    }
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
    renames: usize,
    mkdirs: usize,
    rmdirs: usize,
    seen_new_path: Option<Vec<u8>>,
    seeks: usize,
    preads: usize,
    pwrites: usize,
    seen_offset: Option<u64>,
    seen_seek: Option<(isize, usize)>,
    seek_result: Result<u64, isize>,
    getpid_result: isize,
    spawns: usize,
    spawn_result: Result<usize, isize>,
    seen_argv: Vec<String>,
    waitpids: usize,
    seen_wait_pid: Option<usize>,
    waitpid_result: Result<Option<u32>, isize>,
    kills: usize,
    seen_kill_pid: Option<usize>,
    kill_result: Result<(), isize>,
    stats: usize,
    stat_result: Result<Stat, isize>,
    fstats: usize,
    seen_fstat_fd: Option<usize>,
    fstat_result: Result<Stat, isize>,
    readdirs: usize,
    seen_readdir_index: Option<usize>,
    readdir_result: Result<Option<DirEnt>, isize>,
    execs: usize,
    /// `exec`成功時に返す新contextの`(entry, sp)`。
    exec_result: Result<(u64, u64), isize>,
    pipes: usize,
    pipe_result: Result<(usize, usize), isize>,
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
            renames: 0,
            mkdirs: 0,
            rmdirs: 0,
            seen_new_path: None,
            seeks: 0,
            preads: 0,
            pwrites: 0,
            seen_offset: None,
            seen_seek: None,
            seek_result: Ok(8),
            getpid_result: 7,
            spawns: 0,
            spawn_result: Ok(11),
            seen_argv: Vec::new(),
            waitpids: 0,
            seen_wait_pid: None,
            waitpid_result: Ok(Some(42)),
            kills: 0,
            seen_kill_pid: None,
            kill_result: Ok(()),
            stats: 0,
            stat_result: Ok(Stat {
                size: 18,
                kind: STAT_KIND_FILE,
            }),
            fstats: 0,
            seen_fstat_fd: None,
            fstat_result: Ok(Stat {
                size: 18,
                kind: STAT_KIND_FILE,
            }),
            readdirs: 0,
            seen_readdir_index: None,
            readdir_result: Ok(Some(dirent_fixture())),
            execs: 0,
            exec_result: Ok((0x40_0000, 0x40_7000)),
            pipes: 0,
            pipe_result: Ok((FIRST_FILE_FD, FIRST_FILE_FD + 1)),
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

    fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), isize> {
        self.renames += 1;
        self.seen_path = Some(Vec::from(old_path.as_bytes()));
        self.seen_new_path = Some(Vec::from(new_path.as_bytes()));
        self.result
    }

    fn make_dir(&mut self, path: &str) -> Result<(), isize> {
        self.mkdirs += 1;
        self.seen_path = Some(Vec::from(path.as_bytes()));
        self.result
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), isize> {
        self.rmdirs += 1;
        self.seen_path = Some(Vec::from(path.as_bytes()));
        self.result
    }

    fn seek_fd(&mut self, _fd: usize, offset: isize, whence: usize) -> Result<u64, isize> {
        self.seeks += 1;
        self.seen_seek = Some((offset, whence));
        self.seek_result
    }

    fn pread_fd(&mut self, _fd: usize, offset: u64, output: &mut [u8]) -> Result<usize, isize> {
        self.preads += 1;
        self.seen_offset = Some(offset);
        self.result?;
        let count = core::cmp::min(self.script.len(), output.len());
        output[..count].copy_from_slice(&self.script[..count]);
        Ok(count)
    }

    fn pwrite_fd(&mut self, _fd: usize, offset: u64, data: &[u8]) -> Result<usize, isize> {
        self.pwrites += 1;
        self.seen_offset = Some(offset);
        self.result?;
        self.written.extend_from_slice(data);
        Ok(data.len())
    }

    fn getpid(&mut self) -> isize {
        self.getpid_result
    }

    fn spawn(&mut self, path: &str, argv: &[&str]) -> Result<usize, isize> {
        self.spawns += 1;
        self.seen_path = Some(Vec::from(path.as_bytes()));
        self.seen_argv = argv.iter().map(|text| String::from(*text)).collect();
        self.spawn_result
    }

    fn waitpid(&mut self, pid: usize) -> Result<Option<u32>, isize> {
        self.waitpids += 1;
        self.seen_wait_pid = Some(pid);
        self.waitpid_result
    }

    fn kill(&mut self, pid: usize) -> Result<(), isize> {
        self.kills += 1;
        self.seen_kill_pid = Some(pid);
        self.kill_result
    }

    fn stat(&mut self, path: &str) -> Result<Stat, isize> {
        self.stats += 1;
        self.seen_path = Some(Vec::from(path.as_bytes()));
        self.stat_result
    }

    fn fstat(&mut self, fd: usize) -> Result<Stat, isize> {
        self.fstats += 1;
        self.seen_fstat_fd = Some(fd);
        self.fstat_result
    }

    fn readdir(&mut self, path: &str, index: usize) -> Result<Option<DirEnt>, isize> {
        self.readdirs += 1;
        self.seen_path = Some(Vec::from(path.as_bytes()));
        self.seen_readdir_index = Some(index);
        self.readdir_result
    }

    fn create_pipe(&mut self) -> Result<(usize, usize), isize> {
        self.pipes += 1;
        self.pipe_result
    }

    fn exec(&mut self, path: &str) -> Result<UserContext, isize> {
        self.execs += 1;
        self.seen_path = Some(Vec::from(path.as_bytes()));
        self.exec_result.map(|(entry, sp)| {
            UserContext::new(
                crate::vm::VirtAddr::try_new(entry).expect("fixture entry"),
                crate::vm::VirtAddr::try_new(sp).expect("fixture stack top"),
            )
        })
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

fn read_completion(flow: SyscallFlow<SinkError>) -> (u64, usize) {
    match flow {
        SyscallFlow::ReadComplete { start, len } => (start, len),
        other => panic!("expected ReadComplete, actual {other:?}"),
    }
}
