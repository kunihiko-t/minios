# 19. プリエンプティブscheduler

## 学習目標

一つのMiniBundleに複数のimageを宣言するmanifest v2と、各imageを`Process`としてspawnする流れを説明できるようになります。
U-mode実行中のtimer割り込みを`TrapAction::Timer`へ分類し、kernelへ戻ってから次のprocessを選ぶround-robinを追います。
`read`の入力待ちで`sepc`をecallへ戻し、process単位でblockする規約を確認します。
Stdin frameを再開可能なdecoderで受け取り、frame途中で入力が尽きても他processが進むことをQEMUで確認します。

## 背景

第17章までのpayload経路は、manifestが宣言する単一のimageを最後まで実行するだけでした。
guestがbusy-waitを続けるとkernelへ制御が戻らず、`read`の入力待ちでもkernel内で停まるため、同じbundleに二つ目のprogramを置いても交互には動きません。

この章では、kernelが実行単位を**process**として複数抱え、timer割り込みのたびに実行中のprocessを中断して次のprocessへ切り替えます。
guestの協力（yield呼び出しなど）を必要としない切り替えを**プリエンプション**と呼びます。
中断したprocessの全registerは専用kernel trap stackへ保存されるため、guestは割り込まれたことを意識せずに計算を続けられます。

入力待ちも同じ仕組みで扱い、`read`を完了できないprocessはkernelへ戻って選択対象から外れ、入力が届いたら同じecallをやり直します。
この規約により、一つのprocessの入力待ちが他のprocessの進行を止めなくなります。

## 実装

### manifest v2による複数image

[`abi/src/manifest.rs`](../../abi/src/manifest.rs)の`Manifest::parse`は、先頭行が`version=2`のとき`parse_v2`で`image=`行から始まるsectionの繰り返しを受理します。
各sectionは任意個の`arg=`行と、ELF領域先頭からの範囲を10進数で示す必須の`elf=<offset>,<len>`行で閉じ、長さ0、`u64`の溢れ、他imageとの範囲重複は拒否します。
image数の上限`IMAGE_MAX_COUNT`は4で、5個目の`image=`行は`TooManyImages`になります。
host側では[`xtask/src/bundle.rs`](../../xtask/src/bundle.rs)の`render_manifest_multi`が、連結したELFの累積offsetから`elf=`行を組み立てて同じparserで検査します。

### Processとround-robin

[`kernel/src/process.rs`](../../kernel/src/process.rs)の`Process`は、user address spaceを持つ`LoadedImage`、4ページのkernel trap stack、中断時の`UserContext`、process状態を所有します。
allocatorやframe memoryへの参照は保持せず呼び出し側が都度渡すため、生存中のprocess同士がborrowを共有せず、一つのtableが複数のprocessを同時に抱えられます。

`ProcessTable`は`Vec`でlive processだけを保持し、`insert`のたびに`next_pid`から単調にpidを採番します。
同時に生存できる数`MAX_PROCS`は`IMAGE_MAX_COUNT`と同じ4であり、`new`はその分のcapacityを先に確保して以後のreallocを防ぎます。
`take`は`swap_remove`ではなく`remove`を使うため、終了したprocessを抜いても残りの並びは変わりません。

`pick_next`は直前に選んだpid（`last_picked`）の次の位置から時計回りに走査し、最初の`Runnable`なprocessを返します。
`last_picked`がtableから抜けていれば先頭から走査し、`BlockedOnStdin`、`BlockedOnPid`、`BlockedOnPipe`、`BlockedUntil`のprocessは飛ばし、全員がblock中なら`None`を返します。

### timer割り込みでkernelへ戻る

U-mode実行中のtrapは、第15章の[`__user_trap_entry`](../../kernel/src/arch/riscv64/user.S)がprocess専用kernel stackへ`UserContext`を保存してから`rust_user_trap_handler`を呼びます。
[`classify_user_trap`](../../kernel/src/user/trap.rs)は、interrupt bit付きの原因code 5で保存済み`sstatus.SPP`が0の場合だけ`TrapAction::Timer`を返します。
割り込みは実行済みの命令ではないため、ecallと違って`sepc`を進めません。

[`kernel/src/main.rs`](../../kernel/src/main.rs)の`user_trap_timer`は`time::handle_interrupt`で次のtickを再アームし、outcomeを`USER_RUN_OUTCOME_PREEMPTED`にして`RunExit::ReturnToKernel`を返します。
tickの周期は[`kernel/src/time.rs`](../../kernel/src/time.rs)の`TICKS_PER_SECOND`（100）で決まり、`__user_run_leave`がkernel satpとboot stackを復元して`__run_user`の呼び出し元へ戻ります。

### run loop

`run_boot_payload`はmanifestのimageを宣言順に`Process::spawn`して`ProcessTable`へ入れ、pidがimage indexと一致することを確認します。
dispatch loopの前に`sstatus.SIE`を落とし、`stvec`を`__user_trap_entry`へ向け、`sie`のSTIEを立てます（U-mode実行中の割り込み配送は`sstatus.SIE`に依りません）。
loopの一周は次の順序で進みます。

