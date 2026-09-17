# 10. UART対話シェル

## 学習目標

ヒープを使わない固定長入力、ASCII文字、Backspace、入力超過時の規約を学びます。
純粋なパーサーと作用を持つ処理の分離、六つのコマンドが返す安定した出力も説明できるようになります。

## 背景

シェルのループはUARTをポーリングしますが、入力待ちの間にもタイマー割り込みが入り、ティックを更新します。
動的ヒープがないため、入力には長さの上限が必要です。
上限を超えた部分を捨てた後、末尾を削るだけで有効な入力に戻すと、利用者が端末で見た内容と実行内容が食い違います。

## 実装

[`LineBuffer<128>`](../../kernel/src/shell/line.rs)は、印字可能なASCII文字`0x20..=0x7e`だけを保持します。
容量を超えると超過フラグを立て、その後にBackspaceを受け取っても`finish()`は`Full`を返します。
Enterを受け取ると`error: input exceeds 128 bytes`を出し、次のプロンプトを表示するときの`clear()`だけが長さとフラグを戻します。
Backspaceは端末へ`\x08 \x08`を送ります。

[`parse_command`](../../kernel/src/shell/command.rs)は、前後の空白を除いた入力を`help`、`info`、`uptime`、`memory`、`clear`、`shutdown`、または`Unknown`へ分類する純粋関数です。
実際の作用はシェルのループが担当します。
`uptime`はアトミックカウンター、`memory`は一つだけ存在するアロケーターへの可変参照、`clear`は`\x1b[2J\x1b[H`、`shutdown`はSBI SRSTを使います。

OpenSBIから受け取ったハートIDは、`run(hart_id, ...)`からコマンド実行部へ渡します。
`info`は、既存のバナーの次の行にハートIDを表示します。
`uptime`は`uptime_millis()`でミリ秒を読み、別に`time::ticks()`を読んで次の行へ表示します。
二つの読み取りの間にもタイマー割り込みが入る可能性があるため、値の組を同じ瞬間の観測とは見なさず、それぞれが単調に増えることだけを利用します。

RV32側も同じ`LineBuffer`と`parse_command`を使います。
NEORV32にはSBIタイマーと物理ページアロケーターがないため、対応するコマンドは別の情報源を使います。
`uptime`は64ビットの`cycle`カウンターを96 MHzのシステムクロックで換算し、`memory`はリンカー記号から得たIMEM/DMEMの占有量を表示します。
`clear`は端末側が解釈する同じエスケープ列を使い、`shutdown`は電源切断機構を持たないCPUを`wfi`で恒久的に停止します。
CRLFを送る端末では、CRでコマンドを確定した直後のLFを一度だけ読み飛ばし、空のコマンドが続けて実行されることを防ぎます。

## 実行と確認

```text
minios> help
help      Show available commands
info      Show system information
uptime    Show elapsed time
memory    Show physical memory statistics
ls        List a directory
cat       Read a file
rm        Remove a file
mkdir     Create a directory
rmdir     Remove an empty directory
mv        Rename a file or directory
clear     Clear the terminal
shutdown  Shut down MiniOS
minios> info
MiniOS 0.1.0 on RISC-V 64
hart id: 0
minios> uptime
uptime: 120 ms
ticks: 12
minios> memory
memory: total=32231 allocated=0 free=32231 pages
minios> ls
        18 HELLO.TXT
<DIR> DOCS
        19 Long File Name.txt
minios> ls DOCS
        17 NOTE.TXT
minios> cat DOCS/NOTE.TXT
note inside docs
minios> cat Long File Name.txt
long file contents
minios> rm Long File Name.txt
minios> ls
        18 HELLO.TXT
<DIR> DOCS
minios> cat Long File Name.txt
virtio: file not found
minios> mkdir NEWDIR
minios> ls
        18 HELLO.TXT
<DIR> DOCS
<DIR> NEWDIR
minios> rmdir DOCS
virtio: directory not empty
minios> rmdir NEWDIR
minios> ls
        18 HELLO.TXT
<DIR> DOCS
minios> mv HELLO.TXT WORLD.TXT
minios> ls
        18 WORLD.TXT
<DIR> DOCS
minios> mv WORLD.TXT HELLO.TXT
minios> mv DOCS NOTESD
minios> ls
        18 HELLO.TXT
<DIR> NOTESD
minios> cat NOTESD/NOTE.TXT
note inside docs
minios> mv NOTESD DOCS
minios> unknown
unknown command: unknown; try 'help'
```

稼働時間、ティック数、ページ数は、実行時点とカーネルイメージの大きさによって変わります。
APIの規約は数値形式と行の順序であり、上の数値そのものではありません。
`info`のハートIDは、現在の`-smp 1`を使う受け入れテストでは0です。
`ls`と`cat`は、`cargo xtask run`が接続するvirtio-blkのFAT32 volumeを読みます。
`ls`は引数なしでroot、引数ありでそのsubdirectoryを列挙し、`cat`は`/`区切りのpathを受け付けます。
`rm`はfileを削除し、entryと長い名前のrecord列、cluster chainを回収します。directoryには使えず、`virtio: is a directory`と報告します。
VFATのlong file name entryがあれば表示名と解決名の両方に使い、無効な列（checksum不一致など）は8.3 aliasへfallbackします。
初回の実行時にFDTが報告したvirtio-mmio slotをprobeしてmountし、同じsessionを使い回します。
diskが無い環境では`virtio: no block device found`と表示します。
RV32側は32 KiB IMEMの制約からflatな8.3名前空間に限定し、`/`を含む名前は`sd: invalid 8.3 name`と報告します。

NEORV32では次の対話を確認できます。

```text
MiniOS/RV32 booting...
hart id: 0
minios> help
help      Show available commands
info      Show system information
uptime    Show elapsed time
memory    Show memory usage
echo      Echo text
ls        List a directory
cat       Read a file
clear     Clear the terminal
shutdown  Halt the CPU
minios> echo hello
hello
minios> memory
imem: 15240 / 32768 bytes
dmem: 1096 / 16192 bytes
```

IMEM/DMEMの占有量は、カーネルイメージの大きさによって変わります。
`shutdown`は表示の後にCPUを停止し、UARTも応答しなくなります。

## よくある失敗

- 129バイト目の後にBackspaceを押すとコマンドが実行される：Backspaceで超過フラグを解除してはいけません。
- コマンドが画面に反映されない、または新しいプロンプトが出ない：UART受信、CRとLF、内側と外側のループ境界を調べます。
- `clear`が文字列として見える：生の記録ではエスケープ列`1b 5b 32 4a 1b 5b 48`を確認します。
- 未知の入力に前後の空白が残る：パーサーがASCIIの空白を除いているかホストテストで確認します。

## 演習

QEMUを起動し、128文字の行と129文字の行を比較してください。
129文字を入力した後にBackspaceとEnterを押しても、コマンドが実行されず、入力超過エラーになることを確認します。
読み取り専用の`ticks`コマンドを追加すると仮定し、パーサーテスト、安定出力、QEMUの対話記録をどの順に変更するか書いてください。

## 次の章

[第9章](09-physical-memory.md)へ戻れます。
次は[第11章「テストハーネスの仕組み」](11-test-harness.md)で、ホストとゲストの全経路を一つの入口へまとめます。
