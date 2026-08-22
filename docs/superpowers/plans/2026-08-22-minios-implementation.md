# MiniOS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rust製のRISC-V 64学習用カーネルを、QEMU上のOpenSBIから起動し、UARTシェル、トラップ、タイマー、物理ページ管理、再現可能なテストハーネス、日本語教材まで完成させる。

**Architecture:** `#![no_std]` カーネルを `riscv64gc-unknown-none-elf` 向けにビルドし、QEMU `virt` 上でSモードとして動かす。ハードウェア非依存の処理はカーネルライブラリへ分離してホスト単体テストを行い、実機依存部分はUART出力を捕捉するQEMU統合テストで検証する。ホスト側操作はRust製の `cargo xtask` に統一する。

**Tech Stack:** Rust 1.98.0 stable、Cargo workspace、RISC-V RV64GC、OpenSBI、QEMU `virt`、16550互換UART、GitHub Actions

**Spec:** `docs/superpowers/specs/2026-08-22-minios-design.md`

## Global Constraints

- Rust stableのみを使い、nightly固有機能には依存しない。
- Rustは公式最新stableの1.98.0へ更新して固定し、Goはプロジェクトでは使わないが既存asdf環境を公式最新の1.27.0へ更新する。
- 主なホスト環境はApple Silicon搭載macOSとし、同じCargo入口をLinux CIでも使う。
- ゲストターゲットは `riscv64gc-unknown-none-elf`、QEMU machineは `virt`、CPUは1ハートとする。
- RAMは128 MiB（`0x8000_0000..0x8800_0000`）、カーネルのリンク・ロードアドレスは `0x8020_0000` とする。
- UART MMIOベースは `0x1000_0000`、タイムベース周波数はQEMU `virt` の10 MHzとする。
- カーネルの中核機能は自作し、カーネルcrateには外部依存crateを追加しない。
- RISC-Vアセンブリ、CSR、SBI、MMIO、カーネル実行ループは `cfg(target_arch = "riscv64")` で隔離し、パーサ、時刻変換、ページ管理などの純粋ロジックはホストでコンパイル・テスト可能に保つ。
- 動的ヒープ、仮想メモリ、ユーザーモード、プロセス、ファイルシステム、ネットワーク、Docker、実機対応は含めない。
- コード識別子は英語、教材・設計意図・安全条件のコメントは日本語とする。
- すべての `unsafe` に呼び出し条件または安全性の根拠を隣接コメントとして残す。
- 各タスクは失敗するテストを先に確認し、最小実装、全体回帰テスト、コミットの順で完了する。

---

## File Map

### Workspace and host tooling

- `Cargo.toml`: workspace、共通profile、release時のpanic設定。
- `rust-toolchain.toml`: Rust 1.98.0、rustfmt、Clippy、RISC-Vターゲットを固定。
- `.cargo/config.toml`: `cargo xtask` エイリアス。
- `.gitignore`: Cargo成果物と一時ログを除外。
- `xtask/src/cli.rs`: CLI解析とヘルプ。
- `xtask/src/tools.rs`: rustc、rustup、QEMUの検出とバージョン検証。
- `xtask/src/cargo.rs`: カーネルのbuild、format、Clippy、test呼び出し。
- `xtask/src/qemu.rs`: QEMU引数、子プロセス、タイムアウト、UARTログ捕捉、入力注入。
- `xtask/src/docs.rs`: Markdown内のローカルリンク検査。
- `xtask/src/lib.rs`: xtaskの公開境界とコマンドディスパッチ。
- `xtask/src/main.rs`: 終了コードとエラー表示だけを担当。

### Kernel

- `kernel/linker.ld`: `0x8020_0000` からのセクション、BSS、カーネル終端を定義。
- `kernel/src/arch/riscv64/entry.S`: スタック設定、BSSクリア、Rustへの遷移。
- `kernel/src/arch/riscv64/trap.S`: 汎用レジスタ保存・復帰と `sret`。
- `kernel/src/arch/riscv64/csr.rs`: CSRの型付き読み書き。
- `kernel/src/arch/riscv64/sbi.rs`: TIME、SRST拡張のSBI呼び出し。
- `kernel/src/arch/riscv64/trap.rs`: `scause` 解釈とトラップディスパッチ。
- `kernel/src/drivers/uart.rs`: 16550互換UARTの1バイト入出力。
- `kernel/src/console.rs`: 書式付き出力、入力、緊急出力。
- `kernel/src/time.rs`: タイマー予約、tick、稼働時間。
- `kernel/src/memory/frame.rs`: ビットマップ方式の物理ページ確保・解放。
- `kernel/src/shell/line.rs`: 固定長行バッファ。
- `kernel/src/shell/command.rs`: コマンド解析。
- `kernel/src/shell/mod.rs`: UART対話ループとコマンド実行。
- `kernel/src/lib.rs`: ホストテスト可能な純粋ロジックを公開。
- `kernel/src/main.rs`: 初期化順序、panic、テストモード、シェル開始。

### Documentation

- `README.md`: 概要、最短起動、教材索引。
- `docs/guide/01-introduction.md` から `12-next-steps.md`: 段階的ハンズオン。
- `docs/reference/architecture.md`: モジュール境界と起動フロー。
- `docs/reference/memory-map.md`: QEMU `virt` の利用範囲。
- `docs/reference/glossary.md`: RISC-V、Rust、OS用語。
- `docs/reference/troubleshooting.md`: 症状、確認コマンド、原因、対処。
- `docs/reference/roadmap.md`: 初回スコープ外の発展順序。
- `.github/workflows/ci.yml`: Linux上の `cargo xtask check`。

