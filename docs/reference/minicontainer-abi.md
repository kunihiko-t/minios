# MiniContainer Guest ABI

この文書は、MiniOSとMiniContainerが共有するMiniContainer Guest ABI v1を定義します。
整数は、特記しない限りlittle-endianで表現します。
このABIの公開型と定数は`minios-abi` crateにあります。
現行のminorは2であり、1.0と1.1との互換規約は末尾の互換性規約にまとめます。

## MiniBundle v1

MiniBundle v1は、96バイトの固定header、UTF-8 manifest、8バイト境界までのゼロpadding、静的ELFの順に並ぶ単一ファイルです。
bundle全体の長さは6 MiB以下です。
Rustの構造体をそのままメモリーへ配置せず、各fieldを次のoffsetから読んでください。

| offset | size | field | v1の値と意味 |
| ---: | ---: | --- | --- |
| 0 | 8 | `magic` | ASCII `MINICTR`と末尾NUL |
| 8 | 2 | `abi_major` | `1` |
| 10 | 2 | `abi_minor` | `2`。decoderは自身以下のminorを受理する |
| 12 | 2 | `header_len` | `96` |
| 14 | 2 | `flags` | `0` |
| 16 | 8 | `total_len` | headerを含むbundle全体の長さ |
| 24 | 8 | `manifest_offset` | `96` |
| 32 | 8 | `manifest_len` | manifestのバイト長 |
| 40 | 8 | `elf_offset` | manifest末尾を8バイト境界へ切り上げたoffset |
| 48 | 8 | `elf_len` | ELFのバイト長 |
| 56 | 32 | `digest` | SHA-256 digest |
| 88 | 8 | `reserved` | すべて`0` |

`manifest_offset`は必ず`96`です。
`elf_offset`は8の倍数で、`manifest_offset + manifest_len`を8バイト境界へ切り上げた値と一致します。
manifestの終端は`elf_offset`を超えず、`elf_offset + elf_len`は桁あふれせず`total_len`と一致します。
`flags`と`reserved`に非ゼロの値を入れたheaderは受理しません。

digestは、headerの`digest` fieldだけをゼロにした96バイトと、offset 96から`total_len`までのすべてのバイトを順に入力したSHA-256です。
digestはheader、manifest、padding、ELFをまとめて識別します。
digestは破損検出とcontent addressingに使い、署名や配布元の認証は提供しません。

## Manifest v1

manifestは最大4 KiBのUTF-8テキストです。
次のgrammarに従い、末尾はLFで終えます。

```text
manifest     = "version=1" LF "name=" name LF { "arg=" argument LF }
name         = 1*128(name-char)
name-char    = ALPHA / DIGIT / "." / "_" / "-"
argument     = 0*256(argument-char)
argument-char = UTF-8文字列中のNUL、CR、LF以外のバイト
```

`argument`はNUL、CR、LFを含められません。
`name`はCRを含められません。
`arg=`は0個から16個まで置けます。
未知のkey、順序違反、重複する`version`または`name`、空の`name`、末尾LFの欠落は受理しません。

```text
version=1
name=hello.world_1-test
arg=first
arg=second
```

### Manifest v2

複数のELF imageを1つのbundleへ格納するmanifestです。
`header.elf`領域はすべてのimageのELFを隙間なく連結したもので、各imageは`elf=`行でその領域内の相対rangeを宣言します。

```text
manifest     = "version=2" LF 1*4section
section      = "image=" name LF { "arg=" argument LF } "elf=" range LF
range        = offset "," length    ; いずれも10進数。offsetはheader.elf先頭からの相対
```

`arg=`はimageごとに0個から16個まで置け、`elf=`の前にだけ置けます。
`elf=`は各sectionにちょうど1行必要で、`length`が0のrange、`offset + length`の桁あふれ、image間で重なり合うrange、`header.elf`領域からはみ出すrangeは受理しません。
imageは4個までで、`version=1`のmanifestは`header.elf`領域全体を占める単一imageとして扱います。

```text
version=2
image=spin
arg=slow
elf=0,4096
image=cat
elf=4096,2048
```

## UART Control ABI v1

OpenSBIとMiniOSの起動診断は、control protocol開始前のテキストとして扱います。
MiniOSが同期magicを送った後、UARTは長さ付きbinary frameとしてdecodeします。

各frameは12バイトのheaderとpayloadから構成します。

