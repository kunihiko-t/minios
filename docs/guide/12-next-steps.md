# 12. 次に作るもの

## 学習目標

現在のsingle-hart、physical-address kernelから機能を広げるとき、依存関係を壊さない11段階の順序を
説明し、各段階の最小testを考えられるようになります。

## 背景

OS機能は独立に見えても、hardware発見、memory allocation、address-space分離、privilege境界、
永続I/Oの順に土台を必要とします。たとえばfilesystemを先に書いてもblock deviceもheapもなければ、
失敗がどの層にあるか分かりません。現在の`cargo xtask check`を安全網として維持し、一段ずつpublic
contractとacceptanceを追加します。

## 実装

推奨順序と前提は次のとおりです。

1. **Device Tree解析** — 前提: OpenSBIの`a1`でDTB addressを保持できていること。
   [`entry.S`のboot ABI](../../kernel/src/arch/riscv64/entry.S)から
   [`kernel_main`のDTB境界](../../kernel/src/main.rs)へ渡る値を起点に、固定addressを
   node/propertyから読むpure parserとfixture testを先に作ります。
2. **dynamic heap** — 前提: Device Treeでusable RAMを確認し、物理page allocatorがunique ownershipと
   statsを守ること。[現在のframe ownership](../../kernel/src/memory/frame.rs)の上に最初はsmall
   allocatorを載せ、allocation failureを明示します。
3. **Sv39 virtual memory** — 前提: page-table nodeをheapで確保できること。identity mappingから始め、
   [現在のlinker section境界](../../kernel/linker.ld)をpermission設計の入力にして、UART MMIOと
   kernel section permissionをtestします。
4. **user mode** — 前提: kernel/user address spaceをSv39で分離できること。
   [現在のS-mode trap frame](../../kernel/src/arch/riscv64/trap.S)を拡張して`sret`でU-modeへ入り、
   privileged instructionがtrapすることを確認します。
5. **system calls** — 前提: user trap frameとkernel stackがあること。register ABIを固定し、未知syscallの
   errorもtestします。[現在のcause分岐](../../kernel/src/arch/riscv64/trap.rs)がsyscall dispatchを
   追加する境界です。
6. **processとscheduler** — 前提: syscallでyield/exitでき、address spaceとkernel stackを所有できること。
   [現在のsingle-hart shell loop](../../kernel/src/shell/mod.rs)の一つの実行主体を置き換える形で、
   cooperative schedulingから始めます。
7. **VirtIO block** — 前提: heap、physical/virtual address変換、interrupt待機があること。descriptor ringと
   read-only sector testを作ります。[UART driverのMMIO境界](../../kernel/src/drivers/uart.rs)は、
   volatile accessとdevice固有状態をdriver内へ閉じ込める最小例です。
8. **filesystem** — 前提: VirtIO blockのsector readが安定していること。小さなread-only filesystemから
   始め、壊れたmetadataを拒否します。[固定長line buffer](../../kernel/src/shell/line.rs)のように、
   入力容量と拒否状態を型の境界で明示します。
9. **multi-hart** — 前提: schedulerとallocatorのshared state境界が明確なこと。per-hart stack、IPI、lock、
   atomic orderingを追加します。[現在のtick atomic](../../kernel/src/time.rs)のordering理由を読み、
   複数writerへ変わる状態だけを改めて設計します。
10. **networking** — 前提: VirtIO、interrupt、buffer ownership、timer timeout、concurrent schedulingが
    あること。[command parserのpure logic分離](../../kernel/src/shell/command.rs)を手本に、VirtIO netから
    packet parser、ARP/IPへ進み、device I/Oなしでmalformed packetをhost testします。
11. **real hardware** — 前提: Device Tree駆動でQEMU固定値を除去し、必要driverのdatasheetとboot firmware
    契約を確認できること。[RISC-V arch公開境界](../../kernel/src/arch/riscv64/mod.rs)と
    [QEMU UART constructor](../../kernel/src/drivers/uart.rs)をboard依存実装から分離し、serial console
    だけのbootから移植します。

詳細なmilestoneと完了条件は[発展roadmap](../reference/roadmap.md)にも整理しています。

## 実行と確認

各段階を始める前後で次を実行します。

```sh
cargo xtask check
```

新機能のfocused REDを先に観測し、GREEN後に14 phaseへ戻ります。新しいhardware pathを追加したら、
host pure-logic testだけでなく専用QEMU markerまたはinteractive transcriptをacceptanceへ加えます。

## よくある失敗

- 複数段階を一度に入れる: failure sourceが曖昧になります。前段のpublic contractをcommitしてから進みます。
- fixed addressを新driverへ複製する: Device Tree段階の成果を使い、QEMU固有fallbackを一箇所にします。
- heap導入後に無制限allocationする: allocation failureはkernelでも通常の境界条件です。
- multi-hartを早期に入れる: single-hartで隠れていたownershipとinterrupt ordering問題が全層へ広がります。

## 演習

最も興味のある項目を一つ選び、そのdirect prerequisite、host test可能なpure logic、QEMUでしか確認
できない境界、失敗時diagnosticを四列の表にしてください。prerequisiteが未完なら、一つ前の項目へ
戻って同じ表を作ります。

## 次の章

[第11章: テストハーネス](11-test-harness.md)へ戻ると、発展中も維持するacceptanceの入口を
確認できます。基礎教材の次は[発展roadmap](../reference/roadmap.md)を最終navigationとして開き、
選んだmilestoneの前提と完了条件を書き出してください。[ガイド索引](README.md)から必要な章を再読し、
[architecture](../reference/architecture.md)、[memory map](../reference/memory-map.md)、
[用語集](../reference/glossary.md)、[troubleshooting](../reference/troubleshooting.md)も実装中の参照資料として
使えます。
