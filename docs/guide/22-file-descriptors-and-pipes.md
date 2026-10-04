# 22. file descriptorとpipe

## 学習目標

processごとのfd tableが、file fdとpipe端を同じslot配列で管理する構造を説明できるようになります。
`open`、`read`、`close`、`create`、`write`、`lseek`、`pread`、`pwrite`が、fdのoffsetと方向をどう扱うかを追います。
`stat`、`fstat`、`readdir`がmetadataを固定layoutでuser bufferへ返す規約を確認します。
`unlink`と`rename`が全processのfdを失効または追従させる理由を確認します。
kernel所有のring bufferであるpipeについて、block、EOF、`EPIPE`、`ESPIPE`の規約を確認します。

## 背景

第21章のFAT32 driverは、pathからfileを探し、cluster chainを読み書きできます。
しかしpathだけを受け取るsyscallでは、guestは読むたびにpath解決をやり直し、読み途中の位置をkernelへ伝えられません。
最初の`read_file`（`a0/a1=path`、`a2/a3=buffer`）はfile全体をbufferの長さで打ち切って返すだけで、続きを読む手段を持ちません。

**file descriptor**（fd）は、open済みfileの位置記述子とoffsetをkernel側に保持し、guestへは小さな整数だけを渡す仕組みです。
fd番号はprocess内のtableを指すため、別processのfdを推測しても参照できません。

**pipe**は、fileと同じfd経由で読み書きするkernel内のbyte channelです。
`spawn`したchildへfdを継承すれば、diskを介さずにparentからchildへbyteを流せます。
pipeは容量が有限のため、空のpipeからの`read`と満杯のpipeへの`write`はprocessを待たせる必要があります。

## 実装

### fd table

fd tableは[`kernel/src/process.rs`](../../kernel/src/process.rs)の`FileFdTable`です。
slot数は[`abi/src/syscall.rs`](../../abi/src/syscall.rs)の`MAX_OPEN_FILES`（4）で、fd番号はslot index + `FIRST_FILE_FD`（3）です。
各slotは`Option<FdEntry>`であり、`FdEntry::File(FileFd)`か`FdEntry::Pipe { id, write }`のどちらかを持ちます。

`FileFd`はFAT32の`FileDesc`、次に読み書きする`offset`、`writable`の3つを持ちます。
`open`はread専用、`create`はwrite専用のfdを作り、両方向のfdは作りません。
`alloc_fd`は先頭から空きslotを探し、空きがなければ`EMFILE`を返します。
`close`したslotは次の`alloc_fd`で再利用されるため、同じfd番号が別のfileを指すことがあります。

`spawn`は`file_fds_snapshot`でcallerのtableをcopyしてchildへ渡します。
copyのため、spawn後のoffset変更やcloseは互いに影響しません。
pipe端だけは同じpipe idを指すため、bufferはparentとchildで共有されます。

### file fdのsyscall

番号とerrnoは[`abi/src/syscall.rs`](../../abi/src/syscall.rs)にあり、受け口は[`kernel/src/user/syscall.rs`](../../kernel/src/user/syscall.rs)の`dispatch_syscall`です。
dispatch層はfd範囲、長さ、user pointerを先に検証し、storageへ触れる処理は`ControlSource`の実装である[`kernel/src/control.rs`](../../kernel/src/control.rs)へ委譲します。
`dispatch_read`はstdinかfile fd範囲でなければ`EBADF`、`MAX_READ_LEN`（4096）超過なら`EINVAL`、書けないbufferなら`EFAULT`を返してから`read_fd`を呼びます。
`read_fd`は`FileFd`の`offset`から`read_range`で読み、読んだ分だけ`advance`します。
writableなfdへの`read`とread専用fdへの`write`は`EBADF`です。

`lseek`は`seek_fd`で`SEEK_SET`、`SEEK_CUR`、`SEEK_END`を基準にoffsetを設定します。
`SEEK_END`の基準は`FileDesc`の現在sizeで、結果が負になる指定と未知のwhenceは`EINVAL`です。
file末尾を越えるoffsetは受理され、その位置からの`read`は0を返します。
`pread`と`pwrite`は`a3`の明示offsetで読み書きし、fdが保持するoffsetを動かしません。