| offset | size | field | v1の値と意味 |
| ---: | ---: | --- | --- |
| 0 | 4 | `magic` | ASCII `MCF1` |
| 4 | 1 | `kind` | 下表のframe種別 |
| 5 | 1 | `flags` | `0` |
| 6 | 2 | `reserved` | `0` |
| 8 | 4 | `payload_len` | payloadのバイト長。最大64 KiB |

| 値 | kind | payload | 方向 |
| ---: | --- | --- | --- |
| 1 | `READY` | 4バイト。ABI majorとABI minor | guest→host |
| 2 | `STDOUT` | 任意のバイト列 | guest→host |
| 3 | `STDERR` | 任意のバイト列 | guest→host |
| 4 | `EXIT` | 4バイトの符号なし終了コード | guest→host |
| 5 | `GUEST_ERROR` | UTF-8診断 | guest→host |
| 6 | `DIAGNOSTIC` | UTF-8診断 | guest→host |
| 7 | `STDIN` | 入力バイト列。長さ0はEOF | host→guest |
| 8 | `PROC_EXIT` | 8バイト。process idと終了コード | guest→host |

`READY`と`EXIT`の`payload_len`は必ず4で、`PROC_EXIT`の`payload_len`は必ず8です。
`STDIN`の`payload_len`は4 KiB以下であり、長さ0のframeがEOFを表します。
EOFはstickyであり、EOF以後の`STDIN` frameが届いても入力は戻りません。

`READY` payloadの4バイトは、次の順序で符号なし整数を格納します。

| offset | size | field | encoding |
| ---: | ---: | --- | --- |
| 0 | 2 | `abi_major` | `u16` little-endian |
| 2 | 2 | `abi_minor` | `u16` little-endian |

したがって、ABI 1.2の`READY` payloadは`01 00 02 00`です。
ホストは`READY`のminorが1以上のときだけ`STDIN` frameを送ります。

`PROC_EXIT` payloadの8バイトは、次の順序で符号なし整数を格納します。
`pid`はkernelがprocessごとに採番する単調増加の識別子（0始まり・再利用なし）で、boot時のspawn列ではmanifest内のimage indexと一致します。複数image bundleでは各processの終了ごとに1 frameを送ります。

| offset | size | field | encoding |
| ---: | ---: | --- | --- |
| 0 | 4 | `pid` | `u32` little-endian |
| 4 | 4 | `code` | `u32` little-endian |

未定義の`kind`、非ゼロの`flags`または`reserved`、上限を超える長さ、固定長payloadの不一致は受理しません。
同期後にheaderまたはpayload長の規約が壊れたとき、ホストはbyte streamを推測で再同期せず、protocol failureとしてinstanceを停止します。

## Stdin転送

stdinは疑似TTYなしのbyte streamであり、ホストが`STDIN` frameで順に送り、guestが`read`で引きます。
kernelはguestの`read`要求時にだけUARTから次のframeを読むpull型であり、要求がなければ受信しません。
frame境界と`read`要求長は一致しなくてよく、frame内の残りは次の`read`へ繰り越します。

上限とbackpressureは次のとおりです。

- 1 frameは4 KiB以下です。これを超える`STDIN`はprotocol failureとして実行を停止します。
- kernelが保持するのは未配達の1 frame分（最大4 KiB）だけです。未読のbyteはホストとUARTのbufferに残ります。
- guestの`read`はbyteまたはEOFが届くまで待機します。待機中もtimer割り込みは処理します。
- guest終了後に届いた入力は読みません。shutdownとともに破棄します。

## Syscall ABI v1

syscall番号は`a7`、引数は`a0..a5`、戻り値は`a0`へ置きます。

