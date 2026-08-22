# 発展roadmap

現在のacceptanceを各段階で維持し、次の順序で一つずつ設計します。各項目の「前提」が前段階の完了gateです。

1. **Device Tree**
   - 前提: OpenSBIの`a1`をboot ABIどおり保持できる。
   - 完了: DTB parserをhost fixtureで検証し、RAM、UART、timebaseをfixed constantなしで取得する。
2. **dynamic heap**
   - 前提: usable RAMとreserved rangeをDevice Treeで確定し、physical page ownershipを守れる。
   - 完了: allocation/deallocation/exhaustionをtestし、kernel collectionの失敗規約を定める。
3. **Sv39 virtual memory**
   - 前提: page-table pageをphysical allocatorとheapから安全に所有できる。
   - 完了: kernel section permission、identity移行、UART MMIO mapping、page fault診断をQEMUで確認する。
4. **user mode**
   - 前提: kernel/user mappingとtrap時のkernel stackがある。
   - 完了: `sret`でU-mode codeを実行し、privileged operationが安全にtrapする。
5. **system calls**
   - 前提: user trap frameを保存してkernelへ戻れる。
   - 完了: syscall number/argument/return ABI、unknown number、bad pointerを検証する。
6. **processes**
   - 前提: syscallでyield/exitでき、address space、kernel stack、trap frameを所有できる。
   - 完了: single-hart schedulerで複数processのcontext switchとcleanupを検証する。
7. **VirtIO block**
   - 前提: heap、physical/virtual変換、interrupt wait、DMA buffer ownershipがある。
   - 完了: read-only sector I/O、descriptor exhaustion、device errorをQEMUで検証する。
8. **filesystem**
   - 前提: block sector readが安定し、buffer lifetimeを管理できる。
   - 完了: small read-only filesystemでlookup/readとcorrupt metadata拒否を検証する。
9. **multi-hart**
   - 前提: schedulerとallocatorのshared state、interrupt-critical sectionが明確である。
   - 完了: per-hart stack、IPI、lock、atomic orderingを追加しrace testを設計する。
10. **networking**
    - 前提: VirtIO queue、timer timeout、buffer ownership、concurrent process実行がある。
    - 完了: VirtIO net、packet parser、ARP/IPの順にmalformed packetとtimeoutを検証する。
11. **real hardware**
    - 前提: Device Tree駆動でQEMU fixed assumptionを除去し、対象boardのfirmware/driver契約を読める。
    - 完了: serial-only bootから始め、実機ごとのtimer、interrupt controller、storageを段階移植する。

学習上の理由と各段階のexerciseは[第12章](../guide/12-next-steps.md)を参照してください。
