#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallNumber {
    Write = 1,
    Exit = 2,
}

pub const STDOUT: usize = 1;
pub const STDERR: usize = 2;
pub const MAX_WRITE_LEN: usize = 4096;

pub const ENOSYS: isize = -38;
pub const EBADF: isize = -9;
pub const EFAULT: isize = -14;
pub const EINVAL: isize = -22;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_and_descriptors_are_stable() {
        assert_eq!(SyscallNumber::Write as usize, 1);
        assert_eq!(SyscallNumber::Exit as usize, 2);
        assert_eq!(STDOUT, 1);
        assert_eq!(STDERR, 2);
        assert_eq!(MAX_WRITE_LEN, 4096);
        assert_eq!(ENOSYS, -38);
        assert_eq!(EBADF, -9);
        assert_eq!(EFAULT, -14);
        assert_eq!(EINVAL, -22);
    }
}