---

### Task 1: Workspace and Environment Harness

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.cargo/config.toml`
- Create: `.gitignore`
- Create: `kernel/Cargo.toml`
- Create: `kernel/src/lib.rs`
- Create: `xtask/Cargo.toml`
- Create: `xtask/src/lib.rs`
- Create: `xtask/src/main.rs`
- Create: `xtask/src/cli.rs`
- Create: `xtask/src/tools.rs`
- Create: `docs/guide/01-introduction.md`
- Create: `docs/guide/02-setup.md`
- Test: unit tests inside `xtask/src/cli.rs` and `xtask/src/tools.rs`

**Interfaces:**
- Consumes: design constants from the specification only.
- Produces: `cli::Command::Setup`, `cli::parse(&[String]) -> Result<Command, CliError>`, `tools::Version { major, minor, patch }`, `tools::parse_qemu_version(&str) -> Result<Version, ToolError>`, and `run(Command) -> Result<(), XtaskError>`.

- [ ] **Step 1: Add the workspace and a failing CLI parser test**

Create the workspace manifests, pin the toolchain, define `.cargo/config.toml` with `xtask = "run --package xtask --"`, and add this test before defining `parse`:

```rust
#[test]
fn parses_setup_command() {
    let args = vec!["setup".to_owned()];
    assert_eq!(parse(&args), Ok(Command::Setup));
}

#[test]
fn rejects_unknown_command_with_helpful_name() {
    let args = vec!["unknown".to_owned()];
    assert_eq!(
        parse(&args),
        Err(CliError::UnknownCommand("unknown".to_owned()))
    );
}
```

- [ ] **Step 2: Verify the parser test fails for the intended reason**

Run: `cargo test -p xtask cli::tests -- --nocapture`

Expected: compilation fails because `parse` and the CLI types do not yet exist.

- [ ] **Step 3: Implement CLI parsing and QEMU version parsing**

Use these exact public types and accept only `setup` at this stage:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingCommand,
    UnknownCommand(String),
}

pub fn parse(args: &[String]) -> Result<Command, CliError> {
    match args {
        [command] if command == "setup" => Ok(Command::Setup),
        [] => Err(CliError::MissingCommand),
        [command, ..] => Err(CliError::UnknownCommand(command.clone())),
    }
}
```

In `tools.rs`, represent versions as `Version { major: u32, minor: u32, patch: u32 }`. Parse the first line of `QEMU emulator version X.Y.Z`, reject malformed strings, and require QEMU 9.0.0 or newer. `setup` must check `rustc --version`, `rustup target list --installed`, and `qemu-system-riscv64 --version`, then print the detected versions and a precise install command when a tool is absent.

- [ ] **Step 4: Run unit tests and the environment check**

Run: `cargo test -p xtask`

Expected: all CLI and version parser tests pass.

Run: `cargo xtask setup`

Expected before QEMU installation: nonzero exit and a message that names `brew install qemu` on macOS.

- [ ] **Step 5: Install QEMU and verify the required tools**

Run: `rustup update stable`

Expected: stable toolchain is Rust 1.98.0. Set `rust-toolchain.toml` to channel `1.98.0` after this succeeds.

Run: `asdf plugin update golang`

Run: `asdf install golang 1.27.0`

Run: `asdf global golang 1.27.0`

Expected: `go version` reports `go1.27.0 darwin/arm64`. Go remains outside the MiniOS build graph.

Run: `brew update`

Run: `brew install qemu`

If Homebrew reports QEMU is already installed, run `brew upgrade qemu` only when `brew outdated qemu` lists it. Do not upgrade unrelated formulae.

Run: `cargo xtask setup`

Expected: Rust 1.98.0, the RISC-V target, and QEMU 9.0.0 or newer are reported as available. Record the exact QEMU first-line version output in `docs/guide/02-setup.md` as the verified macOS environment.

- [ ] **Step 6: Write the first two guide chapters**

`01-introduction.md` must explain the milestone, prerequisites, included concepts, excluded features, and the order in which chapters build on one another. `02-setup.md` must show `cargo xtask setup`, explain each detected tool, include the verified QEMU version, and list fixes for a missing Rust target and missing QEMU.

- [ ] **Step 7: Run checks and commit**

Run: `cargo fmt --all -- --check`

Run: `cargo test -p xtask`

Run: `cargo xtask setup`

Expected: all commands exit 0.

```bash
git add Cargo.toml rust-toolchain.toml .cargo/config.toml .gitignore kernel xtask docs/guide/01-introduction.md docs/guide/02-setup.md
git commit -m "build: scaffold MiniOS workspace and environment checks"
```

---

### Task 2: Bootable Kernel, UART, and QEMU Smoke Test

**Files:**
- Modify: `Cargo.toml`
- Modify: `kernel/Cargo.toml`
- Modify: `kernel/src/lib.rs`
- Modify: `xtask/src/cli.rs`
- Modify: `xtask/src/lib.rs`
- Create: `kernel/build.rs`
- Create: `kernel/linker.ld`
- Create: `kernel/src/main.rs`
- Create: `kernel/src/arch/mod.rs`
- Create: `kernel/src/arch/riscv64/mod.rs`
- Create: `kernel/src/arch/riscv64/entry.S`
- Create: `kernel/src/arch/riscv64/sbi.rs`
- Create: `kernel/src/drivers/mod.rs`
- Create: `kernel/src/drivers/uart.rs`
- Create: `kernel/src/console.rs`
- Create: `xtask/src/cargo.rs`
- Create: `xtask/src/qemu.rs`
- Create: `docs/guide/03-no-std-and-linking.md`
- Create: `docs/guide/04-boot-with-opensbi.md`
- Create: `docs/guide/05-uart.md`
- Test: unit tests in `xtask/src/qemu.rs`; QEMU marker test through `cargo xtask test boot`

