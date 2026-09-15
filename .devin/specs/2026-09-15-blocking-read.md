# stdin `read` のスケジューラ対応

## 背景

現在の`read`はsyscall handler内でUARTを同期pollingする。trap中は割り込みが
無効なため、stdin待ちでblockしたprocessの間は他processも一切進まない。
scheduler導入 (PR #23) 時点で既知の制限として文書化したものであり、本featureで
「blockしても他processが回る」状態へ近づける。

## 方針: ecall再実行 (restartable blocking syscall)

frame protocolのbyte pull型受信を非同期化する全面改修ではなく、
「データ未到着ならecall自体をやり直す」方式を取る。

1. `dispatch_read`は検証 (EBADF/EINVAL/EFAULT) をすべて通した後、
   `source.stdin_ready()`で先に受信可能かを確認する。stdinの消費は不可逆
   なので、検証より前にready判定を挟んではいけない。
2. 未到着なら`SyscallFlow::Blocked`を返す。このとき`sepc`を4 byte戻し、
   再開時に同じecallが再実行されるようにする (classifyが+4済みという
   既存契約の上に立つ)。partial readの状態は持たない。
3. handlerは`Blocked`を`USER_RUN_OUTCOME_BLOCKED`としてkernelへ戻す。
   dispatch loopはそのprocessを`Blocked`へ遷移させる。
4. `ProcessTable::pick_next`は`Runnable`だけを選ぶ。占有slotが残るのに
   選べない＝全processがstdin待ち。kernelはその場合だけUARTの
   data-ready bitをpollし、到着したらblocked processを起こす。
5. 各dispatch復帰時にも`stdin_pending`を見てblocked processを起こし、
   1 tick内に到着した入力でreaderがすぐ再選されるようにする。
6. frameの途中受信 (`pull_frame`の`read_exact`) は従来どおりblockし得るが、
   QEMUはhostからのframeをFIFOへ即座に載せるため bounded である。
   この残余のblockingは制限として文書化を残す。

## APIの変更

- `ControlSource::stdin_ready(&mut self) -> bool` を追加。
  `StdinStaging`に未配達byteがあるか、EOF済みか、UARTにdata-readyが
  あるかを意味する。host testのstubは`ready` flagを返すだけでよい。
- `SyscallFlow::Blocked` variant追加。
- `StdinStaging::has_pending()`追加 (`start != end`)。
- `Process`に`Runnable | BlockedOnStdin`のstateを追加。
- `ProcessTable::wake_all_blocked`追加。
- `USER_RUN_OUTCOME_BLOCKED = 6`。

## 検証

- host test: not-ready sourceでBlocked + sepc巻き戻し、ready sourceでは
  従来どおりReadComplete、blocked slotを`pick_next`が飛ばすこと、
  wakeで再選されること、全blockedで`pick_next`がNoneを返すこと。
- QEMU `sched-io`: 3 image bundle。image0は`r1`を出して`read`でblock、
  image1はbusy-waitしながら`a1..a3`、image2は`b1..b3`でexit(7)。
  hostは`b3`を観測してからStdin frameを送る。`r1 < b3 < r2`と
  全ProcExitの到着を検証する。block中に他processが進んだ直接証拠となる。
- 既存`payload-stdin`経路が同一実装で回り続けることを確認する。

## スコープ外

- UART受信割り込み化、wfi省電力待機、複数processのstdin競合解消
  (stagingは引き続き共有)。
- RV32シェルのread経路 (対話shellはprocess modelの外)。
