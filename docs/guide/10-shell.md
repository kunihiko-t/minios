# UART shell

MiniOS は起動、trap、timer、物理メモリの初期化後に UART shell を開始します。shell は
`minios> ` を表示し、QEMU virt の 16550 互換 UART を polling して一文字ずつ読みます。
受信待ちに `wfi` は使いませんが、`sstatus.SIE` は有効なままなので、待っている間も timer
割り込みは trap handler へ入り、uptime の tick を更新します。handler は端末へ文字を出さない
ため、入力途中の prompt は割り込みで崩れません。

## heap を使わない固定長入力

まだ heap allocator がないため、入力行はスタック上の `LineBuffer<128>` に保持します。
内部は `[u8; 128]`、現在長、overflow flag だけです。`push` が受け入れるのは space (`0x20`)
から `~` (`0x7e`) までの printable ASCII に限られます。この制約により安全な公開 API から
不正な UTF-8 を格納できず、`as_str` は unchecked 変換を使わずに slice を検査して返せます。

128 byte を超えた最初の文字で overflow flag が立ちます。その後に Backspace で格納済み文字を
減らしても、その入力行全体は無効なままです。Enter で
`error: input exceeds 128 bytes` を表示して破棄し、次の prompt では `clear` が長さと flag の
両方をリセットします。この規約なら、末尾を削った結果だけを見て途中の欠落した入力を実行する
ことがありません。

## echo と編集

受け入れた printable ASCII は直ちに UART へ echo します。Backspace (`0x08`) と Delete
(`0x7f`) は末尾がある場合だけ一文字を削り、端末へ `\x08 \x08` を送ります。これは cursor を
一つ戻し、space で表示文字を消し、もう一度戻す並びです。空行での削除やほかの制御文字は何も
表示しません。CR と LF はどちらも行の確定として扱います。

## parse と command の作用

`parse_command(&str) -> Command<'_>` は ASCII whitespace を両端から除き、完全一致する小文字の
`help`、`info`、`uptime`、`memory`、`clear`、`shutdown` を識別します。未知の入力は
`Command::Unknown(&str)` にします。この `&str` の lifetime は元の `LineBuffer` の slice と同じ
なので、コピーや heap allocation は不要であり、buffer を clear した後まで保持することも
できません。

parser は文字列を分類するだけで UART、timer、allocator、SBI に触れません。作用は shell loop
側に分離し、`uptime` は `uptime_millis()`、`memory` は渡された
`&mut FrameAllocator<512>` の `stats()`、`shutdown` は SBI system reset を使います。allocator を
global にせず可変参照で渡すことで、将来 page を操作する command を追加した場合も所有権境界が
明示されたままです。`clear` は ANSI の `\x1b[2J\x1b[H` を送り、画面消去後に次の prompt を
左上へ表示します。

## 安定した出力と QEMU test

自動試験が command の外部契約を確認できるよう、help の
`help      Show available commands`、info の `MiniOS 0.1.0 on RISC-V 64`、
`uptime: <number> ms`、`memory: total=<number> allocated=<number> free=<number> pages`、
unknown command の診断、shutdown 前の `shutting down` は安定した文字列です。

`cargo xtask test shell` は feature を付けない通常 kernel を起動し、`minios> ` を待ってから
`help`、`info`、`uptime`、`memory`、未知 command、`shutdown` を UART stdin へ送ります。5 秒以内の
status 0 と全出力を要求し、不一致や timeout では起動からの transcript 全体を残します。これに
より parser だけでなく、UART 受信、echo、timer の進行、allocator stats、SBI reset までを同じ
session で確認します。

## 演習: read-only `ticks`

1. `Command::Ticks` を追加し、parser の host unit testを先に失敗させてください。
2. shell の command 作用から `time::ticks()` を読み、`ticks: <number>` と表示してください。
3. allocator や atomic counter を変更しない read-only command であることを保ち、QEMU script と
   安定出力の検証を追加してください。