**Interfaces:**
- Consumes: Task 1 CLI and tool detection.
- Produces: `Uart::qemu_virt() -> Uart`, `Uart::write_byte(&mut self, u8)`, `Uart::read_byte(&mut self) -> u8`, `Uart::has_byte(&self) -> bool`, `console::_print(fmt::Arguments<'_>)`, `sbi::SbiRet { error: isize, value: usize }`, `sbi::SbiError(isize)`, `SbiRet::into_result() -> Result<usize, SbiError>`, `sbi::system_reset(ResetType, ResetReason) -> !`, `qemu::TestKind::Boot`, `qemu::run_test(TestKind, Duration) -> Result<String, QemuError>`, and CLI commands `build`, `run`, `test boot`.

- [ ] **Step 1: Add a failing QEMU argument test**

```rust
#[test]
fn test_qemu_args_are_headless_and_single_hart() {
    let args = qemu_args(Path::new("kernel.elf"));
    assert!(contains_pair(&args, "-machine", "virt"));
    assert!(contains_pair(&args, "-m", "128M"));
    assert!(contains_pair(&args, "-smp", "1"));
    assert!(contains_pair(&args, "-bios", "default"));
    assert!(contains_pair(&args, "-kernel", "kernel.elf"));
    assert!(contains_pair(&args, "-serial", "stdio"));
    assert!(contains_pair(&args, "-monitor", "none"));
    assert!(contains_pair(&args, "-display", "none"));
}
```

- [ ] **Step 2: Verify the QEMU test fails**

Run: `cargo test -p xtask qemu::tests::test_qemu_args_are_headless_and_single_hart`

Expected: compilation fails because `qemu_args` does not exist.

- [ ] **Step 3: Implement the ELF layout and entry code**

Set `ENTRY(_start)` and start sections at `0x80200000`. Export `__bss_start`, `__bss_end`, and `__kernel_end`, align sections to 4 KiB, and reserve a 64 KiB boot stack. The entry assembly must preserve OpenSBI's `a0` hart ID and `a1` DTB pointer while clearing BSS:

```asm
.section .text.entry
.globl _start
_start:
    csrw sie, zero
    la sp, __boot_stack_end
    la t0, __bss_start
    la t1, __bss_end
1:
    bgeu t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:
    call kernel_main
3:
    wfi
    j 3b
```

`build.rs` must pass the absolute linker-script path only to the `minios-kernel` binary and emit `rerun-if-changed=linker.ld`.

- [ ] **Step 4: Implement UART, console output, SBI reset, and kernel entry**

Use volatile MMIO at `0x1000_0000`; UART Line Status Register offset 5 bit 5 means transmit-ready and bit 0 means receive-ready. Implement `core::fmt::Write` for `Uart`, and provide `print!` and `println!` macros through `console::_print`.

The SBI call boundary must use registers `a0..a2` for arguments, `a6` for function ID, `a7` for extension ID, and return `a0/a1` as `SbiRet { error, value }`. `SbiRet::into_result` returns `Ok(value)` only when `error == 0`; all other codes become `SbiError(error)`. Use SRST extension `0x53525354`, function 0 for shutdown. If system reset unexpectedly returns an error, print the numeric SBI error through emergency UART and enter an interrupt-disabled `wfi` loop.

`kernel_main(hart_id: usize, dtb: usize) -> !` prints `MiniOS booting...` and the hart ID. With feature `qemu-test-boot`, it also prints `[MINIOS_TEST] boot: ok` and requests a successful SBI shutdown; without the feature it waits with `wfi` until the shell exists.

- [ ] **Step 5: Implement build, run, and boot-test harness paths**

Build with this command shape:

```text
cargo build -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf
```

For the boot test, add `--features qemu-test-boot`, capture UART output, enforce a five-second deadline, and require both a zero QEMU exit status and the exact marker `[MINIOS_TEST] boot: ok`. On timeout, kill QEMU, wait for it, and return all captured stdout and stderr.

- [ ] **Step 6: Verify boot behavior**

Run: `cargo xtask build`

Expected: `target/riscv64gc-unknown-none-elf/debug/minios-kernel` is created.

Run: `cargo xtask test boot`

Expected: OpenSBI output is followed by the MiniOS banner and `[MINIOS_TEST] boot: ok`; the command exits 0.

- [ ] **Step 7: Write the boot and UART chapters**

Explain `no_std`, panic-abort, linker sections, OpenSBI's `a0/a1` contract, why BSS is cleared before Rust code, MMIO volatility, each UART status bit used, and how `cargo xtask test boot` distinguishes a crash from a timeout. Include the successful banner as expected output.

- [ ] **Step 8: Run regression checks and commit**

Run: `cargo test -p xtask`

Run: `cargo xtask test boot`

Run: `cargo fmt --all -- --check`

Expected: all commands exit 0.

```bash
git add Cargo.toml kernel xtask docs/guide/03-no-std-and-linking.md docs/guide/04-boot-with-opensbi.md docs/guide/05-uart.md
git commit -m "feat: boot MiniOS and provide UART output"
```

