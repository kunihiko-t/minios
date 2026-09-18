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
    /// 呼び出しprocessのpidを返す。
    Getpid = 15,
    /// `a0`/`a1`が指すFAT32 pathのELF fileを新processとして起動し、
    /// childのpidを返す。childは親と独立してscheduleされる。
    Spawn = 16,
    /// `a0`のpidを持つprocessの終了を待ち、その終了codeを返す。
    /// 対象がまだliveなら呼び出しprocessをblockし、対象の終了で
    /// 再実行される。
    Waitpid = 17,
    /// `a0`/`a1`が指すFAT32 pathのmetadataを`a2`のuser bufferへ
    /// 8 byteの`Stat`として書き込む。fileとdirectory両方を受理する。
    Stat = 18,
    /// `a0`のfile descriptorが指すfileのmetadataを`a1`のuser bufferへ
    /// 8 byteの`Stat`として書き込む。
    Fstat = 19,
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

/// `Stat::kind`の値。通常のfile。
pub const STAT_KIND_FILE: u32 = 0;
/// `Stat::kind`の値。directory。
pub const STAT_KIND_DIR: u32 = 1;
/// `stat`/`fstat`が`out` pointerへ書き込む`Stat`のbyte長。
pub const STAT_LEN: usize = 8;

/// `stat`/`fstat`がuser bufferへ書き込むmetadata。`size`はfileの
/// byte数（directoryはFAT32の規約で0）、`kind`は`STAT_KIND_*`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub size: u32,
    pub kind: u32,
}

impl Stat {
    /// LEの8 byteへserializeする。guestは同じlayoutで読み戻す。
    pub const fn to_le_bytes(self) -> [u8; STAT_LEN] {
        let size = self.size.to_le_bytes();
        let kind = self.kind.to_le_bytes();
        [
            size[0], size[1], size[2], size[3], kind[0], kind[1], kind[2], kind[3],
        ]
    }

    /// `to_le_bytes`が書いた形式から読み戻す。guest側のdecodeに使う。
    pub const fn from_le_bytes(bytes: [u8; STAT_LEN]) -> Self {
        Self {
            size: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            kind: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

pub const ENOENT: isize = -2;
pub const EIO: isize = -5;
pub const EBADF: isize = -9;
pub const ECHILD: isize = -10;
pub const ENOMEM: isize = -12;
pub const EFAULT: isize = -14;
pub const EEXIST: isize = -17;
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
        assert_eq!(SyscallNumber::Getpid as usize, 15);
        assert_eq!(SyscallNumber::Spawn as usize, 16);
        assert_eq!(SyscallNumber::Waitpid as usize, 17);
        assert_eq!(SyscallNumber::Stat as usize, 18);
        assert_eq!(SyscallNumber::Fstat as usize, 19);
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
        assert_eq!(ECHILD, -10);
        assert_eq!(ENOMEM, -12);
        assert_eq!(EFAULT, -14);
        assert_eq!(EEXIST, -17);
        assert_eq!(ENODEV, -19);
        assert_eq!(ENOTDIR, -20);
        assert_eq!(EISDIR, -21);
        assert_eq!(EINVAL, -22);
        assert_eq!(EMFILE, -24);
        assert_eq!(ENOSPC, -28);
        assert_eq!(ENOSYS, -38);
        assert_eq!(ENOTEMPTY, -39);
        assert_eq!(STAT_KIND_FILE, 0);
        assert_eq!(STAT_KIND_DIR, 1);
        assert_eq!(STAT_LEN, 8);
    }

    // Catches the Stat wire layout drifting: size must occupy the first
    // four LE bytes and kind the last four, matching what the guest reads.
    #[test]
    fn stat_serializes_size_then_kind_as_le_bytes() {
        let stat = Stat {
            size: 0x0102_0304,
            kind: STAT_KIND_DIR,
        };
        assert_eq!(
            stat.to_le_bytes(),
            [0x04, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00]
        );
        assert_eq!(Stat::from_le_bytes(stat.to_le_bytes()), stat);
    }
}