| 番号 | 呼び出し | 引数 | 規約 |
| ---: | --- | --- | --- |
| 1 | `write` | `a0=fd`、`a1=pointer`、`a2=length` | fdが指すentryへ書く。console出力（新processのfd 1は標準出力、fd 2は標準エラー出力）、`create`したfile、pipeのwrite端を受理する。出力は一回につき4 KiB以下 |
| 2 | `exit` | `a0=code` | 下位8ビットをアプリケーション終了コードとしてホストへ渡し、アプリケーションへ戻らない |
| 3 | `read` | `a0=fd`、`a1=pointer`、`a2=length` | fdが指すentryから読む。console入力（新processのfd 0）、`open`したfile、pipeのread端を受理する。入力は一回につき4 KiB以下。戻り値はbyte数、EOFは0 |
| 4 | `read_file` | `a0=path pointer`、`a1=path length`、`a2=buffer pointer`、`a3=buffer length` | `path`はUTF-8のFAT32パスで256 byte以下。戻り値は読んだbyte数。fileがbufferより長い場合は先頭で打ち切る |
| 5 | `open` | `a0=path pointer`、`a1=path length` | `path`はUTF-8のFAT32パスで256 byte以下。戻り値は3以上のread-only file descriptorか負のerrno。processごとにfd 0、1、2とは別に最大16個まで開ける |
| 6 | `close` | `a0=fd` | fd 0、1、2を含むfdのslotを空ける。戻り値は0か負のerrno |
| 7 | `create` | `a0=path pointer`、`a1=path length` | `open`と同じpath規約で、fileが無ければ作成し、writableなfile descriptorを返す。最終要素は8.3名へ正規化できる名前のみ受理する |
| 8 | `unlink` | `a0=path pointer`、`a1=path length` | `open`と同じpath規約で、fileを削除する。戻り値は0か負のerrno |
| 9 | `lseek` | `a0=fd`、`a1=offset（符号付き）`、`a2=whence` | fdのoffsetを`whence`（0=file先頭、1=現在offset、2=file末尾）基準の`offset`へ更新する。戻り値は新しいoffsetか負のerrno |
| 10 | `pread` | `a0=fd`、`a1=pointer`、`a2=length`、`a3=offset` | fileの`offset` byte目から読む。fdが保持するoffsetは動かさない。戻り値はbyte数、EOFは0 |
| 11 | `pwrite` | `a0=fd`、`a1=pointer`、`a2=length`、`a3=offset` | fileの`offset` byte目へ書く。fdが保持するoffsetは動かさない。戻り値は書いたbyte数 |
| 12 | `rename` | `a0=old pointer`、`a1=old length`、`a2=new pointer`、`a3=new length` | fileまたはdirectoryを別のpathへ移す（同一directory内では改名）。両pathはUTF-8のFAT32パスで256 byte以下。戻り値は0か負のerrno |
| 13 | `mkdir` | `a0=path pointer`、`a1=path length` | `open`と同じpath規約で、directoryを作成する。戻り値は0か負のerrno |
| 14 | `rmdir` | `a0=path pointer`、`a1=path length` | `open`と同じpath規約で、空のdirectoryを削除する。戻り値は0か負のerrno |
| 15 | `getpid` | なし | 呼び出したprocessのpidを返す |
| 16 | `spawn` | `a0=path pointer`、`a1=path length`、`a2=argv pointer`、`a3=argc` | `open`と同じpath規約で、pathのELF fileを新しいprocessとして起動する。`a3`が1以上なら`a2`の`argc`個のentryがchildのargvになり、0なら`a2`は読まずargvはpathのbasename 1個になる。戻り値はchildのpidか負のerrno |
| 17 | `waitpid` | `a0=pid` | `pid`のprocessの終了codeを返す。対象がliveなら呼び出しprocessをblockし、対象の終了後に同じecallが再実行されてcodeを返す。負のerrnoは`ECHILD`/`EINVAL` |
| 18 | `stat` | `a0=path pointer`、`a1=path length`、`a2=out pointer` | `open`と同じpath規約で、fileまたはdirectoryのmetadataを`a2`のuser bufferへ8 byteの`Stat`として書き込む。戻り値は`STAT_LEN`（8）か負のerrno |
| 19 | `fstat` | `a0=fd`、`a1=out pointer` | fdが指すfileのmetadataを`a1`のuser bufferへ8 byteの`Stat`として書き込む。戻り値は`STAT_LEN`（8）か負のerrno |
| 20 | `readdir` | `a0=path pointer`、`a1=path length`、`a2=index`、`a3=out pointer` | `path`が指すdirectoryの`index`番目のentryを`a3`のuser bufferへ263 byteの`DirEnt`として書き込む。`a1`=0はroot directoryを指す。indexが末尾を越えれば0を返し、それ以外の成功は`DIRENT_LEN`（263）を返す。負のerrnoは`ENOENT`/`ENOTDIR`/`EINVAL`/`EFAULT` |
| 21 | `exec` | `a0=path pointer`、`a1=path length` | `path`のfileをELFとして読み込み、呼び出しprocessのimageを置き替える。pidとfd tableは引き継ぐ。成功時は戻らず新imageのentryから始まる。負のerrnoは`ENOENT`/`EISDIR`/`EINVAL`/`EFAULT`/`ENOMEM` |
| 22 | `pipe` | `a0=out pointer` | kernel管理のbyte channelを1本作成し、read端とwrite端のfdを`a0`のuser bufferへ8 byte（2個の`u32`）で書き込む。戻り値は`PIPE_OUT_LEN`（8）か負のerrno |
| 23 | `sbrk` | `a0=increment（符号付き）` | 呼び出しprocessのheap breakを`increment` byte進め、旧breakを返す。0は現在のbreakを返す。負のerrnoは`EINVAL`/`ENOMEM` |
| 24 | `clock` | なし | boot以降の経過時間をmillisecondの非負整数で返す。分解能はtimer tickの10 ms |
| 25 | `sleep` | `a0=milliseconds` | 少なくとも`a0`のmillisecondが経過してから0を返す。待つ間は他processが走る。0は`yield`と同じ |
| 26 | `yield` | なし | 残りのtime sliceを手放して次のrunnable processへ順番を回し、再び選ばれると0を返す |
| 27 | `dup2` | `a0=oldfd`、`a1=newfd` | `newfd`を閉じてから`oldfd`と同じentryを指させ、`newfd`を返す。`oldfd == newfd`は何も変えずに`newfd`を返す。負のerrnoは`EBADF` |

