# stdin frame の再開可能 decode（frame途中受信の同期polling除去）

2026-09-15。blocking read対応（PR #24）の残存制限を取り除く。

## 背景と現状の制限

`dispatch_read`は`stdin_ready`で入力有無を確認し、なければ`Blocked`を返して
processをstdin待ちへ回す。しかしready判定は「1 byteでも届いたか」であり、
frame途中でbyteが尽きると`StdinStaging::pull_frame`が`read_exact`で残りを
trap内同期pollingする。現行はS-mode割り込みがmaskedなので、その間は
タイマーpreemptionさえ効かず全processが停止する。

## 目標

UARTのstdin経路から同期pollingを完全に取り除き、frame途中のbyte枯渇でも
processが`Blocked`へ戻れるようにする。状態は`StdinStaging`内の再開可能な
decode state machine（header蓄積 + payload蓄積）として保持する。

## 設計

- `ByteReader`に`try_read_byte() -> Option<u8>`を追加（defaultは
  `Some(read_byte())`でblocking reader互換）。`UartBytes`は
  `has_byte`でgateしたnon-blocking実装を持つ。
- `StdinStaging`にdecoder stateを追加:
  `header: [u8; FRAME_HEADER_LEN]`、`header_len`、`want`、`have`。
  - `feed(reader)`: `has_pending() || eof`なら即return。そうでなければ
    `try_read_byte`が得られる間だけ1 frame分までstate machineへ流す。
    消費したbyteがあれば`Ok(true)`。frame完成で`start=0,end=want`。
  - `pull_frame`: `feed`を繰り返し、complete/eofなら`Ok(())`、
    1 byteも進めずsource枯渇なら`Err(WouldBlock)`（新variant）。
  - decode/kind errorは`error: Option<StdinError>`へ保持して以降も
    同じerrorを返し続ける（今日のone-shot errorと等価の終端性）。
- `ControlSource::read_stdin`の戻り値を`Result<Option<usize>, E>`へ変更。
  `Ok(None)`は「検証済みだが入力未到着」。`stdin_ready`は不要になるので削除
  （ready判定とread本体が同じstate machineを共有する方が一貫する）。
- `dispatch_read`: `read_stdin`を直接呼び、`Ok(Some(n))`→ReadComplete、
  `Ok(None)`→sepcを4戻して`Blocked`、`Err`→SourceFatal。
- `UartControlSource::read_stdin`は`Err(WouldBlock)`を`Ok(None)`へ写像。
- スケジューラ側のwake判定`console::stdin_pending()`はbyte粒度のまま。
  byte到着→wake→feed→途中なら再Blocked、という繰り返しで最終的に
  完全frameが揃う。EOF(len=0) frameもheader byte到着でwakeできる。

## 検証

- ホスト: drip-feed reader（script途中でNoneを返す）で
  WouldBlock→追記→再開→完成の往復、decode errorの終端性、
  既存のEOF/分割deliver/oversize各経路を維持。
- QEMU: `sched-io-partial`を追加。`b3`marker後にStdin frameを
  分割して2回書き込み（間にhost sleepを入れてguest側のconsumeを確実化）、
  `r2`到達と全processの正しいProcExit・ok diagnosticを
  既存`verify_sched_io_result`で検証する。再開可能でないdecoderでは
  partial header消費後に続きを先頭からdecodeしてしまいdesync→fatal
  となるため、この成功自体がresume経路の証明になる。
- 全ゲート（フェーズ数は34へ更新）。
