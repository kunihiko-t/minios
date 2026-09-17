#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallNumber {
    Write = 1,
    Exit = 2,
    Read = 3,
    ReadFile = 4,
    Open = 5,
    Close = 6,
    Create = 7,
    Unlink = 8,
    Lseek = 9,
    Pread = 10,
    Pwrite = 11,
    Rename = 12,
    Mkdir = 13,
    Rmdir = 14,
}

pub const STDIN: usize = 0;
pub const STDOUT: usize = 1;
pub const STDERR: usize = 2;
pub const MAX_WRITE_LEN: usize = 4096;
pub const MAX_READ_LEN: usize = 4096;
pub const MAX_PATH_LEN: usize = 256;
pub const FIRST_FILE_FD: usize = 3;
pub const MAX_OPEN_FILES: usize = 4;

/// `lseek`の`a2`が取る基準位置。file先頭からの絶対offset。
pub const SEEK_SET: usize = 0;
/// 現在のfd offsetからの相対offset。
pub const SEEK_CUR: usize = 1;
/// file末尾からの相対offset。
pub const SEEK_END: usize = 2;

pub const ENOENT: isize = -2;
pub const EIO: isize = -5;
pub const EBADF: isize = -9;
pub const ENOMEM: isize = -12;
pub const EFAULT: isize = -14;
pub const EEXIST: isize = -17;
pub const EXDEV: isize = -18;
pub const ENODEV: isize = -19;
pub const ENOTDIR: isize = -20;
pub const EISDIR: isize = -21;
pub const EINVAL: isize = -22;
pub const EMFILE: isize = -24;
pub const ENOSPC: isize = -28;
pub const ENOSYS: isize = -38;
pub const ENOTEMPTY: isize = -39;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_and_descriptors_are_stable() {
        assert_eq!(SyscallNumber::Write as usize, 1);
        assert_eq!(SyscallNumber::Exit as usize, 2);
        assert_eq!(SyscallNumber::Read as usize, 3);
        assert_eq!(SyscallNumber::ReadFile as usize, 4);
        assert_eq!(SyscallNumber::Open as usize, 5);
        assert_eq!(SyscallNumber::Close as usize, 6);
        assert_eq!(SyscallNumber::Create as usize, 7);
        assert_eq!(SyscallNumber::Unlink as usize, 8);
        assert_eq!(SyscallNumber::Lseek as usize, 9);
        assert_eq!(SyscallNumber::Pread as usize, 10);
        assert_eq!(SyscallNumber::Pwrite as usize, 11);
        assert_eq!(SyscallNumber::Rename as usize, 12);
        assert_eq!(SyscallNumber::Mkdir as usize, 13);
        assert_eq!(SyscallNumber::Rmdir as usize, 14);
        assert_eq!(SEEK_SET, 0);
        assert_eq!(SEEK_CUR, 1);
        assert_eq!(SEEK_END, 2);
        assert_eq!(STDIN, 0);
        assert_eq!(STDOUT, 1);
        assert_eq!(STDERR, 2);
        assert_eq!(MAX_WRITE_LEN, 4096);
        assert_eq!(MAX_READ_LEN, 4096);
        assert_eq!(MAX_PATH_LEN, 256);
        assert_eq!(FIRST_FILE_FD, 3);
        assert_eq!(MAX_OPEN_FILES, 4);
        assert_eq!(ENOENT, -2);
        assert_eq!(EIO, -5);
        assert_eq!(EBADF, -9);
        assert_eq!(ENOMEM, -12);
        assert_eq!(EFAULT, -14);
        assert_eq!(EEXIST, -17);
        assert_eq!(EXDEV, -18);
        assert_eq!(ENODEV, -19);
        assert_eq!(ENOTDIR, -20);
        assert_eq!(EISDIR, -21);
        assert_eq!(EINVAL, -22);
        assert_eq!(EMFILE, -24);
        assert_eq!(ENOSPC, -28);
        assert_eq!(ENOSYS, -38);
        assert_eq!(ENOTEMPTY, -39);
    }
}
