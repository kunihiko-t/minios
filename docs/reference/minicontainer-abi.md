# MiniContainer Guest ABI

この文書は、MiniOSとMiniContainerが共有するMiniContainer Guest ABI v1を定義します。
整数は、特記しない限りlittle-endianで表現します。
このABIの公開型と定数は`minios-abi` crateにあります。

## MiniBundle v1

MiniBundle v1は、96バイトの固定header、UTF-8 manifest、8バイト境界までのゼロpadding、静的ELFの順に並ぶ単一ファイルです。
bundle全体の長さは8 MiB以下です。
Rustの構造体をそのままメモリーへ配置せず、各fieldを次のoffsetから読んでください。

| offset | size | field | v1の値と意味 |
| ---: | ---: | --- | --- |
| 0 | 8 | `magic` | ASCII `MINICTR`と末尾NUL |
| 8 | 2 | `abi_major` | `1` |
| 10 | 2 | `abi_minor` | `0` |
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
argument-char = UTF-8文字列中のNULとLF以外のバイト
```

`argument`のCRは値として保持します。
`name`はCRを含められません。
`arg=`は0個から16個まで置けます。
未知のkey、順序違反、重複する`version`または`name`、空の`name`、末尾LFの欠落は受理しません。

```text
version=1
name=hello.world_1-test
arg=first
arg=second
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

| 値 | kind | payload |
| ---: | --- | --- |
| 1 | `READY` | 4バイト。ABI majorとABI minor |
| 2 | `STDOUT` | 任意のバイト列 |
| 3 | `STDERR` | 任意のバイト列 |
| 4 | `EXIT` | 4バイトの符号なし終了コード |
| 5 | `GUEST_ERROR` | UTF-8診断 |
| 6 | `DIAGNOSTIC` | UTF-8診断 |

`READY`と`EXIT`の`payload_len`は必ず4です。
未定義の`kind`、非ゼロの`flags`または`reserved`、上限を超える長さ、固定長payloadの不一致は受理しません。
同期後にheaderまたはpayload長の規約が壊れたとき、ホストはbyte streamを推測で再同期せず、protocol failureとしてinstanceを停止します。

## Syscall ABI v1

syscall番号は`a7`、引数は`a0..a5`、戻り値は`a0`へ置きます。

| 番号 | 呼び出し | 引数 | 規約 |
| ---: | --- | --- | --- |
| 1 | `write` | `a0=fd`、`a1=pointer`、`a2=length` | `fd=1`は標準出力、`fd=2`は標準エラー出力。出力は一回につき4 KiB以下 |
| 2 | `exit` | `a0=code` | 下位8ビットをアプリケーション終了コードとしてホストへ渡し、アプリケーションへ戻らない |

`write`は、対象範囲がユーザー空間の読み取り可能ページにすべて含まれることを要求します。
負のABI error値は次のとおりです。

| 値 | 名前 | 条件 |
| ---: | --- | --- |
| `-38` | `ENOSYS` | 未知のsyscall番号 |
| `-9` | `EBADF` | 未知のfile descriptor |
| `-14` | `EFAULT` | 不正なpointerまたは読み取り不能な範囲 |
| `-22` | `EINVAL` | 4 KiBを超える出力長 |

## 互換性規約

v1のBootHeader decoderは`abi_major=1`かつ`abi_minor=0`だけを受理します。
header長、magic、flags、reserved、range layoutは表の値と規約どおりでなければなりません。
Control frame decoderは定義済みの六つのkindだけを受理します。
この文書にないfield、syscall番号、frame種別、非ゼロの予約値は推測して解釈しません。
MiniOS release候補とMiniContainer release候補は、固定した相手のrelease artifactに対するABI互換性試験を通過してから公開します。

[全体構成](architecture.md) | [QEMU `virt`のメモリーマップ](memory-map.md)