`stat`は`a0/a1`のpath、`fstat`は`a0`のfdを受け、8 byteの`Stat`（`size: u32`、`kind: u32`のLE）を書きます。
`kind`は`STAT_KIND_FILE`、`STAT_KIND_DIR`、`STAT_KIND_PIPE`のいずれかで、directoryとpipeの`size`は0です。
`readdir`は`a0/a1`のdirectory、`a2`のindex、`a3`のbufferを受け、`DIRENT_LEN`（263）byteの`DirEnt`を書きます。
`a1`=0はroot directoryを指す`readdir`固有の規約で、indexが末尾を越えると0を返します。
どの経路もout bufferの`EFAULT`をsourceの呼び出しより先に確定し、成功時は`ReadComplete`経由でscratchからuser memoryへcopyします。

### unlinkとrenameによるfdの失効と追従

`unlink`は`unlink_file`で削除したentryの`(dir_cluster, dir_index)`を受け取り、[`kernel/src/main.rs`](../../kernel/src/main.rs)の`revoke_file_fds`を呼びます。
`ProcessTable::revoke_file_fds`は全processのtableを走査し、同じdir entryを指す`FdEntry::File`を`None`にします。
FAT32はclusterを即座に解放するため、fdを残すと再利用されたclusterへの読み書きで別fileを壊すからです。
呼び出しprocess自身のfdも失効し、以後の操作は`EBADF`になります。
`rename`は`RenameOutcome`を返し、`moved`があれば`relocate_file_fds`で開いたfdのwrite-back先を新しいdir entryへ書き換え、`replaced`があれば置き換えられたtargetのfdを失効させます。
pipe端はdir entryを指さないため、どちらの走査でも対象外です。

### pipe

pipe本体は[`kernel/src/pipe.rs`](../../kernel/src/pipe.rs)の`Pipe`で、`PIPE_CAPACITY`（256）byteのring bufferです。
`head`が次に読む位置、`len`が有効byte数で、書き込み位置は`(head + len) % PIPE_CAPACITY`から求めます。
`Pipe::write`は`free()`を上限に、`Pipe::read`は`len`と出力長の小さい方を上限に処理するため、どちらもpartialになり得ます。
`PipeTable`は`MAX_PIPES`（4）個のslotを持つだけで、参照計数を持ちません。

pipeの生死は、`ProcessTable::pipe_ends`が全processのfd tableを走査して数えたread端とwrite端の数から決まります。
closeやexitでfdが消えれば走査結果も減るため、incrementとdecrementの取りこぼしが構造的に起きず、`pipe_alloc`はlive端が0になったslotを再利用できます。
`pipe` syscallは`a0`が指す8 byteへ`[read_fd: u32, write_fd: u32]`を書き、`PIPE_OUT_LEN`（8）を返します。
[`kernel/src/main.rs`](../../kernel/src/main.rs)の`create_pipe`は、write端の割り当てに失敗するとread端を巻き戻して`EMFILE`を返します。

`ProcessTable::pipe_read`は、dataがあれば読んで待機processを起こし、空でwrite端が残っていればcallerを`BlockedOnPipe(id)`へ移します。
空でwrite端が0なら`Eof`となり、`read`は0を返します。
`pipe_write`は、read端が0なら`Broken`を返し、guestには`EPIPE`が届きます。
満杯ならcallerを`BlockedOnPipe(id)`へ移し、空きがあれば書ける分だけ書いて待機processを起こします。
長さ0の`write`は、pipeの状態に関わらず0を返します。

blockは`EAGAIN`を内部signalとして実装します。
control層が`Err(EAGAIN)`を返すと、`dispatch_read`と`dispatch_write`は`sepc`を4 byte戻して`SyscallFlow::Blocked`を返します。
processが起こされると同じ`ecall`がやり直されるため、guestに`EAGAIN`が返ることはありません。
起床の契機は、同じpipeへのdata到着と空き発生、端のclose（`close_file_fd`）、いずれかのprocessのexit（`wake_on_exit`）、stdinへのbyte到着（`wake_all_blocked`）です。
無関係な起床で条件を満たさないprocessは、再実行で再びblockします。

pipe端はseekできないため、`lseek`、`pread`、`pwrite`は`file_mut`が`None`を返した時点で`ESPIPE`になります。
read端への`write`とwrite端への`read`は`EBADF`です。

## 実行と確認

この章のguestは異常時に終了code70、正常時に42で終わり、host harnessは[`xtask/src/qemu.rs`](../../xtask/src/qemu.rs)の期待frame列との完全一致を検証します。

[`file_fd.rs`](../../guest/src/bin/file_fd.rs)は`DOCS/NOTE.TXT`を8 byteずつ読んでEOFまで進め、close後の`EBADF`と不在fileの`ENOENT`を確かめます。