`write`は、対象範囲がユーザー空間の読み取り可能ページにすべて含まれることを要求します。
`read`は、対象範囲がユーザー空間の書き込み可能ページにすべて含まれることを要求し、範囲の検証を通ってから入力を消費します。
長さ0の`read`は入力へ触れず0を返します。
`read_file`は、path範囲が読み取り可能でbuffer範囲が書き込み可能であることを要求し、両方の検証を通ってからstorageへ触れます。
長さ0の`read_file`は0を返します。`read_file`は呼び出しのたびにfile全体を先頭から読む1回限りの操作であり、offsetやopen中のhandleは持ちません。
processごとのfd tableはfd 0から18までの19個のslotを持ちます（`FIRST_FILE_FD + MAX_OPEN_FILES`個）。
新processはfd 0にconsole入力、fd 1に標準出力、fd 2に標準エラー出力を指すconsole entryを持って始まり、残りは空です。
fd 0、1、2は特別扱いではなく普通のslotであり、`read`と`write`はfd番号ではなくslotが指すentryの種類で経路を選びます。
そのため`dup2`でfd 1をpipeのwrite端へ差し替えれば、以後のfd 1への`write`はpipeへ流れます。
console入力のentryへの`write`とconsole出力のentryへの`read`は、pipeの方向規約と同じく`EBADF`です。
`open`は`read_file`と同じpath規約で、成功するとfileをread-onlyの`fd`へ結び付けます。
`open`、`create`、`pipe`は、fd 0、1、2が空いていても使わず、`FIRST_FILE_FD`（3）以上で最小の空きslotを割り当てます。
`create`はfileが存在しなければ8.3名のentryを親directoryへ追加し、存在すればそのfileをwritableに開きます。作成名は小文字を大文字化した8.3形だけを受理し、長い名前や8.3へ写像できない要素は`EINVAL`です。
file fdへの`read`と`write`は現在のoffsetから行い、処理した分だけoffsetを進めます。
fdはread専用（`open`由来）またはwrite専用（`create`由来）のどちらかであり、writable fdへの`read`とread-only fdへの`write`は`EBADF`を返します。
`write`のoffsetがfile sizeを越える場合は穴あき書き込みになるため`EINVAL`を返します。size以内なら上書きと末尾への追記ができます。
`lseek`はfile末尾を越えるoffsetも受理します。その位置からの`read`はEOFとして0を返し、`write`は`EINVAL`です。
新しいoffsetが負になる指定と0..=2以外のwhenceは`EINVAL`、console entryへの`lseek`/`pread`/`pwrite`はpipe端と同じく`ESPIPE`を返します。
`pread`と`pwrite`は呼び出しごとに明示したoffsetだけを使い、fdが保持するoffsetを読みも書きもしません。
方向性の規約は`read`/`write`と同じで、writable fdへの`pread`とread-only fdへの`pwrite`は`EBADF`を返します。
`pwrite`のoffsetがfile sizeを越える場合も`write`と同じく`EINVAL`です。
`close`はfdのslotを空けます。fd 0、1、2も閉じられ、閉じた後の`read`/`write`は`EBADF`です。以前の版は標準streamへの`close`を`EBADF`で拒否していましたが、fd 1をpipeやfileへ差し替える用途のため普通のslotと同じ扱いへ変えました。processは終了時に残ったfdを自動的に閉じます。
`dup2`は`oldfd`のentryを`newfd`へ複製します。`oldfd`が空いている場合と、どちらかがfd table（0から`FD_TABLE_LEN - 1`、現在は18）の範囲外の場合は`EBADF`で、何も変えません。`oldfd == newfd`は開いていれば何もせず`newfd`を返します。
それ以外では、まず`newfd`を`close`と同じく閉じます。閉じたのがpipe端なら、そのpipeを待つprocessをwakeしてEOFや`EPIPE`を再判定させます。
複製したpipe端は独立したlive端として数えるため、EOFと`EPIPE`は同じpipeを指す端がすべて閉じてから起きます。
file entryは位置記述子とoffsetごとcopyするため、POSIXと異なり`dup2`後の2個のfdはoffsetを共有しません。片方の`read`/`write`/`lseek`はもう片方のoffsetを動かしません。`unlink`による失効と`rename`による追従は、複製したfdにも同じく働きます。
`unlink`はdir entryとそれに続く長い名前のrecord列を削除し、fileのcluster chainを解放します。directoryには使えず、`EISDIR`を返します。
POSIXと異なり、削除したentryを指すfdはどのprocessのものも即座に失効し、以後の`read`/`write`/`close`は`EBADF`を返します。これはclusterを即座に解放するために必要な規約です。
`rename`はfileまたはdirectoryを別のpathへ移します。同一directory内では名前の変更、別directoryを指定した場合は移動になります。新しい名前は`create`と同じく8.3へ正規化できる名前のみ受理します。
同一directory内の改名ではentryのcluster chainと内容、dir entryの物理位置は変わらないため、fileを開いているfdは有効なままです。別directoryへの移動ではdir entryが新しい親の中の別の物理位置へ移りますが、kernelが開いているfdのwrite-back先を新しい位置へ追従させるため、fdは有効のままです。同名へのrenameは成功のno-opです。
新しい名前が既存のfileと一致する場合はPOSIXと同じく置き換えで、消えたfileを指すfdは`unlink`と同じく即座に失効します。既存のtargetがdirectoryの場合は`EISDIR`を返します。
directoryの移動も可能で、移したdirectoryの`..`は新しい親のcluster番号（root親の場合は0）へ更新されます。sourceがdirectoryでtargetがfileの場合は`ENOTDIR`、targetが空でないdirectoryの場合は`ENOTEMPTY`を返します。空のdirectoryへのrenameは置き換えとなり、targetのentryとcluster chainが解放されます。
directoryを自身またはその子孫directoryの中へ移す指定は`EINVAL`を返します。
`mkdir`は`create`と同じ名規約でdirectoryを作成し、`.`と`..`のentryを持つclusterを割り当てます。同名のentryが既にある場合は`EEXIST`を返します。
`rmdir`は`.`と`..`以外のentryを持たないdirectoryを削除し、そのcluster chainを解放します。対象がfileの場合は`ENOTDIR`、空でない場合は`ENOTEMPTY`を返します。root directoryは削除できません。
directoryはfdを持たないため、`mkdir`と`rmdir`が失効させるfdはありません。
`getpid`は呼び出したprocessのpidを返します。pidはprocess tableが採番する単調な識別子で、manifest宣言順のimageは0から始まり、`spawn`で起動したprocessは以後の番号を受け取ります。
`spawn`は`path`のfileをELF executableとして読み込み、新しいprocessを生成してschedulerへ登録し、childのpidを返します。childは呼び出し側と独立してscheduleされ、親が終了しても残り続けます。
childは呼び出し側のfd tableのsnapshotをfd 0、1、2も含めて引き継ぎます。同じfd番号が同じfileを指し、spawn時点のoffsetを引き継ぎますが、継承はcopyなのでその後のseekやcloseは互いに影響しません。`dup2`でfd 1をpipeへ向けてから`spawn`すれば、childの標準出力はそのpipeへ流れます。manifestから起動するprocessはconsole entryだけを持つtableで開始します。
`spawn`の`a2`は`argc`個のentryの配列を指し、各entryは`SPAWN_ARG_LEN`（16）byteの`[pointer: u64, length: u64]`です。
文字列はpathと同じくpointerと長さで渡し、user memory上でNUL終端する必要はありません。
childは初期スタックABIのとおり`a0=argc`、`a1=argv`で起動し、argvはNUL終端した文字列として渡した順に並びます。
`argv[0]`も呼び出し側が渡した文字列そのままで、慣習としてprogram名を置きます。
上限はmanifest経路のchildと同じで、`argc`は`SPAWN_MAX_ARGC`（`ARG_MAX_COUNT + 1`、現在は17）以下、各文字列は`ARG_MAX_LEN`（256）byte以下です。
上限を超える`argc`や文字列、NULを含む文字列、UTF-8でない文字列は`EINVAL`、entry配列か文字列が読み取り可能なユーザーページに収まらない場合は`EFAULT`です。
kernelはprocessを作る前にentry配列と全文字列をkernel内へcopyして検証するため、これらの失敗ではprocessもpidも消費されません。
`argc`が0の場合は`a2`を読まず、childは従来どおりpathのbasename（`DOCS/CHILD.ELF`なら`CHILD.ELF`）だけを`argv[0]`に持つ`argc=1`で起動します。
argvを導入する前の呼び出し側は`a2`と`a3`に0を渡していたため、この規約で挙動は変わりません。
`spawn`が失敗した場合、途中まで確保したframe・address space・imageはすべて解放され、新しいprocessは登録されません。pathがfileを指さない（`ENOENT`）、directoryを指す（`EISDIR`）、ELFとして受理できない（`EINVAL`）、process tableが満杯または資源が足りない（`ENOMEM`）場合がerrnoです。
`waitpid`は`a0`のpidを持つprocessの終了codeを返します。対象が既に終了していればkernelが保持する終了codeを1回だけ消費して返し（reap）、liveなら呼び出しprocessを対象の終了までblockします。`read`と同じく、block中は`sepc`がecallへ戻されるため、wake後の再実行でcodeを返します。
対象が自分自身・存在しない・既にreap済み・異常終了でstatusを持たない場合は`ECHILD`、wait連鎖が呼び出し側へ戻るcycleは`EINVAL`を返します。複数のprocessが同じpidを待つこともでき、終了時に全員がwakeしますが、codeを回収できるのは先に再実行された1つだけで、残りは`ECHILD`を受け取ります。kernelは終了codeをprocess数上限分だけ台帳へ保持し、超過分は最古からdropします。

