# 23. spawn、waitpid、exec

## 学習目標

`spawn`がFAT32上のELFを読み込み、単調に採番したpidでprocess tableへ登録する流れを説明できるようになります。
`waitpid`が終了codeの台帳と`BlockedOnPid`を使い、対象の終了まで呼び出し側を止める仕組みを追います。
`exec`がpid、kernel trap stack、fd tableを保ったままimageだけを差し替える手順と、旧imageの解放を遅らせる理由を確認します。

## 背景

[第22章](22-file-descriptors-and-pipes.md)までのguestは、manifestに並べたimageをkernelが起動するだけで、guest自身は新しいprogramを始められませんでした。
shellのように別のprogramを起動して結果を待つには、process生成、終了待ち、image置き換えの三つをsystem callとしてguestへ渡す必要があります。

UNIXでは`fork`でaddress spaceを複製してから`exec`で置き換えますが、MiniOSは`fork`を持ちません。
`spawn`はpathを受け取り、そのELFから新しいaddress spaceを一から作ります。

`waitpid`は`read`と同じく、条件が満たされるまで`sepc`をecallへ戻して呼び出し側をblockし、再開時に同じsystem callをやり直す規約に従います。

## 実装

### 番号とdispatch

[`dispatch_syscall`](../../kernel/src/user/syscall.rs)は`a7`の番号（[`abi/src/syscall.rs`](../../abi/src/syscall.rs)の`SyscallNumber::Getpid`、`Spawn`、`Waitpid`、`Exec`）を`dispatch_getpid`、`dispatch_spawn`、`dispatch_waitpid`、`dispatch_exec`へ振り分けます。

`dispatch_spawn`と`dispatch_exec`は、process生成やimage差し替えの前に`copy_user_path`でpathを検証します。
長さ0か`MAX_PATH_LEN`（256）超は`EINVAL`、読めないuser rangeは`EFAULT`、UTF-8でないpathは`EINVAL`です。
検証を通ったpathだけが`ControlSource`の`spawn`や`exec`へ渡り、kernel側の実装は[`kernel/src/control.rs`](../../kernel/src/control.rs)にあります。

### spawnとpid採番

`spawn`の実装は、storage sessionの`read_file`でfile全体をheapの`Vec`へ読み込みます。
不在pathは`ENOENT`、directoryは`EISDIR`となり、`fat_errno`がFAT32のerrorをerrnoへ写します。
process名はpathのbasenameで、`Process.name`が`&'static str`を要求するため`String::leak`で確保します。

ELFからprocessを組み立てるのは[`Process::spawn`](../../kernel/src/process.rs)です。
user imageの読み込み、kernel trap stackの確保、`argv`の書き込み、初期`UserContext`の構築を順に行い、途中で失敗すれば確保済みのframeをすべて返します。
ELFとして受理できないfileは`SpawnError::Load`として`EINVAL`、frame不足などそれ以外の失敗は`ENOMEM`になります。

組み立てたprocessは[`ProcessTable::insert`](../../kernel/src/process.rs)で登録され、ここで初めてpidが決まります。
`insert`は`next_pid`を返してから1増やすだけで、終了したpidを再利用しません。
live processが`MAX_PROCS`（4）に達していると`insert`はprocessをそのまま返し、呼び出し側が`reclaim`でframeを回収してから`ENOMEM`を返します。

`getpid`は[`kernel/src/main.rs`](../../kernel/src/main.rs)の`current_pid`で、trap中のprocessのpidを返します。
単一imageのmanifestでは最初のprocessがpid 0、そこから`spawn`したchildがpid 1になります。

### fd tableの継承

`spawn_process`は、呼び出し側processの`file_fds_snapshot`を取り、childの初期fd tableとして`Process::spawn`へ渡します。
`FileFdTable`は`Copy`な固定長配列（`MAX_OPEN_FILES`は4）であり、snapshot後のoffsetやcloseは親子で独立します。
pipe端だけはcopy先も同じpipe idを指すため、bufferは親子で共有されます。

### waitpidと終了台帳

processが`exit`すると、run loopは`ProcessTable::record_exit`で`(pid, code)`を台帳へ記録し、`wake_on_exit`で待っているprocessを起こしてから、そのprocessを回収します。
台帳の上限は`MAX_PROCS`件で、超えると最古の記録を捨てます。

`waitpid`の本体は`wait_pid`です。
台帳に対象pidの記録があれば`take_exit`で消費してcodeを返し、なければ`ProcessTable::block_waitpid`へ進みます。
`block_waitpid`は自分自身と不在pidを`ECHILD`、wait連鎖が呼び出し側へ戻るcycleを`EINVAL`で拒否し、それ以外では呼び出し側を`BlockedOnPid(target)`へ移します。

blockした場合、`dispatch_waitpid`は`sepc`を4 byte戻して`SyscallFlow::Blocked`を返します。
対象が終了して`wake_on_exit`がprocessを`Runnable`へ戻すと、同じecallが再実行され、今度は台帳からcodeを得ます。
run loopは`Blocked`を受けると`block_on_stdin`を呼びますが、この関数はrunnableのときだけ遷移するため、`BlockedOnPid`は上書きされません。
MiniOSのprocessには親子関係の記録がなく、liveなpidであれば自分以外のどのprocessでも待てます。

### exec

[`Process::exec`](../../kernel/src/process.rs)は、新しいimageの読み込みと`argv`の書き込みをすべて終えてから旧imageと交換します。
交換前に失敗すれば旧imageには触れず、呼び出し側はerrnoを受けて元のprogramのまま動き続けます。
成功時はpid、kernel trap stack、fd tableを引き継ぎ、address space、`user_satp`、process名だけを新しくします。