1. `console::stdin_pending`がtrueなら`wake_all_blocked`でblock中のprocessを起こし、続けて`wake_sleepers`で起床tickに達したsleep中のprocessを起こします。
2. `pick_next`でpidを選び、`None`ならstdinのbyte到着か`sip.STIP`（timer割り込みのpending）をpollします。STIPが立っていれば`time::handle_interrupt`でtickを進め、loopの先頭へ戻ります。
3. `__run_user`へprocessの`context_ptr`、`user_satp`、`kernel_stack_top`を渡してU-modeへ入ります。
4. 戻ったら`reload_context`でtrap frame slot（kernel stack topから416 byte下）の中断contextを`Process`へ回収します。
5. outcomeが`PREEMPTED`ならそのまま、`BLOCKED`なら`block_on_stdin`、`EXIT`なら終了frameを送って`reclaim_process_slot`で回収します。

manifestが`version=2`のとき、終了は[`ProcExitPayload`](../../abi/src/control/proc_exit.rs)の8 byte（pidと終了codeのlittle endian `u32`）を載せた`ProcExit` frameで通知します。
`version=1`では従来どおり4 byteの`Exit` frameを送るため、第17章までのtestは変わりません。
全processの終了後はallocatorの統計がspawn前と一致することを確認し、`processes=`と`switches=`を報告します。

### blockとecallのやり直し

`read`の入力待ちは、[`kernel/src/user/syscall.rs`](../../kernel/src/user/syscall.rs)の`dispatch_read`がsourceから`Ok(None)`を受けたときに起こります。
`classify_user_trap`が`sepc`をecallの次へ進めてあるため、`dispatch_read`は`sepc`を4 byte戻してから`SyscallFlow::Blocked`を返し、run loopはprocessを`BlockedOnStdin`へ移します。
起こされたprocessは同じecallから再開し、`a0`から`a2`も保存時のままなので、同じ引数で`read`をやり直します。

pipeの`read`/`write`と`waitpid`も同じ規約に従い、control層がcallerを`BlockedOnPipe`や`BlockedOnPid`へmarkしてから`EAGAIN`（`waitpid`は`Ok(None)`）を返すと、dispatch層が`sepc`を戻して`Blocked`にします。
guestへ`EAGAIN`自体が返ることはありません。
`block_on_stdin`は`Runnable`のときだけ状態を変えるため、先にmarkされたpipe待ちやpid待ちを上書きしません。

### yieldとsleep

`yield`（syscall 26）は`a0`へ0を書き、`sepc`をecallの次に置いたまま`SyscallFlow::Yield`を返します。
handlerはこれをtimer割り込みと同じ`USER_RUN_OUTCOME_PREEMPTED`へ写すため、run loopは次のprocessを選び、再び選ばれたprocessはecallの次から続けます。
`sleep`（syscall 25）は、control層が`time::sleep_deadline`で求めた起床tickを`BlockedUntil`へmarkしてから同じ`Yield`を返します。
起床tickはmillisecondをtick数へ切り上げ、途中まで経過した現在のtickの分として1を足すため、早く起きることはありません。
戻り値の0は先に書いてあるので、`wake_sleepers`で起きたprocessはsyscallをやり直しません。
そのため`wake_all_blocked`はstdinが届いても`BlockedUntil`のprocessを起こしません。
`sleep(0)`はmarkせずに`yield`と同じ経路を通ります。

dispatch loopのS-modeは`sstatus.SIE`を落としているため、全processがblock中の間はtimer割り込みでtrapせず、tickは進みません。
そこでidle待ちはstdinと並べて`sip.STIP`をpollし、立っていれば`handle_interrupt`でtickを進めて次のtimerを再アームします。
この方法なら、process一つだけがsleepしている場合も期限に起こせます。

### 再開可能なStdin decoder

[`kernel/src/user/stdin.rs`](../../kernel/src/user/stdin.rs)の`StdinStaging`は、Stdin frameのheader蓄積とpayload蓄積を`header_len`、`want`、`have`で表すstate machineであり、`feed`は`try_read_byte`が`None`を返すまでbyteを流し込みます。
完全なframeが揃わなければ`read`は`StdinError::WouldBlock`を返し、[`kernel/src/control.rs`](../../kernel/src/control.rs)の`read_stdin`がそれを`Ok(None)`へ写します。
この写像により、frameの途中で止まった`read`も前節のblock経路へ入り、続きのbyteが届いた後の再実行で蓄積済みの断片からdecodeが進みます。

## 実行と確認

[`guest/src/bin/sched_a.rs`](../../guest/src/bin/sched_a.rs)は`a1`、`a2`、`a3`の各出力の後に`yield`を呼んで終了code 0で、[`guest/src/bin/sched_b.rs`](../../guest/src/bin/sched_b.rs)は`b1`から`b3`をすぐに出力して終了code 7で`exit`します。
[`guest/src/bin/sched_r.rs`](../../guest/src/bin/sched_r.rs)は`r1`を出してから`read(stdin)`でblockし、入力が届くと`r2`を出して終了code 5で`exit`します。
sched_aが各出力の後に`yield`するため、切り替えの時点はtime sliceの長さではなくguestの命令列で決まり、testの結果がQEMUの速度に左右されません。
sched_rとsched_bは`yield`を呼ばないので、それ以外の切り替えはtimerプリエンプションか`read`のblockで起こります。