`stat`と`fstat`はfileのmetadataを`Stat`構造としてuser bufferへ書き込みます。`Stat`はlittle-endianの8 byteで、先頭4 byteが`size`（fileのbyte数。directoryはFAT32の規約で0）、残り4 byteが`kind`（`STAT_KIND_FILE` = 0、`STAT_KIND_DIR` = 1、`STAT_KIND_PIPE` = 2、`STAT_KIND_CONSOLE` = 3）です。`stat`はfileとdirectoryの両方を受理し、`fstat`はfile、pipe、consoleのentryを受理して未割り当てfdに`EBADF`を返します。pipeとconsoleのentryの`size`は常に0です。
どちらも`read`と同じく、out pointerが指す8 byteがユーザー空間の書き込み可能ページにすべて含まれることを要求し、検証を通ってからstorageやfd tableへ触れます。成功時の戻り値は`read`系のbyte数規約に従う`STAT_LEN`（8）です。

`readdir`はdirectoryの中身をindex順に1件ずつ返します。`DirEnt`は263 byteで、先頭4 byteが`name_len`、次の4 byteが`kind`（`STAT_KIND_*`と同じ値）、残り255 byteがゼロ詰めの`name`です。`readdir`は呼び出しごとにdirectoryを先頭から走査して`index`番目のentryを返すため、一覧はindex 0からの連続呼び出しで得ます。`.`と`..`は列挙に含まれず、末尾を越えたindexは0を返します。空path（`a1`=0）はroot directoryを指し、fileへの指定は`ENOTDIR`を返します。out pointerの検証は`stat`と同じく、走査より先に`EFAULT`を確定します。

