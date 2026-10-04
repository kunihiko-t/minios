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
process名はargvを渡せば`argv[0]`、渡さなければpathのbasenameです。
`Process.name`が`&'static str`を要求するため、どちらも`String::leak`で確保します。

ELFからprocessを組み立てるのは[`Process::spawn`](../../kernel/src/process.rs)です。
user imageの読み込み、kernel trap stackの確保、`argv`の書き込み、初期`UserContext`の構築を順に行い、途中で失敗すれば確保済みのframeをすべて返します。
ELFとして受理できないfileは`SpawnError::Load`として`EINVAL`、frame不足などそれ以外の失敗は`ENOMEM`になります。

組み立てたprocessは[`ProcessTable::insert`](../../kernel/src/process.rs)で登録され、ここで初めてpidが決まります。
`insert`は`next_pid`を返してから1増やすだけで、終了したpidを再利用しません。
live processが`MAX_PROCS`（4）に達していると`insert`はprocessをそのまま返し、呼び出し側が`reclaim`でframeを回収してから`ENOMEM`を返します。

`getpid`は[`kernel/src/main.rs`](../../kernel/src/main.rs)の`current_pid`で、trap中のprocessのpidを返します。
単一imageのmanifestでは最初のprocessがpid 0、そこから`spawn`したchildがpid 1になります。

### spawnへのargv

`spawn`は`a2`と`a3`でchildのargvも受け取ります。
`a2`は`a3`個のentryの配列を指し、各entryは`SPAWN_ARG_LEN`（16）byteの`[pointer: u64, length: u64]`です。
文字列はpathと同じくpointerと長さの組で渡すため、user memory上でNUL終端する必要はありません。

`dispatch_spawn`はpathの次に`copy_spawn_argv`を呼び、entry配列と全文字列をkernelのheapへcopyしてから`ControlSource`の`spawn`へ`&[&str]`として渡します。
上限はmanifest経路と同じ`ARG_MAX_COUNT`と`ARG_MAX_LEN`から決まり、`argc`は`SPAWN_MAX_ARGC`（17）以下、各文字列は256 byte以下です。
上限超過、NULを含む文字列、UTF-8でない文字列は`EINVAL`、読めないentry配列や文字列は`EFAULT`です。
検証もcopyもprocess生成より前に終えるため、失敗したspawnはframeもpidも消費しません。

kernel側の`spawn`は`argv[0]`を`Process::spawn`の`name`へ、残りを`arguments`へ渡します。
`Process::spawn`はmanifest経路と同じ`write_initial_argv`で初期スタックを組むため、childから見た`a0`と`a1`の形はmanifestで起動したprocessと区別できません。
`a3`が0なら`a2`は読まず、childは以前と同じくpathのbasenameだけを`argv[0]`に持ちます。
argv導入前のguestは`a2`と`a3`へ0を渡していたため、そのまま同じ結果になります。

### fd tableの継承

`spawn_process`は、呼び出し側processの`file_fds_snapshot`を取り、childの初期fd tableとして`Process::spawn`へ渡します。
`FileFdTable`はfd 0、1、2のconsole entryを含む`Copy`な固定長配列（slotは19個）であり、snapshot後のoffsetやcloseは親子で独立します。
親が`dup2`でfd 1をpipeへ差し替えてから`spawn`すれば、childの標準出力もそのpipeを指します。
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
run loopはkernel `satp`へ戻った後に`take_retired_image`で旧imageを取り出して`destroy`し、processはrunnableのまま次のdispatchで新imageのELF entryから走ります。

### user shellのpipeline

ここまでのsystem callを組み合わせると、kernelを変えずにuser modeのshellを書けます。
[`guest/src/bin/sh.rs`](../../guest/src/bin/sh.rs)は、FAT32 diskへ置く`SH.ELF`としてstdinを1行ずつ読みます。
`|`で区切った最大3個のcommandをpipelineとして起動し、最初のcommandには`< file`、最後のcommandには`> file`を付けられます。
`/`を含まないcommand名`NAME`は`BIN/NAME.ELF`（大文字）へ、`/`を含む語はそのままpathとして解決します。
built-inは`exit [code]`だけで、cwdを持たないため`cd`はありません。
起動されるtoolは[`cat.rs`](../../guest/src/bin/cat.rs)、[`wc.rs`](../../guest/src/bin/wc.rs)、[`echo.rs`](../../guest/src/bin/echo.rs)です。

MiniOSには`fork`がないため、childのfd 0と1を差し替える場所はshell自身のfd tableです。
shellは起動時にconsoleのfd 0と1を`dup2`でtable末尾の2個へ退避し、各commandを次の手順で起動します。

1. 前段のpipeのread端か`< file`のfdがあれば、`dup2`でfd 0へ付け替えます。
2. 後段があれば`pipe`を作り、write端を`dup2`でfd 1へ付け替えてから元のwrite端のfdを閉じます。
3. 最後のcommandで`> file`があれば、そのfdを`dup2`でfd 1へ付け替えます。
4. `spawn`し、childへこの時点のfd tableの複製を渡します。
5. 退避しておいたconsoleでfd 0と1を戻します。この`dup2`がshellの持っていたpipeのwrite端を閉じるため、write端を持つのは書き手のchildだけになります。

