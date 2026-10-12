//! user system callを番号ごとに検証し、`ControlSink`/`ControlSource`への
//! 委譲と純粋なdispatch結果（`SyscallFlow`）へ変換する。未知の番号は`ENOSYS`。

use crate::user::context::UserContext;
use minios_abi::{
    control::FrameKind,
    syscall::{DirEnt, ENOSYS, STDERR, STDIN, STDOUT, Stat},
};

mod dispatch;
#[cfg(test)]
mod tests;

pub use dispatch::{complete_read, dispatch_syscall};

/// processごとのfd tableの長さ。fd 0から`FIRST_FILE_FD + MAX_OPEN_FILES - 1`
/// までが有効な番号で、これ以上は常に`EBADF`である。
pub use minios_abi::syscall::FD_TABLE_LEN;

/// consoleを指すfd entryの向き。新processはfd 0/1/2にこの3個を持つが、
/// `dup2`や`close`で他のfdへ移したり空けたりできる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleFd {
    /// Stdin frameを読む入力。`write`は`EBADF`。
    Stdin,
    /// Stdout frameへ書く出力。`read`は`EBADF`。
    Stdout,
    /// Stderr frameへ書く出力。`read`は`EBADF`。
    Stderr,
}

/// fd tableを持たない経路の`console_fd`。fd 0/1/2を常にconsoleとみなす。
pub const fn fixed_console_fd(fd: usize) -> Option<ConsoleFd> {
    match fd {
        STDIN => Some(ConsoleFd::Stdin),
        STDOUT => Some(ConsoleFd::Stdout),
        STDERR => Some(ConsoleFd::Stderr),
        _ => None,
    }
}

/// syscall結果の受け先。kernelはUARTへframeを載せ、host testは記録する。
pub trait ControlSink {
    type Error;

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error>;
}

/// stdin byteの供給元。kernelはUARTのStdin frameから引き、host testは用意した列を返す。
pub trait ControlSource {
    type Error;

    /// `fd`がconsoleを指すならその向きを返す。dispatchは`read`/`write`の
    /// 経路をfd番号でなくこの結果で選ぶ。default実装はfd tableを持たない
    /// source向けの`fixed_console_fd`である。
    fn console_fd(&mut self, fd: usize) -> Option<ConsoleFd> {
        fixed_console_fd(fd)
    }

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

    /// `fd`のoffsetを`whence`（`SEEK_SET`/`SEEK_CUR`/`SEEK_END`）基準の
    /// `offset`へ更新し、新しいoffsetを返す。`Err`はそのまま`a0`へ返す
    /// errnoである。default実装は`ENOSYS`。
    fn seek_fd(&mut self, fd: usize, offset: isize, whence: usize) -> Result<u64, isize> {
        let _ = (fd, offset, whence);
        Err(ENOSYS)
    }