`exec`は呼び出したprocessのimageを`path`のELFで置き替えます。pidとfd tableは引き継ぎ、address spaceと実行contextだけが新しくなります。新imageの構築がすべて成功してから差し替えるため、失敗した`exec`はerrnoを返して旧imageのまま動き続けます。成功時は`a0`の戻り値ではなく新imageの`_start`へ入り、引数は空です。

`pipe`はkernelが所有する256 byteのring bufferを1本作り、read端とwrite端のfdを`out`が指す8 byteへlittle-endianの`u32`二つ（先にread端）で書き込みます。fdの割り当てより先に`out`の書き込み可能性を検査するため、失敗時にfdは残りません。pipeの両端はfile fdと同じtableのslotを使うため、`spawn`のsnapshot継承で同じpipeを指す端が子へ渡り、process間のbyteの流れ道になります。
pipeのread端への`read`は、bufferにdataがあれば読んでbyte数を返し、空でwrite端が残っていれば呼び出しprocessを`BlockedOnPipe`へ移してecallを再実行へ回します（`read`のstdin待ちと同じ巻き戻し規約）。write端がすべて閉じていればEOFとして0を返します。write端への`write`は、read端がすべて閉じていれば`EPIPE`、bufferが満杯なら同じくblockして再実行、空きがあれば書いてbyte数を返します。片端への逆方向操作（read端への`write`、write端への`read`）は`EBADF`、pipe fdへの`lseek`/`pread`/`pwrite`は`ESPIPE`を返します。
`fstat`はpipe fdへ`kind`=`STAT_KIND_PIPE`（2）・`size`=0を返します。processが終了または`close`で端を手放すと、そのpipeを待つprocessは全員wakeされ、再実行した`read`/`write`が新しい端数（EOFや`EPIPE`）を見ます。両端ともどのprocessのfd tableからも消えたpipeのslotは再利用されます。同時にliveなpipeは4本までで、枯渇は`ENOMEM`を返します。