trap handlerの実行窓ではまだ旧imageのpage tableが`satp`に載っているため、旧imageはその場では解放できません。
`exec`は旧imageを`retired_image`へ退避し、`dispatch_exec`はtrap slotの`UserContext`を新imageの初期contextで上書きして`SyscallFlow::Exec`を返します。
run loopはkernel `satp`へ戻った後に`take_retired_image`で旧imageを取り出して`destroy`し、processはrunnableのまま次のdispatchで新imageの`_start`から走ります。

## 実行と確認

guestは[`guest/src/bin/file_spawn.rs`](../../guest/src/bin/file_spawn.rs)、[`file_waitpid.rs`](../../guest/src/bin/file_waitpid.rs)、[`file_exec.rs`](../../guest/src/bin/file_exec.rs)、[`file_fdinherit.rs`](../../guest/src/bin/file_fdinherit.rs)です。
childとして起動するELFは、[`xtask/src/disk.rs`](../../xtask/src/disk.rs)がdisk imageへ書く手書きの最小ELFです。
`DOCS/CHILD.ELF`は`spawn-child`を書いてから`getpid`の値に41を足した値で`exit`し、`DOCS/FDCHILD.ELF`はfd 3から13 byteを読んでstdoutへ写します。

`spawn`と`getpid`のend-to-end実行は次のコマンドです。

```console
$ cargo xtask test file-spawn
...
MiniOS payload: ok code=42
...
summary: PASSED all 1 phases (elapsed: ...)
```

guestは`getpid`が0、`spawn("DOCS/CHILD.ELF")`が1を返すことと、不在path、directory、非ELF file、長さ0のerrnoを確かめます。
host harnessはstdoutに`spawn-child`と`spawn verified`の両方があること、Exit codeが42と42の二つであることを検証します。

`waitpid`の実行は次のコマンドです。

```console
$ cargo xtask test file-waitpid
...
MiniOS payload: ok code=42
...
summary: PASSED all 1 phases (elapsed: ...)
```

親は`waitpid(1)`でblockするため、childの`spawn-child`とExit(42)が必ず親の`waitpid verified`とExit(42)より先に出ます。
harnessはReadyからdiagnosticまでのframe列を完全一致で照合し、順序そのものをblockingの証拠にします。

`exec`の実行は次のコマンドです。

```console
$ cargo xtask test file-exec
...
MiniOS payload: ok code=41
...
summary: PASSED all 1 phases (elapsed: ...)
```

guestは`ENOENT`、`EISDIR`、`EINVAL`、null pointerの`EFAULT`、長さ0の`EINVAL`を確かめてから`DOCS/CHILD.ELF`へ`exec`します。
差し替え後のimageが`spawn-child`を出し、pid 0を保ったまま`getpid()+41`の41で終了します。
終了codeが42ではなく41であることが、`exec`がpidを引き継いだ証拠です。

fd継承の実行は次のコマンドです。

```console
$ cargo xtask test file-fdinherit
...
MiniOS payload: ok code=42
...
summary: PASSED all 1 phases (elapsed: ...)
```

親は`DOCS/NOTE.TXT`をfd 3で開き、`lseek`でoffset 4へ進めてから`DOCS/FDCHILD.ELF`を`spawn`します。
childはfd 3から` inside docs\n`の13 byteを読んでstdoutへ写し、harnessはこのframeが親の`fd-inherit verified`より先に出る列を完全一致で照合します。
親は`waitpid`の後に同じfdから同じ13 byteを読み直し、childのreadが親のoffsetを動かしていないことを確かめます。

## よくある失敗

- `insert`の前にpidを使う：`Process::spawn`が返すprocessのpidは仮の`usize::MAX`で、実際のpidは`ProcessTable::insert`が採番します。
- table満杯の`Err`でprocessを捨てる：`insert`は拒否したprocessを返すため、`reclaim`でframeを回収してから`ENOMEM`を返します。
- `waitpid`のblock時に`sepc`を戻し忘れる：再開後にecallの次の命令へ進み、`a0`に終了codeが入らないまま続行します。
- 同じpidを二回`waitpid`する：台帳の記録は`take_exit`で一回だけ消費され、二回目は`ECHILD`です。
- `exec`の中で旧imageを`destroy`する：trap窓では旧page tableが`satp`に載っているため、解放はrun loopがkernel `satp`へ戻った後に行います。
- 継承したfdのoffsetが共有されると考える：fd tableはsnapshotのcopyであり、共有されるのはpipe端のbufferだけです。

## 演習

[`kernel/src/process.rs`](../../kernel/src/process.rs)のhost testへ、`MAX_PROCS`件のprocessを`insert`した後の`insert`が`Err`でprocessを返し、空きを作った後の`insert`が以前より大きいpidを採番するtestを追加してください。
試験後は`cargo test -p minios-kernel process::tests --locked`を実行します。

[`guest/src/bin/file_exec.rs`](../../guest/src/bin/file_exec.rs)を変え、`DOCS/NOTE.TXT`を`open`してから`DOCS/FDCHILD.ELF`へ`exec`させてください。
`exec`がfd tableを引き継ぐなら、差し替え後のimageはfd 3の先頭13 byteである`note inside d`をstdoutへ出して42で終了します。
期待frame列と合わなくなるため`cargo xtask test file-exec`は失敗しますが、失敗時の診断に含まれる受信出力でこのstdout frameを確認できます。

## 次の章

このガイドの実装順はここで終わります。
今後の実装計画は[ロードマップ](../reference/roadmap.md)を、全章の索引は[学習ガイド](README.md)を参照してください。