---

### Task 3: Panic Diagnostics and Trap Handling

**Files:**
- Modify: `kernel/Cargo.toml`
- Modify: `kernel/src/main.rs`
- Modify: `kernel/src/arch/riscv64/mod.rs`
- Modify: `xtask/src/cli.rs`
- Modify: `xtask/src/qemu.rs`
- Create: `kernel/src/arch/riscv64/csr.rs`
- Create: `kernel/src/arch/riscv64/trap.rs`
- Create: `kernel/src/arch/riscv64/trap.S`
- Create: `docs/guide/06-panic-and-diagnostics.md`
- Create: `docs/guide/07-traps-and-interrupts.md`
- Test: unit tests in `kernel/src/arch/riscv64/trap.rs`; QEMU marker test through `cargo xtask test trap`

**Interfaces:**
- Consumes: Task 2 console and SBI reset.
- Produces: `TrapCause::{Interrupt(usize), Exception(usize)}`, `decode_scause(usize) -> TrapCause`, `trap::init()`, `extern "C" fn rust_trap_handler()`, CSR read/write helpers, `qemu::TestKind::Trap`, and `test trap`.

- [ ] **Step 1: Write failing `scause` decoding tests**

```rust
#[test]
fn decodes_supervisor_timer_interrupt() {
    let value = (1usize << (usize::BITS - 1)) | 5;
    assert_eq!(decode_scause(value), TrapCause::Interrupt(5));
}

#[test]
fn decodes_breakpoint_exception() {
    assert_eq!(decode_scause(3), TrapCause::Exception(3));
}
```

- [ ] **Step 2: Verify decoding tests fail**

Run: `cargo test -p minios-kernel --lib trap::tests`

Expected: compilation fails because `decode_scause` and `TrapCause` are missing.

- [ ] **Step 3: Implement CSR access and trap frame assembly**

Provide `read_scause`, `read_sepc`, `write_sepc`, `read_stval`, `write_stvec`, `read_sstatus`, `write_sstatus`, `read_sie`, and `write_sie`. Every CSR write function must be `unsafe` and document the required privilege mode and bit invariants.

The assembly entry reserves a 256-byte, 16-byte-aligned frame and saves x1 plus x3 through x31 before calling `rust_trap_handler`; it restores the same registers, releases the frame, and executes `sret`. The interrupted stack pointer is recovered by adding 256, so x2 is not stored as a separate mutable register slot.

- [ ] **Step 4: Implement trap decoding and diagnostics**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCause {
    Interrupt(usize),
    Exception(usize),
}

