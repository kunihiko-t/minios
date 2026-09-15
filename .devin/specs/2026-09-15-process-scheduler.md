# プロセス管理・プリエンプティブスケジューラ設計

2026-09-15。作業用設計書。コミット対象外（最終ドキュメントは `docs/guide` / `docs/reference` へ書く）。

## 背景

現行のユーザー実行経路は単一 `UserRun` の一回限りモデル:

- `run_boot_payload` が1個のELFをloadし、`UserRun::execute` でU-modeへ入る。
- `__run_user` → `__user_trap_entry` が trap 毎に全コンテキストを kernel trap stack (`stack_top-416`) へ保存。
- `rust_user_trap_handler` は `RunExit::{Resume, ReturnToKernel}` のみ返す。U-mode ecall 以外は全て fatal。
- 実行窓では STIE を落としているため、タイマー割り込みはユーザー実行中に起きない。
- `read` は syscall ハンドラ内で UART を同期ポーリングする。trap 中は SIE=0 なので、その間は一切の割り込みが起きない。

## 要件（確定済み）

- 1個の MiniBundle に複数 ELF を格納し、複数プロセスとして起動する。
- プリエンプティブ round-robin。既存の 100 Hz タイマーをそのまま使う（10 ms タイムスライス）。
- プロセス数上限は固定 `MAX_PROCS = 4`。
- `read` の同期ブロックは維持し、制限として文書化する。
- 既存の所有権・rollback 保証（image frames、page tables、user stack、kernel trap stack）を維持する。
- スケジューラの状態機械はホストテスト可能な純粋 Rust とする。

## 方式: trap-return スイッチループ（案A）

タイマー割り込みは U-mode trap として `__user_trap_entry` に入り、実行中プロセス固有の kernel trap stack へ context を保存する。handler はタイマー再アーム + `RunOutcome::Preempted` を記録して `ReturnToKernel` を返す。Rust 側スケジュールループが次の runnable プロセスを選んで `__run_user` へ再投入する。

- asm 変更なし（`bnez a0` は既に「非ゼロなら kernel 復帰」）。
- handler は判断を持たない。tick では常に kernel へ戻り、ループが再選択する（同一プロセスが選ばれればそのまま再開＝実質 continue）。
- context の実体は `Process.context: UserContext` フィールド。`ReturnToKernel` 後に trap stack の slot (`stack_top-416`) から 272 B をコピーバックする。再開時は `&mut proc.context` を `a0` に渡す。初回・再開で同一経路。

## コンポーネント

### 1. ABI / manifest v2（`abi` crate）

`BootHeader` は不変（`elf` range は全 image の連結領域）。manifest を `version=2` へ拡張:

```text
version=2
image=spin
arg=slow
elf=0,4096
image=cat
elf=4096,2048
```

- `image=<name>` が新しい image セクションを開始する。後続の `arg=` / `elf=` は直近の image に属する。
- `elf=<offset>,<len>` は `header.elf` 先頭からの相対 range。必須・重複不可・範囲内必須。
- `version=1` の manifest は単一 image `{name, args, elf=全range}` として受理（後方互換）。
- image 数 > `MAX_PROCS` は bundle 拒否。
- `Manifest` パーサは `images()` iterator を返す形へ一般化。v1 でも 1 要素を yield。

### 2. Process（`kernel/src/process.rs` 新設）

```rust
pub const MAX_PROCS: usize = 4;

pub enum ProcessState {
    Runnable,
    Exited(u32),
    Faulted,
}

pub struct Process {
    pid: usize,                    // slot index = manifest の image index
    name: &'static str,            // 予約窓は boot 中存在するので借用でよい
    image: LoadedImage,            // ELF segment frames の所有権
    memory: ProcessMemory,         // user stack + stdin staging 参照
    space: AddressSpace,           // page tables + heap Box<AddressSpaceStorage>
    kernel_stack: [Option<PhysFrame>; KERNEL_STACK_PAGES],
    kernel_stack_bottom: usize,
    context: UserContext,
    state: ProcessState,
}
```

- `Process::spawn(spec, image_elf, args, memory, allocator) -> Result<Self, SpawnError>`: ELF load → user stack 構築 → kernel stack 確保 → context 初期化。途中失敗は確保済みを逆順 rollback。
- `context_ptr()` / `stack_top()` / `satp()` アクセサ。
- `UserRun` の所有権モデルを proc 化する形。`UserRun` 自体は one-shot probe 経路で残し、scheduler は `Process` + `__run_user` 直接呼び出しで実装する（`UserRun::execute` の closure モデルは再入に向かないため）。

### 3. ProcessTable / スケジューラ（`kernel/src/sched.rs` または process.rs 内）

```rust
pub struct ProcessTable {
    procs: Vec<Option<Process>>,  // slot 固定。回収後は None or state=Exited
    next_hint: usize,
}
```

- `spawn` は空き slot へ挿入。満杯は `SpawnError::TableFull`。
- `pick_next() -> Option<usize>`: `next_hint` から巡回して最初の `Runnable`。状態遷移のみの純粋ロジックとして host test 可能にする。
- `mark_exited(pid, code)` / `mark_faulted(pid)` / `reclaim(pid) -> Process`（所有権を取り出して drop = frame 解放）。
- 終了条件: `pick_next() == None`。

### 4. trap 経路の拡張