`sbrk`はprocessのheap breakを動かします。
初期breakはimageの最上位PT_LOAD segment末尾をpage境界へ切り上げた位置で、heapはそこからuser stack下のguard pageへ向けて上へ伸びます。
新しく必要になったpageはzero済みのユーザー読み書き可能ページ（実行不可）としてmapし、戻り値は伸ばす前のbreakです。
image page数とheap page数の合計が2,048を超える要求や、breakがguard pageの先頭を越える要求は`ENOMEM`を返し、breakを動かしません。
frameの確保が途中で尽きた場合も`ENOMEM`を返してbreakを動かしませんが、map済みのpageはprocessが所有したまま残り、次の`sbrk`が再利用します。
縮小は未対応で、負のincrementは`EINVAL`です。
breakはimageに属するため、`exec`は新imageの初期breakから始まり、`spawn`したchildも自身のimageの初期breakから始まります。

`clock`は100 Hzのtimer tick数をmillisecondへ換算した値を返すため、値は10 ms刻みで増えます。
`sleep`はmillisecondをtick数へ切り上げ、途中まで経過した現在のtickの分として1 tickを足した起床tickまで呼び出しprocessを`BlockedUntil`へ移します。
そのため`sleep`の前後に読んだ`clock`の差は、必ず指定値以上になります。
`sleep`は戻り値0を書いてecallの次へ進めてからblockするため、`read`や`waitpid`と違ってecallは再実行されず、stdinの到着でも早く起きません。
`yield`と`sleep(0)`はprocessをblockせず、timer割り込みと同じ経路で次のprocessを選ばせます。
runnableなprocessがほかになければ、呼び出し側がすぐに再び選ばれます。
負のABI error値は次のとおりです。