pub fn decode_scause(value: usize) -> TrapCause {
    let interrupt_bit = 1usize << (usize::BITS - 1);
    let code = value & !interrupt_bit;
    if value & interrupt_bit == 0 {
        TrapCause::Exception(code)
    } else {
        TrapCause::Interrupt(code)
    }
}
```

`trap::init` writes the assembly entry address to `stvec`. Unexpected traps must print the decoded cause plus hexadecimal `scause`, `sepc`, and `stval`, then request failure reset. Under `qemu-test-trap`, `kernel_main` executes `ebreak`; the breakpoint path prints `[MINIOS_TEST] trap: ok` and requests successful reset.

Keep `arch::riscv64` visible to host unit tests, but guard `global_asm!`, inline-assembly CSR functions, and the runtime dispatcher with `cfg(target_arch = "riscv64")`; `TrapCause` and `decode_scause` remain target-independent.

The panic handler must use direct emergency UART output, include message, file, line, and hart ID when present, and then request failure reset. It must not take a lock that an interrupted formatter could already own.

- [ ] **Step 5: Run host and QEMU tests**

Run: `cargo test -p minios-kernel --lib trap::tests`

Expected: both decode tests pass.

Run: `cargo xtask test trap`

Expected: output contains `[MINIOS_TEST] trap: ok`, and the command exits 0 before the five-second deadline.

- [ ] **Step 6: Write diagnostic and trap chapters**

Document synchronous exceptions versus asynchronous interrupts, direct versus vectored `stvec`, why all caller-visible registers are preserved, the meaning of `scause/sepc/stval`, panic lock avoidance, and an exercise that replaces `ebreak` with an illegal instruction and predicts the changed cause code.

- [ ] **Step 7: Run regression checks and commit**

Run: `cargo test -p minios-kernel --lib`

Run: `cargo test -p xtask`

Run: `cargo xtask test boot`

Run: `cargo xtask test trap`

Expected: all commands exit 0.

```bash
git add kernel xtask docs/guide/06-panic-and-diagnostics.md docs/guide/07-traps-and-interrupts.md
git commit -m "feat: diagnose panics and RISC-V traps"
```

---

### Task 4: Supervisor Timer Interrupts

**Files:**
- Modify: `kernel/Cargo.toml`
- Modify: `kernel/src/main.rs`
- Modify: `kernel/src/arch/riscv64/csr.rs`
- Modify: `kernel/src/arch/riscv64/sbi.rs`
- Modify: `kernel/src/arch/riscv64/trap.rs`
- Modify: `kernel/src/lib.rs`
- Modify: `xtask/src/cli.rs`
- Modify: `xtask/src/qemu.rs`
- Create: `kernel/src/time.rs`
- Create: `docs/guide/08-timer-interrupts.md`
- Test: unit tests in `kernel/src/time.rs`; QEMU marker test through `cargo xtask test timer`

**Interfaces:**
- Consumes: Task 3 CSR and trap dispatcher; Task 2 SBI call primitive.
- Produces: `sbi::set_timer(u64) -> Result<usize, SbiError>`, `time::init() -> Result<(), SbiError>`, `time::handle_interrupt() -> Result<(), SbiError>`, `time::ticks() -> u64`, `time::uptime_millis() -> u64`, `ticks_to_millis(u64) -> u64`, `qemu::TestKind::Timer`, and `test timer`.

- [ ] **Step 1: Write failing time conversion tests**

```rust
#[test]
fn converts_ticks_to_milliseconds() {
    assert_eq!(ticks_to_millis(0), 0);
    assert_eq!(ticks_to_millis(1), 10);
    assert_eq!(ticks_to_millis(250), 2_500);
}
```

- [ ] **Step 2: Verify the conversion test fails**

Run: `cargo test -p minios-kernel --lib time::tests`

Expected: compilation fails because `ticks_to_millis` is missing.

- [ ] **Step 3: Implement timer state and SBI scheduling**

Use `TIMEBASE_HZ = 10_000_000`, `TICKS_PER_SECOND = 100`, and `CYCLES_PER_TICK = 100_000`. Keep the observed tick count in `AtomicU64`. Read the `time` CSR, schedule `now + CYCLES_PER_TICK` through SBI TIME extension `0x54494D45`, enable STIE bit 5 in `sie`, and finally enable SIE bit 1 in `sstatus`.

```rust
pub fn ticks_to_millis(ticks: u64) -> u64 {
    ticks.saturating_mul(1_000) / TICKS_PER_SECOND
}
```

`handle_interrupt` increments the atomic counter and schedules the next absolute deadline. The trap dispatcher handles only interrupt code 5 as a timer; all other causes retain Task 3 diagnostics.

Guard SBI/CSR-dependent functions with `cfg(target_arch = "riscv64")`; keep `ticks_to_millis` and its constants available to host tests.

- [ ] **Step 4: Add the timer QEMU test**

Under `qemu-test-timer`, wait until `time::ticks() >= 3`, print `[MINIOS_TEST] timer: ok`, and request successful reset. Extend xtask with `test timer` using the same five-second timeout and exact marker check.

- [ ] **Step 5: Run time tests**

Run: `cargo test -p minios-kernel --lib time::tests`

Run: `cargo xtask test timer`

Expected: host conversions pass and QEMU observes at least three supervisor timer interrupts before printing the marker.

- [ ] **Step 6: Write the timer chapter**

Explain the `time` CSR, OpenSBI TIME call, STIE/SIE ordering, absolute deadlines, why the handler rearms the timer, atomic ordering choice for one writer and readers, and how 10 MHz becomes a 100 Hz tick.

- [ ] **Step 7: Run regression checks and commit**

Run: `cargo test -p minios-kernel --lib`

Run: `cargo xtask test boot`

Run: `cargo xtask test trap`

Run: `cargo xtask test timer`

Expected: all commands exit 0.

```bash
git add kernel xtask docs/guide/08-timer-interrupts.md
git commit -m "feat: handle supervisor timer interrupts"
```

---

### Task 5: Bitmap Physical Page Allocator

**Files:**
- Modify: `kernel/Cargo.toml`
- Modify: `kernel/src/lib.rs`
- Modify: `kernel/src/main.rs`
- Modify: `xtask/src/cli.rs`
- Modify: `xtask/src/qemu.rs`
- Create: `kernel/src/memory/mod.rs`
- Create: `kernel/src/memory/frame.rs`
- Create: `docs/guide/09-physical-memory.md`
- Test: unit tests in `kernel/src/memory/frame.rs`; QEMU marker test through `cargo xtask test memory`

**Interfaces:**
- Consumes: linker symbol `__kernel_end`, Task 2 console, Task 4 initialized timer.
- Produces: `PhysFrame::from_start(usize) -> Result<PhysFrame, FrameError>`, `PhysFrame::start(self) -> usize`, `FrameAllocator::<WORDS>::new(usize, usize) -> Result<Self, FrameError>`, `allocate(&mut self) -> Option<PhysFrame>`, `deallocate(&mut self, PhysFrame) -> Result<(), FrameError>`, `stats(&self) -> FrameStats`, `qemu::TestKind::Memory`, and `test memory`.

- [ ] **Step 1: Write failing allocator tests**

```rust
#[test]
fn allocates_and_reuses_a_frame() {
    let mut allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    assert_eq!(first.start(), 0x4000);
    assert_eq!(second.start(), 0x5000);
    allocator.deallocate(first).unwrap();
    assert_eq!(allocator.allocate().unwrap(), first);
}

#[test]
fn rejects_double_free_and_out_of_range_frames() {
    let mut allocator = FrameAllocator::<1>::new(0x4000, 0x8000).unwrap();
    let frame = allocator.allocate().unwrap();
    allocator.deallocate(frame).unwrap();
    assert_eq!(allocator.deallocate(frame), Err(FrameError::DoubleFree));
    assert_eq!(
        allocator.deallocate(PhysFrame::from_start(0x9000).unwrap()),
        Err(FrameError::OutOfRange)
    );
}
```

- [ ] **Step 2: Verify allocator tests fail**

Run: `cargo test -p minios-kernel --lib memory::frame::tests`

Expected: compilation fails because allocator types are missing.

- [ ] **Step 3: Implement the allocator contracts**

```rust
pub const PAGE_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysFrame(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    EmptyRange,
    Unaligned,
    CapacityExceeded,
    OutOfRange,
    DoubleFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub total: usize,
    pub allocated: usize,
    pub free: usize,
}