    /// `fd`のfileの`offset` byte目から`output`へ最大`output.len()` byte
    /// 読み、読んだbyte数を返す。fd保持のoffsetは動かさない。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn pread_fd(&mut self, fd: usize, offset: u64, output: &mut [u8]) -> Result<usize, isize> {
        let _ = (fd, offset, output);
        Err(ENOSYS)
    }

    /// `fd`のfileの`offset` byte目へ`data`を書き、書いたbyte数を返す。
    /// fd保持のoffsetは動かさない。`Err`はそのまま`a0`へ返すerrnoである。
    /// default実装は`ENOSYS`。
    fn pwrite_fd(&mut self, fd: usize, offset: u64, data: &[u8]) -> Result<usize, isize> {
        let _ = (fd, offset, data);
        Err(ENOSYS)
    }

    /// `old_path`のentryを`new_path`へ移す。同一directory内では8.3名の
    /// 書き換え、別directoryへの指定は移動になる。fileの中身とclusterは
    /// 変わらず、開いているfdは有効のままである。同名へのrenameはno-op
    /// の成功、既存fileへのrenameはPOSIXと同じく置き換えで、置き換え
    /// られたfileを指すfdは失効する。`Err`はそのまま`a0`へ返すerrno
    /// である。default実装は`ENOSYS`。
    fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), isize> {
        let _ = (old_path, new_path);
        Err(ENOSYS)
    }

    /// `path`のdirectoryを作成する。同名entryがあれば`EEXIST`、
    /// 8.3へ正規化できない名前は`EINVAL`である。`Err`はそのまま
    /// `a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn make_dir(&mut self, path: &str) -> Result<(), isize> {
        let _ = path;
        Err(ENOSYS)
    }

    /// `path`の空のdirectoryを削除する。`.`/`..`以外のentryを残すdirは
    /// `ENOTEMPTY`、fileは`ENOTDIR`である。`Err`はそのまま`a0`へ返す
    /// errnoである。default実装は`ENOSYS`。
    fn remove_dir(&mut self, path: &str) -> Result<(), isize> {
        let _ = path;
        Err(ENOSYS)
    }

    /// 呼び出しprocessのpidを返す。負の値はそのまま`a0`へ返すerrno
    /// である。default実装は`ENOSYS`。
    fn getpid(&mut self) -> isize {
        ENOSYS
    }

    /// `path`のFAT32 fileをELFとして新processへ読み込み、採番したpidを
    /// 返す。`argv`はdispatchが検証・copy済みのchildのargvで、空なら
    /// pathのbasenameだけをargv[0]にする。childは親と独立してscheduleされ、
    /// 終了codeは`waitpid`で回収できる。`Err`はそのまま`a0`へ返すerrno
    /// である。default実装は`ENOSYS`。
    fn spawn(&mut self, path: &str, argv: &[&str]) -> Result<usize, isize> {
        let _ = (path, argv);
        Err(ENOSYS)
    }

    /// `pid`のprocessの終了codeを返す。`Ok(None)`は対象がまだliveで
    /// 呼び出しprocessをblockすることを意味し、対象の終了で同じecallが
    /// 再実行される。`Err`はそのまま`a0`へ返すerrnoである。
    /// default実装は`ENOSYS`。
    fn waitpid(&mut self, pid: usize) -> Result<Option<u32>, isize> {
        let _ = pid;
        Err(ENOSYS)
    }

    /// `pid`のprocessへ終了を要求する。`Ok(())`は要求を受理したことで、
    /// 実際の回収と`waitpid`への終了code通知はrun loop側の契約である。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn kill(&mut self, pid: usize) -> Result<(), isize> {
        let _ = pid;
        Err(ENOSYS)
    }

    /// `path`のfileまたはdirectoryのmetadataを返す。`Err`はそのまま
    /// `a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn stat(&mut self, path: &str) -> Result<Stat, isize> {
        let _ = path;
        Err(ENOSYS)
    }

    /// `fd`のfile metadataを返す。`Err`はそのまま`a0`へ返すerrno
    /// である。default実装は`ENOSYS`。
    fn fstat(&mut self, fd: usize) -> Result<Stat, isize> {
        let _ = fd;
        Err(ENOSYS)
    }

    /// `path`のdirectoryの`index`番目のentryを返す。`""`はrootを
    /// 指し、`Ok(None)`はindexが末尾を越えたことを表す。`Err`は
    /// そのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn readdir(&mut self, path: &str, index: usize) -> Result<Option<DirEnt>, isize> {
        let _ = (path, index);
        Err(ENOSYS)
    }

    /// `path`のELFで呼び出しprocessのimageを置き換える。`Ok`の
    /// `UserContext`は新imageの初期frameで、dispatchがtrap slotへ
    /// 書き込み`Exec`を返す。`Err`はそのまま`a0`へ返すerrnoである。
    /// 旧imageの解放はrun loop側の契約である。default実装は`ENOSYS`。
    fn exec(&mut self, path: &str) -> Result<UserContext, isize> {
        let _ = path;
        Err(ENOSYS)
    }

    /// 新しいpipeを作り、`(read_fd, write_fd)`を返す。両端は呼び出し
    /// processのfd tableへ割り当てられ、`spawn`したchildへ継承される。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn create_pipe(&mut self) -> Result<(usize, usize), isize> {
        Err(ENOSYS)
    }

    /// 呼び出しprocessのbreakを`increment` byte動かし、旧breakを返す。
    /// `Err`はそのまま`a0`へ返すerrnoである。default実装は`ENOSYS`。
    fn sbrk(&mut self, increment: isize) -> Result<u64, isize> {
        let _ = increment;
        Err(ENOSYS)
    }

    /// boot以降の経過millisecondを返す。負の値はそのまま`a0`へ返す
    /// errnoである。default実装は`ENOSYS`。
    fn clock(&mut self) -> isize {
        ENOSYS
    }

    /// 呼び出しprocessを`millis`（1以上）経過までのsleep状態へmarkする。
    /// dispatchは`Ok`で`a0`へ0を書き`Yield`を返すため、guestは起床後に
    /// ecallの次から再開する。`Err`はそのまま`a0`へ返すerrnoである。
    /// default実装は`ENOSYS`。
    fn sleep(&mut self, millis: usize) -> Result<(), isize> {
        let _ = millis;
        Err(ENOSYS)
    }

    /// `newfd`を閉じてから`oldfd`と同じentryを指させ、`newfd`を返す。
    /// 範囲と同一fdの検査はdispatch済みである。`Err`はそのまま`a0`へ
    /// 返すerrnoである。default実装は`ENOSYS`。
    fn dup2(&mut self, oldfd: usize, newfd: usize) -> Result<usize, isize> {
        let _ = (oldfd, newfd);
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
    /// `exec`がimageを差し替え、trap slotへ新imageの初期contextを書いた。
    /// kernelへ戻ってcaller satpを離れた後、run loopが退避した旧imageを
    /// 解放する。process自体はrunnableのまま残り、次のdispatchで
    /// 新しいsatpとcontextで`__run_user`へ入る。
    Exec,
    /// `yield`または`sleep`が残りのtime sliceを手放した。`a0`は0で、
    /// `sepc`はecallの次を指したままである。run loopはtimer preemptと
    /// 同じく扱い、次のprocessを選ぶ。`sleep`はcontrol層がcallerを
    /// sleep状態へmark済みなので、deadlineまで選ばれない。
    Yield,
}