pipeのEOFは全processのfd tableからwrite端が消えたときに決まるため、手順2と5でshellのwrite端を残さないことが必須です。
一つでも残っていれば、読み手のchildは書き手が終了してもEOFを得られず、shellの`waitpid`とともに止まります。
なお、各pipeのread端はその書き手のchildにも複製されますが、読み手のEOFには影響しません。

全員を起動してから、shellは起動順にpidを`waitpid`し、最後のcommandの終了codeを覚えます。
前段が後段より先に終わるとは限りませんが、終了したprocessの記録は台帳に残るため、順に待てば全員を回収できます。
pipelineの段数はshellを含めて`MAX_PROCS`（4）に収まる3個までです。
`spawn`の失敗は、不在pathの`ENOENT`を`not found`、table満杯やframe不足の`ENOMEM`をprocess枠かmemoryの不足として標準エラーへ1行出し、次の行へ進みます。
2段目以降で失敗した場合、前段は読み手のいないpipeへ書き続けてbufferが満杯になると止まるため、shellは前段のpipeをEOFまで読み捨ててから待ちます。

## 実行と確認

guestは[`guest/src/bin/file_spawn.rs`](../../guest/src/bin/file_spawn.rs)、[`file_waitpid.rs`](../../guest/src/bin/file_waitpid.rs)、[`file_exec.rs`](../../guest/src/bin/file_exec.rs)、[`file_fdinherit.rs`](../../guest/src/bin/file_fdinherit.rs)です。
これらのguestがchildとして起動するELFは、[`xtask/src/disk.rs`](../../xtask/src/disk.rs)がdisk imageへ書く手書きの最小ELFです。
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

argvの受け渡しは次のコマンドで確かめます。

```console
$ cargo xtask test spawn-args
...
MiniOS payload: ok code=42
...
summary: PASSED all 1 phases (elapsed: ...)
```

guestは[`guest/src/bin/spawn_args.rs`](../../guest/src/bin/spawn_args.rs)です。
childの`DOCS/ECHO.ELF`は手書きではなく、argvを順にstdoutへ書く`minios-guest`をbuildしたELFで、harnessがdisk imageの連続clusterへ置きます。
親はまず`argc`の上限超過と長すぎる文字列の`EINVAL`、null pointerのentry配列と読めない文字列の`EFAULT`を確かめます。
続いて`argc`が0のspawnでchildが`ECHO.ELF`だけを書くこと、`echoargs`、`alpha`、`beta gamma`を渡したspawnでその3個が順に届くことを示します。
失敗したspawnがpidを消費していれば2個のchildはpid 1と2にならず、親は70で終了します。
harnessはframe列を完全一致で照合し、最後のdiagnosticで全frameの回収も確かめます。

user shellは次のコマンドで確かめます。

```console
$ cargo xtask test user-shell
...
MiniOS payload: ok code=0
...
summary: PASSED all 1 phases (elapsed: ...)
```

harnessは`SH.ELF`をboot payloadにし、build済みの`cat`、`wc`、`echo`をdisk imageの`BIN`へ置いてから、次のscriptをStdin frameで送り、最後にEOFを送ります。

```text
echo hello world
cat HELLO.TXT
cat HELLO.TXT | wc
echo redirected > OUT.TXT
cat OUT.TXT
wc < HELLO.TXT
echo a b c | cat | wc
nosuch
```

標準出力には`hello world`、`hello from virtio`、`1 3 18`、`redirected`、`1 3 18`、`1 3 6`が順に届き、標準エラーには`sh: nosuch: not found (errno -2)`が届きます。
pipelineでconsoleへ書くのは最後のcommandだけで、そのcommandは前段が全員終了してwrite端が消えた後にEOFを読んでから出力します。
このため前段のExit frameは必ず最後のcommandの出力より前に並び、harnessはframe列を完全一致で照合できます。
EOFを読んだshellは0で終了します。

`cargo xtask run`は従来どおりkernel shellを起動します。
user shellを手元で試すには、`cargo xtask test user-shell`のscriptを変えて期待frameとの差を見るのが手軽です。

## よくある失敗

- `insert`の前にpidを使う：`Process::spawn`が返すprocessのpidは仮の`usize::MAX`で、実際のpidは`ProcessTable::insert`が採番します。
- table満杯の`Err`でprocessを捨てる：`insert`は拒否したprocessを返すため、`reclaim`でframeを回収してから`ENOMEM`を返します。
- `waitpid`のblock時に`sepc`を戻し忘れる：再開後にecallの次の命令へ進み、`a0`に終了codeが入らないまま続行します。
- 同じpidを二回`waitpid`する：台帳の記録は`take_exit`で一回だけ消費され、二回目は`ECHILD`です。
- `exec`の中で旧imageを`destroy`する：trap窓では旧page tableが`satp`に載っているため、解放はrun loopがkernel `satp`へ戻った後に行います。
- pipelineでshellのwrite端を残す：`spawn`の後にfd 1をconsoleへ戻さないと、shellがwrite端を持ち続けるため、読み手のchildはEOFを得られません。
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