pub struct FrameAllocator<const WORDS: usize> {
    base: usize,
    frame_count: usize,
    allocated: usize,
    bitmap: [u64; WORDS],
}
```

`new` requires page-aligned nonempty bounds and rejects more than `WORDS * 64` frames. `allocate` scans frame indices from zero, sets the first clear bit, increments `allocated`, and returns its physical address. `deallocate` validates alignment and range, rejects a clear bit as `DoubleFree`, clears the bit, and decrements `allocated`. `stats` reports exact totals without scanning the bitmap.

- [ ] **Step 4: Initialize physical memory in the kernel**

Read `__kernel_end` through an `extern "C"` linker symbol, round it upward to 4096, and construct `FrameAllocator::<512>` with end `0x8800_0000`. Keep the allocator as a local owned value in `kernel_main` and later pass `&mut FrameAllocator<512>` to the shell; do not introduce a global mutable allocator.

Print one initialization status line after traps, timer, and physical memory each become usable. The normal successful sequence must include `[ok] traps`, `[ok] timer`, and `[ok] memory` before entering the shell.

Under `qemu-test-memory`, allocate two frames, verify distinct page-aligned addresses, free the first, verify it is reused, print `[MINIOS_TEST] memory: ok`, and request successful reset.

- [ ] **Step 5: Run allocator tests**

Run: `cargo test -p minios-kernel --lib memory::frame::tests`

Run: `cargo xtask test memory`

Expected: unit edge cases pass and the QEMU marker confirms the real linker-derived range works.

- [ ] **Step 6: Write the physical-memory chapter**

Explain physical versus virtual addresses, 4 KiB alignment, why OpenSBI and the kernel image are excluded, bitmap size for 128 MiB, first-fit behavior, double-free detection, why the allocator remains locally owned, and exercises for exhaustion and fragmentation observation.

- [ ] **Step 7: Run regression checks and commit**

Run: `cargo test -p minios-kernel --lib`

Run: `cargo test -p xtask`

Run: `cargo xtask test boot`

Run: `cargo xtask test trap`

Run: `cargo xtask test timer`

Run: `cargo xtask test memory`

Expected: all commands exit 0.

```bash
git add kernel xtask docs/guide/09-physical-memory.md
git commit -m "feat: manage physical pages with a bitmap"
```

---

### Task 6: Interactive Shell

**Files:**
- Modify: `kernel/src/lib.rs`
- Modify: `kernel/src/main.rs`
- Modify: `kernel/src/console.rs`
- Modify: `xtask/src/cli.rs`
- Modify: `xtask/src/qemu.rs`
- Create: `kernel/src/shell/mod.rs`
- Create: `kernel/src/shell/line.rs`
- Create: `kernel/src/shell/command.rs`
- Create: `docs/guide/10-shell.md`
- Test: unit tests in `kernel/src/shell/line.rs` and `kernel/src/shell/command.rs`; interactive QEMU test through `cargo xtask test shell`

**Interfaces:**
- Consumes: Task 2 UART/console/SBI, Task 4 uptime, Task 5 `FrameAllocator<512>`.
- Produces: `LineBuffer::<N>::new() -> Self`, `push(&mut self, u8) -> Result<(), LineError>`, `backspace(&mut self) -> Option<u8>`, `as_str(&self) -> &str`, `clear(&mut self)`; `parse_command(&str) -> Command<'_>`; `shell::run(usize, &mut FrameAllocator<512>) -> !`; `qemu::TestKind::Shell`; and `test shell`.

- [ ] **Step 1: Write failing line-buffer and parser tests**

```rust
#[test]
fn line_buffer_handles_text_backspace_and_capacity() {
    let mut line = LineBuffer::<3>::new();
    assert_eq!(line.push(b'a'), Ok(()));
    assert_eq!(line.push(b'b'), Ok(()));
    assert_eq!(line.backspace(), Some(b'b'));
    assert_eq!(line.as_str(), "a");
    assert_eq!(line.push(b'c'), Ok(()));
    assert_eq!(line.push(b'd'), Ok(()));
    assert_eq!(line.push(b'e'), Err(LineError::Full));
}

#[test]
fn parser_trims_and_recognizes_commands() {
    assert_eq!(parse_command("  uptime  "), Command::Uptime);
    assert_eq!(parse_command("memory"), Command::Memory);
    assert_eq!(parse_command("wat"), Command::Unknown("wat"));
}
```

- [ ] **Step 2: Verify shell unit tests fail**

Run: `cargo test -p minios-kernel --lib shell::`

Expected: compilation fails because the shell modules and types are missing.

- [ ] **Step 3: Implement fixed input and command parsing**