```console
$ cargo xtask test file-fd
...
MiniOS payload: ok code=42
phase 1/1 passed (elapsed: 0.673s)
summary: PASSED all 1 phases (elapsed: 0.673s)
```

検査されるframeは、Ready、spawn通知、stdoutの`note inside docs`、Exit（code 42）、回収diagnosticの順です。
consoleにはframeのbinary headerも混ざるため、上の出力ではその行を省いています。

[`file_seek.rs`](../../guest/src/bin/file_seek.rs)は`pread`と`pwrite`がoffsetを動かさないこと、`lseek`の3種の基準、EOF越えのseek、`EINVAL`と`EBADF`の境界を確かめます。
[`file_stat.rs`](../../guest/src/bin/file_stat.rs)は`stat`と`fstat`のsizeとkind、`ENOENT`、`ENOTDIR`、`EFAULT`を確かめます。
[`file_readdir.rs`](../../guest/src/bin/file_readdir.rs)はrootと`DOCS`のentryをindex順に読み、末尾超過の0とerrnoを確かめます。

```console
$ cargo xtask test file-seek
$ cargo xtask test file-stat
$ cargo xtask test file-readdir
```

どれもfile-fdと同じ形の出力で`summary: PASSED all 1 phases`に至り、stdoutのframeはそれぞれ`seek verified`、`stat verified`、`readdir verified`です。

[`file_pipe.rs`](../../guest/src/bin/file_pipe.rs)はpipeを作り、`DOCS/PIPECH.ELF`をspawnしてから`pipe-bytes`を書き込み、`waitpid`でchildを待ちます。
childは継承したfd 3から読んだbyteをstdoutへ写して終了します。

```console
$ cargo xtask test file-pipe
...
MiniOS payload: ok code=42
phase 1/1 passed (elapsed: 0.943s)
summary: PASSED all 1 phases (elapsed: 0.943s)
```

期待frame列は、Ready、spawn通知、childのstdout `pipe-bytes`、childのExit、parentのstdout `pipe verified`、parentのExit、回収diagnosticです。
parentは`waitpid`でblockするため、childのframeは必ずparentより先に出ます。

## よくある失敗

- 同じfdで読みと書きを混ぜる：`open`のfdはread専用、`create`のfdはwrite専用であり、逆方向の操作は`EBADF`です。
- `pread`の後に`read`が続きから読むと期待する：`pread`と`pwrite`はfdのoffsetを動かさず、`read`は`lseek`か直前の`read`で決まった位置から進みます。
- `unlink`後もfdが使えると考える：削除したentryを指すfdは全processで失効し、呼び出し側自身の`read`や`close`も`EBADF`になります。
- pipeの`write`が常に全量を書くと考える：空きが`PIPE_CAPACITY`未満なら書けた分だけを返すため、guestは戻り値を見て残りを書き直します。
- write端を閉じ忘れたまま`read`でEOFを待つ：childへ継承したwrite端も数えるため、全processのwrite端が0になるまで`read`はblockし続けます。
- callerを`BlockedOnPipe`へ移さずに`EAGAIN`を返す：run loopの`block_on_stdin`がprocessを`BlockedOnStdin`へ移すため、`wake_pipe_waiters`では起きなくなります。

## 演習

[`kernel/src/pipe.rs`](../../kernel/src/pipe.rs)の`PIPE_CAPACITY`を8へ下げたとき、`cargo xtask test file-pipe`がどこで失敗するかを予想してください。
parentが書く`pipe-bytes\n`は11 byteであり、`Pipe::write`は`free()`を上限にpartial writeを返す点が手がかりです。
予想をtestで確かめ、guestの終了codeとframe列の差を読んだ後、256へ戻します。

[`guest/src/bin/file_fd.rs`](../../guest/src/bin/file_fd.rs)を参考に、`open`を5回続けて呼ぶguestを書き、5回目が`EMFILE`を返すことを`MAX_OPEN_FILES`から説明してください。

## 次の章

[第23章「spawn、waitpid、exec」](23-process-syscalls.md)では、この章のpipe検査でも使った`spawn`と`waitpid`を中心に、processを操作するsyscallを扱います。
fd tableのsnapshot継承と`BlockedOnPid`による待機を、process管理の側から見直します。
前の章は[第21章「FAT32の読み書き」](21-fat32.md)、全章の索引は[学習ガイド](README.md)です。
