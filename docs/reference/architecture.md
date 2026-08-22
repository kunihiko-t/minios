# MiniOS architecture

MiniOSはhardware依存部分を小さな境界へ閉じ込め、pure logicをhost testできるlibraryへ分けます。
依存方向はshellやkernel entryからtyped APIへ向かい、上位moduleがCSR、SBI register、UART offsetを
直接操作しない構成です。

## kernel module interface

### binary entryとlink

- `kernel/linker.ld`: `_start`、ELF section、BSS、64 KiB boot stack、`__kernel_end`を定義します。
- `kernel/src/arch/riscv64/entry.S`: OpenSBIの`a0/a1`を保持し、stackとBSSを準備して`kernel_main`へ渡します。
- `kernel/src/main.rs`: hart ID記録、trap、timer、frame allocatorの順序付き初期化、test feature、shell、
  panic/fatal診断を統合します。下位moduleのimplementation detailはここからtyped functionで呼びます。

### `arch/riscv64`

- `mod.rs`: RISC-V専用のassemblyと`csr`、`sbi`、`trap`をまとめるarchitecture boundaryです。
- `csr.rs`: `scause/sepc/stval/time/sstatus/sie`のreadと、S-modeおよびbit不変条件を要求するunsafe writeを
  提供します。callerはraw inline assemblyを他moduleへ複製しません。
- `sbi.rs`: `set_timer(deadline)`、typed reset type/reason、`system_reset`、interrupt-disabled `wfi`を
  提供します。共通の`SbiRet/SbiError`はhost-testable library型を再利用します。
- `trap.S`: 256-byte frameへinteger registerを保存し、Rust handlerをcall、復元後`sret`します。
- `trap.rs`: `scause`を`Interrupt/Exception`へdecodeし、Direct `stvec`初期化、timer dispatch、breakpoint
  acceptance、unexpected trap診断を担当します。

### deviceとconsole

- `drivers/uart.rs`: QEMU `virt`の16550 compatible UARTについて、`write_byte`、`read_byte`、`has_byte`と
  `fmt::Write`だけを公開します。文字列policyやcommandは持ちません。
- `console.rs`: format出力macroのbackend、blocking byte input、one-byte output、lockを使わないemergency
  outputを提供し、device register配置を上位から隠します。

### time

- `time.rs`: 10 MHz/100 Hz constants、`ticks_to_millis`、`ticks`、`uptime_millis`をhostとguestへ公開します。
  RISC-V側では初回SBI deadline、STIE/SIE、interruptごとのtick incrementと再予約を所有します。

### memory

- `memory/frame.rs`: `PhysFrame`、`FrameError`、`FrameStats`、const-generic `FrameAllocator`を提供します。
  4 KiB alignment、capacity、range、double freeを検証し、bitmapによるunique ownershipを守ります。
- `memory/mod.rs`: physical frame管理のnamespaceです。virtual addressやheapの責務はまだありません。

### shell

- `shell/line.rs`: fixed-capacity printable ASCII buffer、Backspace、persistent overflow state、clearをpure logic
  として提供します。
- `shell/command.rs`: trimmed inputをcommand enumへ分類するだけで、UARTやglobal stateへ作用しません。
- `shell/mod.rs`: UART polling/echo、prompt、command dispatchを所有し、timerのread APIと一意なallocator
  referenceを利用します。shutdownだけはtyped SBI reset境界へ渡します。

### host-testable library

- `kernel/src/lib.rs`: host test可能な`arch` decode、memory、SBI return conversion、shell pure logic、time conversionを
  公開します。RISC-V runtime専用implementationは`cfg(target_arch = "riscv64")`で隔離します。
- `kernel/src/sbi.rs`: SBI error/valueのpure conversion contractを定義します。

## xtask module interface

- `xtask/src/main.rs`: process arguments、human-readable error、終了statusだけを担当します。
- `cli.rs`: `setup/build/run/test/check`とtest filterのparse/help contractです。
- `tools.rs`: rustc、rustup target、QEMUの検出、version parse、platform別修正commandを担当します。
- `cargo.rs`: Cargo subprocess、cross build、ELF path、command/status/stdout/stderr診断を担当します。
- `qemu.rs`: QEMU `virt`引数、interactive/marker mode、deadline、concurrent pipe drain、kill/wait、transcript
  validationを所有します。
- `docs.rs`: repository内Markdownのrelative inline linkとChapter 1–12の7必須sectionを検査します。
- `lib.rs`: public commandを14-phase planへ変換し、first failureで停止してelapsed summaryを出します。

## 8-step boot flow

1. **QEMU → OpenSBI**: `-machine virt -m 128M -smp 1 -bios default`でfirmwareを起動します。
2. **OpenSBI → `_start`**: kernel ELFを`0x8020_0000`へ配置し、hart IDを`a0`、DTB addressを`a1`へ
   入れてS-modeへ制御を渡します。
3. **assembly前処理**: `_start`が`SIE`を止め、`__boot_stack_end`を`sp`へ設定します。
4. **Rust不変条件**: `__bss_start..__bss_end`をzero化し、`kernel_main(hart_id, dtb)`をC ABIでcallします。
5. **runtime初期化**: `kernel_main`がhart IDを記録し、trap vector、初回timer、physical frame allocatorの順に
   初期化します。
6. **観測可能化**: `[ok] traps`、`[ok] timer`、`[ok] memory`とboot bannerをUARTへ出し、test featureなら
   markerとSBI resetへ進みます。
7. **shell loop**: 通常buildは`minios> `を表示し、UART inputをbounded lineへechoしてcommandを処理します。
8. **非同期timer**: shell実行中もsupervisor timer trapが入り、registerを保存、tick更新、次deadlineを予約、
   registerを復元して`sret`で中断位置へ戻ります。

addressと占有範囲は[memory map](memory-map.md)、略語は[用語集](glossary.md)を参照してください。