Define `LineBuffer<const N: usize> { bytes: [u8; N], len: usize, overflowed: bool }`. Accept printable ASCII only, echo accepted bytes, support Backspace/Delete, and mark the whole line invalid after the first capacity error. Enter returns either a valid UTF-8 ASCII slice or `LineError::Full`; clearing resets `len` and `overflowed`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    Empty,
    Help,
    Info,
    Uptime,
    Memory,
    Clear,
    Shutdown,
    Unknown(&'a str),
}
```

Trim ASCII whitespace and recognize exact lowercase command names. Preserve the trimmed input in `Unknown`.

- [ ] **Step 4: Implement the shell loop and commands**

`shell::run` prints `minios> `, blocks on UART input, echoes printable bytes, emits `\x08 \x08` for backspace, and uses a 128-byte `LineBuffer`. On overflow, print `error: input exceeds 128 bytes` and start a fresh prompt.

Command output must include these stable strings for integration tests:

```text
help      Show available commands
MiniOS 0.1.0 on RISC-V 64
uptime: <number> ms
memory: total=<number> allocated=<number> free=<number> pages
unknown command: <input>; try 'help'
```

`clear` emits `\x1b[2J\x1b[H`. `shutdown` prints `shutting down` before SBI reset. Pass the locally owned frame allocator by mutable reference even though the first commands only read `stats`; this keeps future allocator exercises explicit without global state.

- [ ] **Step 5: Add interactive QEMU testing**

`cargo xtask test shell` must spawn the normal kernel, wait for `minios> `, write `help\ninfo\nuptime\nmemory\nnot-a-command\nshutdown\n` to QEMU stdin, and require the stable strings above plus a zero exit within five seconds. Include the full transcript on mismatch.

- [ ] **Step 6: Run shell tests**

Run: `cargo test -p minios-kernel --lib shell::`

Run: `cargo xtask test shell`

Expected: unit tests pass and the full scripted UART session reaches `shutting down`.

- [ ] **Step 7: Write the shell chapter**

Explain polling I/O, fixed buffers without a heap, echo/backspace behavior, parsing lifetimes for `Unknown(&str)`, separation of parsing from command effects, stable output strings for testing, and exercises that add a read-only `ticks` command.

- [ ] **Step 8: Run regression checks and commit**

Run: `cargo test -p minios-kernel --lib`

Run: `cargo test -p xtask`

Run: `cargo xtask test boot`

Run: `cargo xtask test trap`

Run: `cargo xtask test timer`

Run: `cargo xtask test memory`

Run: `cargo xtask test shell`

Expected: all commands exit 0.

```bash
git add kernel xtask docs/guide/10-shell.md
git commit -m "feat: add the MiniOS UART shell"
```

---

### Task 7: Unified Checks and Linux CI

**Files:**
- Modify: `xtask/src/cli.rs`
- Modify: `xtask/src/lib.rs`
- Modify: `xtask/src/cargo.rs`
- Modify: `xtask/src/qemu.rs`
- Create: `docs/guide/11-test-harness.md`
- Create: `.github/workflows/ci.yml`
- Test: unit tests in `xtask/src/cli.rs`; full `cargo xtask check`

**Interfaces:**
- Consumes: every build and QEMU test command from Tasks 1–6.
- Produces: `cli::Command::{Setup, Build, Run, Test(TestFilter), Check}`, `cli::TestFilter::{All, Boot, Trap, Timer, Memory, Shell}`, and final CLI `setup`, `build`, `run`, `test [all|boot|trap|timer|memory|shell]`, and `check`.

- [ ] **Step 1: Write failing final CLI tests**

```rust
#[test]
fn parses_all_public_commands() {
    assert_eq!(parse(&owned(&["build"])), Ok(Command::Build));
    assert_eq!(parse(&owned(&["run"])), Ok(Command::Run));
    assert_eq!(parse(&owned(&["test"])), Ok(Command::Test(TestFilter::All)));
    assert_eq!(
        parse(&owned(&["test", "timer"])),
        Ok(Command::Test(TestFilter::Timer))
    );
    assert_eq!(parse(&owned(&["check"])), Ok(Command::Check));
}
```

- [ ] **Step 2: Verify final CLI tests fail**

Run: `cargo test -p xtask cli::tests::parses_all_public_commands`

Expected: the incomplete command enum or parser causes the test to fail.

- [ ] **Step 3: Implement deterministic command orchestration**

`test all` runs host kernel tests, xtask tests, then QEMU tests in order: boot, trap, timer, memory, shell. `check` runs this exact sequence and stops at the first failure:

```text
cargo fmt --all -- --check
cargo clippy -p xtask --all-targets -- -D warnings
cargo clippy -p minios-kernel --lib -- -D warnings
cargo clippy -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf -- -D warnings
cargo build -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf
cargo test -p minios-kernel --lib
cargo test -p xtask
QEMU boot, trap, timer, memory, shell tests
```

Avoid recursively invoking `cargo xtask` from inside xtask. Call the internal Rust functions for QEMU tests and spawn Cargo only for compiler-owned operations. Print a numbered phase header and elapsed time for each phase.

- [ ] **Step 4: Add GitHub Actions**

Use Ubuntu, install `qemu-system-misc`, install the toolchain declared by `rust-toolchain.toml`, cache only Cargo registry/git data and `target`, then run `cargo xtask setup` and `cargo xtask check`.

```yaml
name: CI
on:
  push:
  pull_request:
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: sudo apt-get update && sudo apt-get install -y qemu-system-misc
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.98.0
          targets: riscv64gc-unknown-none-elf
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo xtask setup
      - run: cargo xtask check
```

- [ ] **Step 5: Run the final harness locally**

Run: `cargo xtask setup`

Run: `cargo xtask check`

Expected: every numbered phase exits 0; QEMU tests finish without timeout; the final summary reports all phases passed.

- [ ] **Step 6: Write the harness chapter**

Explain the xtask pattern, why host and guest tests are separate, QEMU marker and interactive modes, timeout cleanup, how failure transcripts are preserved, the exact `check` phase order, and how the CI command is identical to local verification.

- [ ] **Step 7: Commit**

```bash
git add xtask docs/guide/11-test-harness.md .github/workflows/ci.yml
git commit -m "ci: unify MiniOS checks across host and QEMU"
```

---

### Task 8: Complete the Japanese Learning Material and Acceptance Review

**Files:**
- Create: `README.md`
- Create: `docs/guide/README.md`
- Create: `docs/guide/12-next-steps.md`
- Create: `docs/reference/architecture.md`
- Create: `docs/reference/memory-map.md`
- Create: `docs/reference/glossary.md`
- Create: `docs/reference/troubleshooting.md`
- Create: `docs/reference/roadmap.md`
- Create: `xtask/src/docs.rs`
- Modify: `xtask/src/lib.rs`
- Test: unit tests in `xtask/src/docs.rs`; final `cargo xtask check`; manual shell acceptance session

**Interfaces:**
- Consumes: all completed code and stable command output.
- Produces: `docs::check_local_links(root: &Path) -> Result<(), DocsError>`, `docs::check_guide_structure(root: &Path) -> Result<(), DocsError>`, complete reader navigation, and a verified milestone.

- [ ] **Step 1: Write a failing local-link checker test**

```rust
#[test]
fn reports_a_missing_relative_markdown_link() {
    let temp = TestTree::new();
    temp.write("README.md", "[missing](docs/missing.md)\n");
    let error = check_local_links(temp.path()).unwrap_err();
    assert_eq!(error.path(), Path::new("docs/missing.md"));
}

#[test]
fn accepts_existing_relative_markdown_links_and_anchors() {
    let temp = TestTree::new();
    temp.write("README.md", "[guide](docs/guide.md#start)\n");
    temp.write("docs/guide.md", "# Start\n");
    assert_eq!(check_local_links(temp.path()), Ok(()));
}

#[test]
fn rejects_a_guide_chapter_missing_required_sections() {
    let temp = TestTree::new();
    temp.write("docs/guide/01-example.md", "# Example\n## 学習目標\n");
    let error = check_guide_structure(temp.path()).unwrap_err();
    assert_eq!(error.missing_section(), "背景");
}
```

- [ ] **Step 2: Verify the documentation test fails**

Run: `cargo test -p xtask docs::tests`

Expected: compilation fails because `check_local_links` and the test helper are missing.

- [ ] **Step 3: Implement local Markdown link validation**

Walk `.md` files below the repository root while excluding `.git/` and `target/`, extract inline Markdown destinations, ignore `http:`, `https:`, and `mailto:`, remove anchors before filesystem lookup, resolve relative to the source file, and report source file, destination, and line number for missing paths. Also require every numbered guide chapter to contain the sections `学習目標`, `背景`, `実装`, `実行と確認`, `よくある失敗`, `演習`, and `次の章`. Add both checks after format and before compilation in `cargo xtask check`.

- [ ] **Step 4: Write the README and guide index**

The root README must contain project purpose, a five-command quick start, expected `minios> ` output, supported host/guest matrix, chapter index, architecture/reference links, test command, current limitations, and license status. The guide index must list Chapters 1–12 with one learning outcome each and provide previous/next navigation conventions.

- [ ] **Step 5: Write references and the final chapter**

`architecture.md` must describe every module interface and the eight-step boot flow. `memory-map.md` must tabulate OpenSBI-reserved RAM, kernel start, kernel end symbol, allocatable RAM, UART MMIO, and the 128 MiB upper bound. `glossary.md` must define ABI, BSS, CSR, DTB, hart, ISA, MMIO, OpenSBI, page, privilege mode, SBI, trap, UART, and volatile access.

日本語のトラブルシューティングとして、`troubleshooting.md` must cover missing target, missing QEMU, linker overlap, no UART output, immediate trap, timer timeout, shell input overflow, stuck QEMU, and CI-only failure; each entry includes symptom, diagnostic command, likely cause, and corrective action. `roadmap.md` and Chapter 12 must order Device Tree, heap, Sv39, user mode, syscalls, processes, VirtIO block, filesystem, multi-hart, networking, and real hardware, explaining the prerequisite for each.

- [ ] **Step 6: Verify documentation, code, and interactive behavior**

Run: `cargo test -p xtask docs::tests`

Run: `cargo xtask check`

Run: `cargo xtask run`

In the interactive run, execute `help`, `info`, `uptime`, `memory`, `clear`, an unknown command, and `shutdown`. Confirm the output matches the documented examples and that `uptime` increases. If a documented line differs, change the documentation to the verified behavior or fix the implementation when the documented behavior is an acceptance requirement, then rerun `cargo xtask check`.

- [ ] **Step 7: Audit comments and unsafe boundaries**

Run: `rg -n "unsafe|global_asm|asm!|read_volatile|write_volatile" kernel/src`

For every match, confirm an adjacent Japanese comment states the hardware/ABI precondition and safety argument. Run `rg -n "QEMU|0x8020_0000|0x1000_0000|0x8800_0000" kernel/src docs` and confirm each fixed platform assumption is explained in the reference material.

- [ ] **Step 8: Commit the completed material**

```bash
git add README.md docs xtask/src/docs.rs xtask/src/lib.rs
git commit -m "docs: complete the MiniOS learning guide"
```

- [ ] **Step 9: Record final evidence**

Run: `git status --short`

Expected: no output.

Run: `git log --oneline --decorate -8`

Expected: the eight implementation commits appear in task order, ending with `docs: complete the MiniOS learning guide`.

Run: `cargo xtask check`

Expected: format, link validation, Clippy, host tests, cross-build, and all five QEMU tests pass in the final tree.
