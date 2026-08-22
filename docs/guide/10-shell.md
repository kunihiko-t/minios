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

稼働時間、ティック数、ページ数は、実行時点とカーネルイメージの大きさによって変わります。
APIの規約は数値形式と行の順序であり、上の数値そのものではありません。
`info`のハートIDは、現在の`-smp 1`を使う受け入れテストでは0です。

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
