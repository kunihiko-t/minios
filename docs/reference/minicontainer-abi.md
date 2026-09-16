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
`pid`はmanifest内のimage index（0始まり）で、複数image bundleでは各processの終了ごとに1 frameを送ります。

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
| 1 | `write` | `a0=fd`、`a1=pointer`、`a2=length` | `fd=1`は標準出力、`fd=2`は標準エラー出力。出力は一回につき4 KiB以下 |
| 2 | `exit` | `a0=code` | 下位8ビットをアプリケーション終了コードとしてホストへ渡し、アプリケーションへ戻らない |
| 3 | `read` | `a0=fd`、`a1=pointer`、`a2=length` | `fd=0`は標準入力、`fd>=3`は`open`したfile。入力は一回につき4 KiB以下。戻り値はbyte数、EOFは0 |
| 4 | `read_file` | `a0=path pointer`、`a1=path length`、`a2=buffer pointer`、`a3=buffer length` | `path`はUTF-8のFAT32パスで256 byte以下。戻り値は読んだbyte数。fileがbufferより長い場合は先頭で打ち切る |
| 5 | `open` | `a0=path pointer`、`a1=path length` | `path`はUTF-8のFAT32パスで256 byte以下。戻り値は3以上のfile descriptorか負のerrno。processごとに最大4個まで開ける |
| 6 | `close` | `a0=fd` | 戻り値は0か負のerrno |

`write`は、対象範囲がユーザー空間の読み取り可能ページにすべて含まれることを要求します。
`read`は、対象範囲がユーザー空間の書き込み可能ページにすべて含まれることを要求し、範囲の検証を通ってから入力を消費します。
長さ0の`read`は入力へ触れず0を返します。
`read_file`は、path範囲が読み取り可能でbuffer範囲が書き込み可能であることを要求し、両方の検証を通ってからstorageへ触れます。
長さ0の`read_file`は0を返します。`read_file`は呼び出しのたびにfile全体を先頭から読む1回限りの操作であり、offsetやopen中のhandleは持ちません。
`open`は`read_file`と同じpath規約で、成功するとfileを`fd`へ結び付けます。
file fdへの`read`は現在のoffsetから読み、読んだ分だけoffsetを進めます。
`close`はfdを解放します。標準stream（0、1、2）は閉じられず、processは終了時に残ったfdを自動的に閉じます。
負のABI error値は次のとおりです。

| 値 | 名前 | 条件 |
| ---: | --- | --- |
| `-2` | `ENOENT` | fileが存在しない |
| `-5` | `EIO` | storageの読み取りまたはfilesystem構造の失敗 |
| `-9` | `EBADF` | 未知のfile descriptor、標準streamへの`close`、未割り当てfdへの`read`/`close` |
| `-12` | `ENOMEM` | kernelがstorage用のframeを確保できない |
| `-14` | `EFAULT` | 不正なpointerまたは権限不足の範囲 |
| `-19` | `ENODEV` | block deviceが見つからない |
| `-20` | `ENOTDIR` | パス途中の要素がfileである |
| `-21` | `EISDIR` | `read_file`の対象がdirectoryである |
| `-22` | `EINVAL` | 4 KiBを超える入出力長、256 byteを超えるpath、UTF-8でないpath、無効なパス要素 |
| `-24` | `EMFILE` | processの同時open数（4個）を超えた |
| `-38` | `ENOSYS` | 未知のsyscall番号、またはstorageを持たない経路での`read_file` |

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
