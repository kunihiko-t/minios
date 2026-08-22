# 10. UART対話shell

## 学習目標

heapなしの固定長入力、ASCII/Backspace/overflow規約、pure parserと作用の分離、6 commandの安定出力を
説明できるようになります。

## 背景

shell loopはUARTをpollingしますが、待機中もtimer trapが入りtickを更新します。dynamic heapが
まだないため、入力はboundedでなければなりません。また途中で捨てた文字を含む行を、末尾を削った
だけで有効に戻すと、userが見た入力と実行内容が食い違います。

## 実装

[`LineBuffer<128>`](../../kernel/src/shell/line.rs)はprintable ASCII `0x20..=0x7e`だけを保持します。
capacity超過でoverflow flagを立て、Backspace後も`finish()`は`Full`のままです。Enterで
`error: input exceeds 128 bytes`を出し、次promptの`clear()`だけがlenとflagを戻します。Backspaceは
端末へ`\x08 \x08`を送ります。

[`parse_command`](../../kernel/src/shell/command.rs)はtrimした入力を`help/info/uptime/memory/clear/
shutdown`または`Unknown`へ分類するpure functionです。作用はshell loopが担当し、uptimeはatomic
counter、memoryは一意なallocatorへのmutable reference、clearは`\x1b[2J\x1b[H`、shutdownはSBI
SRSTを使います。

OpenSBIから受け取ったhart IDは`run(hart_id, ...)`からcommand実行へ渡します。`info`は既存bannerの
次行にhart IDを表示します。`uptime`は既存の`uptime_millis()`でmsを読み、追加の`time::ticks()`を
次行へ表示します。timer割り込みは二つのread間にも入り得るため、両方を単調な観測値として扱います。

## 実行と確認

```text
minios> help
help      Show available commands
info      Show system information
uptime    Show elapsed time
memory    Show physical memory statistics
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
minios> unknown
unknown command: unknown; try 'help'
```

uptimeとtick数、page数は実行時点やkernel image sizeで変わり得るため、数値形式と行順を契約にし、
固定値をAPIにしません。`info`のhart IDは現在の`-smp 1` acceptanceでは0です。

## よくある失敗

- 129 byte目の後にBackspaceすると実行される: overflow flagをBackspaceで解除してはいけません。
- commandがechoされない/新promptがない: UART受信、CR/LF、outer/inner loop境界を調べます。
- `clear`が文字列として見える: raw logではescape `1b 5b 32 4a 1b 5b 48`を確認します。
- unknown入力の前後spaceが残る: parserがASCII whitespaceをtrimしているかhost testで確認します。

## 演習

QEMUを起動し、128文字と129文字の行を比較してください。129文字の後でBackspaceしてEnterを押しても
commandが実行されずoverflow errorになることを確認します。次にread-only `ticks` commandを追加する
なら、parser test、stable output、QEMU transcriptのどこを先に変更するか順序を書きます。

## 次の章

[第9章](09-physical-memory.md)へ戻れます。次は
[第11章: test harness](11-test-harness.md)で、hostとguestの全経路を一つの入口へまとめます。
