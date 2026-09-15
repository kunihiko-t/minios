# 実装計画: プロセス管理・プリエンプティブスケジューラ

設計: `.devin/specs/2026-09-15-process-scheduler.md`

## PR-A: ABI manifest v2 + ProcExit + multi-image bundle

1. `abi/src/manifest.rs`: `version=2` の image セクションパース。
   - `image=<name>` がセクション開始、`arg=`/`elf=<off>,<len>` は直近 image に属する。
   - `Manifest::images()` iterator（v1 は単一要素）。検証は parse 時に eager。
   - 上限 `MAX_PROCS=4`、elf range の重複/範囲外/欠落を拒否。
   - TDD: v2 parse、per-image args、range 検証、v1 互換、エラー系。
2. `abi/src/control.rs`: `FrameKind::ProcExit = 8`（payload 8B: u32 pid + u32 code）。
   - decode の固定長チェック追加。
3. `xtask/src/bundle.rs` + `cli.rs`: 複数 `--image name=path` 対応、manifest v2 生成。
   - 単一 image 指定時は従来通り v1 manifest を生成（互換維持）。
4. ホストテスト全緑 + `cargo xtask check`。実行経路は変えないので QEMU 影響なし。

## PR-B: Process/ProcessTable + プリエンプション + sched テスト

1. `kernel/src/process.rs`: `Process`（image/memory/space/kernel_stack/context/state の所有権）+ `spawn` rollback。`ProcessTable`（固定 slot、`pick_next` RR、`mark_exited`/`mark_faulted`/`reclaim`）。ホストテスト。
2. `kernel/src/user/run.rs` + `trap.rs`: `RunOutcome::Preempted`、timer interrupt → `time::handle_interrupt()` + Preempted。sched 用 trap handler variant。
3. `kernel/src/main.rs`: `run_boot_payload` を multi-proc ループ化。probe statics の per-dispatch 設定、context copy-back、ProcExit 送出、STIE 実行窓有効化。
4. `guest/src/bin/sched_a.rs` / `sched_b.rs` + `qemu-test-sched` feature + xtask phase + 2-image bundle 配線。
5. docs: roadmap、architecture.md、minicontainer-abi.md、guide 16 or 新章、11章の phase 数更新。
6. 全ゲート → PR → merge → CI 確認。

## 検証

- 各ステップで RED→GREEN（TDD）。
- PR毎に `cargo xtask check` 全緑がマージ条件。