| 値 | 名前 | 条件 |
| ---: | --- | --- |
| `-2` | `ENOENT` | fileが存在しない |
| `-5` | `EIO` | storageの読み取りまたはfilesystem構造の失敗 |
| `-9` | `EBADF` | fd tableの範囲外のfile descriptor、未割り当てfdへの`read`/`write`/`pread`/`pwrite`/`lseek`/`close`/`fstat`/`dup2`、範囲外の`newfd`への`dup2`、console入力への`write`とconsole出力への`read`、writable fdへの`read`/`pread`、read-only fdへの`write`/`pwrite`、pipeのread端への`write`とwrite端への`read`、`unlink`や`rename`の置き換えで失効したfdへの操作 |
| `-10` | `ECHILD` | `waitpid`の対象が自分自身・存在しない・reap済み・異常終了でstatusを持たない |
| `-11` | `EAGAIN` | pipeの`read`/`write`が条件未達でcallerをblockへ移したことを示すkernel内部のsignal。dispatchがecallを巻き戻して再実行するためguestへは返らない |
| `-12` | `ENOMEM` | kernelがstorage用のframeを確保できない、`spawn`のprocess table満杯や資源不足、同時にliveなpipe本数（4本）の枯渇、`sbrk`の上限超過やframe不足 |
| `-14` | `EFAULT` | 不正なpointerまたは権限不足の範囲 |
| `-17` | `EEXIST` | `mkdir`の対象と同名のentryが既にある |
| `-19` | `ENODEV` | block deviceが見つからない |
| `-20` | `ENOTDIR` | パス途中の要素がfileである、`rmdir`の対象がfileである、`rename`でdirectoryをfileへ改名しようとした |
| `-21` | `EISDIR` | `read_file`や`create`、`unlink`、`spawn`の対象がdirectoryである |
| `-22` | `EINVAL` | 4 KiBを超える入出力長、256 byteを超えるpath、UTF-8でないpath、無効なパス要素、8.3へ正規化できない作成名、file sizeを越えるwrite offset、負になる`lseek`結果や未知のwhence、directoryを自身または子孫の中へ移す`rename`、`spawn`の対象がELFとして受理できない、`spawn`の`argc`の上限超過、上限を超えるかNULを含むかUTF-8でない`spawn`のargv文字列、`waitpid`のwait連鎖が呼び出し側へ戻るcycle、`sbrk`の負のincrement |
| `-24` | `EMFILE` | processの同時open数（fd 0、1、2とは別に16個）を超えた。pipeの両端もこのtableのslotを使う |
| `-28` | `ENOSPC` | freeなclusterやdirectory entryが残っていない |
| `-29` | `ESPIPE` | pipe fdとconsole entryへの`lseek`/`pread`/`pwrite` |
| `-32` | `EPIPE` | read端がすべて閉じたpipeへの`write` |
| `-38` | `ENOSYS` | 未知のsyscall番号、storageを持たない経路でのfile操作、process contextを持たない経路での`getpid`/`spawn`/`waitpid`/`fstat`/`pipe`/`sbrk`/`sleep`/`dup2` |
| `-39` | `ENOTEMPTY` | `rmdir`の対象directoryに`.`と`..`以外のentryが残っている |

## 初期スタック ABI v1

MiniOSは、MiniBundleのmanifestにある`name`と`arg=`行をguestの初期スタックへ配置してから起動します。
この配置はMiniOSとguestが共有する契約です。

起動時のregisterとスタックは次のとおりです。

| 項目 | 値 |
| --- | --- |
| `a0` | `argc`。program nameを含む引数の個数 |
| `a1` | `argv`配列のユーザー仮想アドレス |
| `sp` | `argc`が置かれたアドレス。16バイト整列 |

スタック上位から低位へは、NUL終端の文字列、`AT_NULL`（typeとvalueの二語とも0）、空の`envp`（0）、`argv`のNULL終端（0）、`argv[0]`から`argv[argc - 1]`までのpointer列、`argc`の順に並びます。
`argv[0]`はmanifestの`name`を指し、`argv[1]`以降は`arg=`行の順序どおりの文字列を指します。
`spawn`で起動したchildも同じ配置で始まり、argvは`spawn`へ渡した文字列（`argc`が0ならpathのbasename 1個）です。
guestは書き換え前のスタックを読み取り専用の初期データとして扱い、以降のスタック使用は`sp`より下位へ行います。

## 互換性規約

BootHeader decoderは`abi_major=1`かつ自身以下の`abi_minor`を受理します。
現行kernelは1.0から1.2までのbundleをどれも実行でき、既存ホストの1.0 bundleと共存します。
header長、magic、flags、reserved、range layoutは表の値と規約どおりでなければなりません。
Control frame decoderは定義済みの八つのkindだけを受理します。
この文書にないfield、syscall番号、frame種別、非ゼロの予約値は推測して解釈しません。
ホストは`READY`のminorで対応ABIを判定し、minor 0の相手へ`STDIN`を送りません。
MiniOS release候補とMiniContainer release候補は、固定した相手のrelease artifactに対するABI互換性試験を通過してから公開します。

[全体構成](architecture.md) | [QEMU `virt`のメモリーマップ](memory-map.md)