`sched` testは`spin`（sched_a）と`quick`（sched_b）の二つのimageを持つbundleを実行します。
次の出力ではcontrol frameのheader byteを取り除き、payloadのtextだけを残しています。

```console
$ cargo xtask test sched
...
MiniOS sched: spawned pid=0 name=spin
MiniOS sched: spawned pid=1 name=quick
a1
b1
b2
b3
a2
a3
MiniOS payload: ok processes=2 switches=2
phase 1/1 passed (elapsed: ...)
summary: PASSED all 1 phases (elapsed: ...)
```

[`verify_sched_result`](../../xtask/src/qemu.rs)は、stdout上で`a1 < b1 < a3`の順序になること、`ProcExit`が`(1, 7)`、`(0, 0)`の順に届くこと、`switches`が1以上であることを要求します。
`b1`が`a1`と`a3`の間に出るのは、`spin`が`a1`の後の`yield`で`quick`へ順番を譲ったからです。
`quick`がtickの境目でpreemptされても、`spin`は次の出力の直後にまた`yield`するため、`quick`は`spin`の`a3`より先に終わります。

`sched-io` testは`reader`（sched_r）、`spin`、`quick`の三つを実行し、hostは`b3`を観測してから1 byteのStdin frameを送ります。

```console
$ cargo xtask test sched-io
...
r1
a1
b1
b2
b3
a2
a3
r2
MiniOS payload: ok processes=3 switches=4
...
summary: PASSED all 1 phases (elapsed: ...)
```

[`verify_sched_io_result`](../../xtask/src/qemu.rs)は`r1 < r2`、`a1 < a3`、`b1 < b3`、`b3 < r2`の順序と、`ProcExit`の集合が`(0, 5)`、`(1, 0)`、`(2, 7)`であることを要求します。
exitの順序と`switches`の値は実行ごとに変わるため、exitは集合として比較し、`switches`は1以上であることだけを見ます。

`sched-io-partial` testは同じbundleで、Stdin frameの先頭5 byte（magicとkind）を送ってから100 ms待ち、残りを送ります。

```console
$ cargo xtask test sched-io-partial
...
b3
a2
a3
r2
MiniOS payload: ok processes=3 switches=4
...
summary: PASSED all 1 phases (elapsed: ...)
```

検査条件は`sched-io`と同じです。
decoderが再開可能でなければ、残りのbyteをheaderの先頭として読み直してframe境界がずれるため、`r2`へ到達せずに失敗します。

## よくある失敗

- timer割り込みで`sepc`を進める：割り込みは実行済みの命令ではないため、進めると中断位置の命令を一つ飛ばします。`sepc`を4 byte進めるのはecallだけです。
- `Blocked`で`sepc`を戻し忘れる：再開時にecallの次から走り、`a0`に残ったfd値（stdinでは0）を戻り値として受け取るため、guestにはEOFに見えます。
- dispatch loopで`sstatus.SIE`を立てたままにする：S-mode実行中に届いたtickが`sscratch`未設定の`__user_trap_entry`へ飛び、context保存先が壊れます。
- `reload_context`を呼ばずに次のprocessを選ぶ：`Process`内の`context`が古いままになり、次のdispatchで今回の中断位置ではなく前回回収した位置から再開します。
- `take`を`swap_remove`へ変える：末尾のprocessが抜けた位置へ移り、round-robinの順序が終了のたびに入れ替わります。
- 全processをpipe待ちやpid待ちにする：`pick_next`が`None`になるとloopはstdinの到着とtimer tickだけを待ち、tickで起きるのはsleep中のprocessだけなので、stdinを送らない限り進みません。
- `sleep`で`sepc`を戻す：起床後に同じecallを再実行すると、その時点から新しい期限を計算して眠り直すため、`sleep`がいつまでも戻りません。

## 演習

[`guest/src/bin/sched_a.rs`](../../guest/src/bin/sched_a.rs)の`yield_now`の呼び出しを外し、`cargo xtask test sched`の出力で`b1`の位置がどう変わるかを観察してください。
`spin`は1 tickのうちに`a3`まで進むため、`a1 < b1 < a3`が崩れてtestが失敗します。
確認後は呼び出しを元に戻します。

[`kernel/src/process.rs`](../../kernel/src/process.rs)の`pick_next`へ、四つのprocessのうち二つを`BlockedOnStdin`にした状態で走査順を確かめるhost testを追加してください。
`last_picked`のprocessを`take`で抜いた後に、先頭から走査が再開することも確認します。
試験後は`cargo test -p minios-kernel process::tests --locked`を実行してください。

## 次の章

[第20章「virtio-blkでディスクを読み書きする」](20-virtio-blk.md)では、processが読み書きするfile systemの土台として、virtio-blk deviceの駆動を追います。