- `RunOutcome` に `Preempted` を追加（`run.rs`）。
- `classify_user_trap`: `Interrupt(cause=5)`（supervisor timer）を新 outcome へ。handler 内で `time::handle_interrupt()`（再アーム + tick++）を呼ぶ。`sepc` は進めない（割り込みは skip する命令ではない）。
- 本番 sched 用の `rust_user_trap_handler` variant を追加。probe 用 variant は現行のまま。
- `user_trap_fatal` は従来通り GuestError frame + `ReturnToKernel`（proc を Faulted として回収し他 proc を継続）。

### 5. 実行ループ（`main.rs` の `run_boot_payload` 改修）

```text
bundle 検証 → manifest images を列挙
for image in images: Process::spawn → table.insert
Ready frame 送出（従来通り1回）
STIE を set（実行窓でタイマー有効化）
loop {
    pid = table.pick_next() else break
    proc = &mut table[pid]
    probe statics (SPACE, MEMORY) を proc へ向ける
    USER_RUN_OUTCOME = None
    __run_user(&mut proc.context, proc.satp(), kernel_satp, proc.stack_top())
    proc.context = *(stack_top - 416)          // copy-back
    match outcome {
        Preempted => continue,                  // pick_next が次を選ぶ
        Exited(code) => ProcExit frame + reclaim,
        Fatal => GuestError は既送出 + reclaim,
    }
}
STIE クリア → shell 復帰 / qemu test は shutdown
```

- probe statics (`USER_SYSCALL_PROBE_SPACE`/`MEMORY`) は dispatch 毎に現在 proc を指す。単一 hart・逐次 dispatch なのでデータ競合なし。
- `Preempted` でも `pick_next` が同じ pid を返せば同一 proc が直ちに再開される（単一 proc 時の正しい動作）。

### 6. 制御フレーム

- `Exit` は 4 byte code 固定長で proc 識別子を持たない。複数 proc の終了を host が帰属できるよう `FrameKind::ProcExit = 8` を追加: payload = `u32 pid + u32 code` の 8 byte 固定長。
- 単一 image（v1）bundle では従来の `Exit` のみ送出し互換性を維持。v2 では各 proc 終了時に `ProcExit` を送出。
- proc 開始時に `Diagnostic` frame で `sched: spawned pid=N name=...` を出すと検証が楽になる（任意・要検討）。

### 7. xtask / bundle 生成

- `cargo xtask bundle` に複数 image 指定を追加（例: `--image name=path --arg ...` の繰り返し、または test 専用の内部 API）。
- QEMU sched テストは guest crate に `sched_a` / `sched_b` bin を追加し、2-image bundle を loader で流す。
- `cargo xtask test sched` phase を追加（`check` 段階数 31→32、順序テスト更新）。

### 8. ゲスト

- `guest/src/bin/sched_a.rs`: `a1` 表示 → busy-wait(~100ms) → `a2` → busy-wait → `a3` → exit(0)。busy-wait は cycle counter 読みまたは大きなカウンタ。
- `guest/src/bin/sched_b.rs`: `b1`..`b3` を短時間で表示して exit(7)。
- プリエンプションが効けば `b*` 行が `a1..a3` の間に挟まる。kernel 側は `switch_count >= 1` と両 proc 終了を確認して `[MINIOS_TEST] sched: ok` を出力。

## 既知の制限（文書化）

- `read` が syscall 内でブロックしている間は SIE=0 であり、タイマー割り込みも起きない。**stdin を読むプロセスがブロックすると全プロセスが停止する**。非同期化は将来課題。
- stdin staging は単一グローバル。どの proc の read でも同じストリームを消費する（先着順）。
- 単一 hart のみ。SMP は対象外。
- プロセス生成は boot 時の bundle のみ。動的 spawn/fork は対象外。

## エラー経路

- manifest v2 パース失敗 → bundle 拒否（従来の検証エラーと同じ扱い）。
- image 数 > 4 → 拒否。
- N 個目の spawn 失敗 → 確保済み proc を全 reclaim してエラー。
- proc の fatal trap → GuestError frame 送出済み → 該当 proc のみ Faulted 回収、他 proc は継続。
- 全 proc 終了 → 従来どおり shell 復帰（interactive）または test shutdown。

## テスト計画

### ホストテスト

- manifest v2: 複数 image パース、image 毎 arg、elf range 検証（重複・範囲外・欠落を拒否）、v1 互換。
- ProcessTable: spawn 上限、pick_next の round-robin 順序、Exited/Faulted の skip、全終了で None。
- Process::spawn の rollback（alloc 失敗注入で解放確認 — 既存 UserRun テストのパターンを踏襲）。

### QEMU テスト

- `cargo xtask test sched`: 2-image bundle、interleave した出力、`sched: ok` マーカー。
- 既存全経路が不変で通ること（v1 単一 image 互換）。

### ゲート

- `cargo xtask check`（31→32 phases 想定）全通過。

## 未決事項

- `ProcExit` payload に name を含めるか（pid のみ + Diagnostic で十分か）。
- kernel stack は現行 4 ページのまま（proc 毎に 16 KiB × 4 proc = 64 KiB）。
- switch_count の数え方（実切替のみカウント — pick が同一 pid を返した再開は含めない）。

## 作業の流れ（本 PR 以降の定常プロトコル）

1. ブランチ作成 → 実装 → `cargo xtask check` 全緑。
2. PR 作成（変更理由中心の説明 + 検証記録）。
3. セルフレビュー: diff 精読、設計との整合、境界条件、unsafe/rollback の安全性コメント。
4. CI `check` 成功を確認してから merge（`gh pr merge --merge`）。
5. merge 後に main で CI が緑であることを確認。
6. 次のゴールを roadmap と照らして選定し繰り返す。機能間の隙間には小さな harness 整備・リファクタリング PR を挟む。
