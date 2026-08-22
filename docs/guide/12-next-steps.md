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

1. **Device Tree解析** — 前提: OpenSBIの`a1`でDTB addressを保持できていること。固定addressを
   node/propertyから読むpure parserとfixture testを先に作ります。
2. **dynamic heap** — 前提: Device Treeでusable RAMを確認し、物理page allocatorがunique ownershipと
   statsを守ること。最初はsmall allocatorとallocation failureを明示します。
3. **Sv39 virtual memory** — 前提: page-table nodeをheapで確保できること。identity mappingから始め、
   UART MMIOとkernel section permissionをtestします。
4. **user mode** — 前提: kernel/user address spaceをSv39で分離できること。`sret`でU-modeへ入り、
   privileged instructionがtrapすることを確認します。
5. **system calls** — 前提: user trap frameとkernel stackがあること。register ABIを固定し、未知syscallの
   errorもtestします。
6. **processとscheduler** — 前提: syscallでyield/exitでき、address spaceとkernel stackを所有できること。
   まずsingle-hart cooperative schedulingから始めます。
7. **VirtIO block** — 前提: heap、physical/virtual address変換、interrupt待機があること。descriptor ringと
   read-only sector testを作ります。
8. **filesystem** — 前提: VirtIO blockのsector readが安定していること。小さなread-only filesystemから
   始め、壊れたmetadataを拒否します。
9. **multi-hart** — 前提: schedulerとallocatorのshared state境界が明確なこと。per-hart stack、IPI、lock、
   atomic orderingを追加します。
10. **networking** — 前提: VirtIO、interrupt、buffer ownership、timer timeout、concurrent schedulingが
    あること。VirtIO netからpacket parser、ARP/IPへ進みます。
11. **real hardware** — 前提: Device Tree駆動でQEMU固定値を除去し、必要driverのdatasheetとboot firmware
    契約を確認できること。serial consoleだけのbootから移植します。

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

[第11章](11-test-harness.md)へ戻れます。これで基礎教材は完了です。[ガイド索引](README.md)から
必要な章を再読し、[architecture](../reference/architecture.md)、[memory map](../reference/memory-map.md)、
[用語集](../reference/glossary.md)、[troubleshooting](../reference/troubleshooting.md)を実装中の参照資料として
使ってください。
