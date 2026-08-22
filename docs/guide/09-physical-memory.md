# 9. 物理memoryとpage管理

## 学習目標

4 KiB物理page、`__kernel_end`、bitmap allocator、first-fit、解放error、統計の不変条件を説明できる
ようになります。

## 背景

Sv39を有効にする前でも、将来page tableやheapへ渡す物理RAMの所有権を重複なく管理する必要が
あります。QEMU `virt`のRAM全体`0x8000_0000..0x8800_0000`にはOpenSBIとkernel imageが含まれる
ため、そのままfreeにしてはいけません。

## 実装

[`FrameAllocator`](../../kernel/src/memory/frame.rs)は
`align_up(__kernel_end, 4096)..0x8800_0000`だけを管理します。128 MiBは最大32,768 pagesなので、
512個の`u64`、合計4 KiBのbitmapで表せます。bitmap自体はkernel image内です。

`allocate`は先頭の0 bitを1にするfirst-fit、`deallocate`は範囲外を`OutOfRange`、未使用bitを
`DoubleFree`として拒否します。`FrameAllocator::new`は`unsafe`なclaim境界です。alignment、空範囲、
bitmap容量は実装が検査できますが、その物理範囲をOpenSBI、別allocator、device用subsystemなどが既に
所有していないかはaddress値だけでは判定できません。callerは成功時の全範囲が未所有かつ排他的で、
allocatorの生存中に別ownerがclaimしないことを保証します。

kernel entryでのclaimは、OpenSBI領域`0x8000_0000..0x8020_0000`と、linkerが示すkernel image末尾までを
除外した`align_up(__kernel_end, 4096)..0x8800_0000`です。この範囲を管理するsubsystemは局所
`FrameAllocator<512>`だけで、global allocatorや別のmemory managerは初期化しません。

払い出す`PhysFrame`は`Clone`/`Copy`ではなく、`start(&self)`はtokenを消費せずaddressだけを読みます。
`deallocate`はtokenを消費するため、safe codeは同じtokenで再解放できません。低レベルsubsystemとの
ownership移管用`PhysFrame::from_start`も`unsafe`で、callerにpageの排他所有と同addressの生存tokenが
ないことを要求します。これによりsafe codeはtokenの複製・forge・重複range作成を行えません。

bitmapは`stats`について常に`total = allocated + free`を保ち、拒否操作では値を変えません。一方、
`unsafe` callerが契約を破って作ったaliasや、firmware/DMAなどallocator外の所有状態は検出も保証も
できません。`OutOfRange`、`Unaligned`、`DoubleFree`は、そのような境界誤用を調べる診断であり、
unsafe preconditionの代わりではありません。

## 実行と確認

```console
$ cargo xtask test memory
...
[MINIOS_TEST] memory: ok
```

guest testは異なる整列pageを2枚確保し、1枚を解放、同じpageが再利用されることを検査します。
hostのfocused testsは確保前、確保後、拒否操作後、解放後の`FrameStats`も具体値で確認します。

## よくある失敗

- linker overlap/firmware破壊: startを固定値にせず`__kernel_end`から上へalignします。
- 同じaddressが二度返る: bitmap bit更新と、`unsafe` claim時に範囲を重複所有していないかを調べます。
- `free`が減らない: `allocated`更新と`total = allocated + free`を状態遷移ごとに確認します。
- 128 MiB上端を超える: QEMUの`-m 128M`と`PHYSICAL_MEMORY_END`を同時に更新しない限り変更しません。

## 演習

128 MiBを4 KiBで割って32,768 pages、さらに8で割って4,096 bytesのbitmapになることを計算して
ください。次に3 pagesだけのallocatorでallocate/deallocate列を書き、bit patternとstatsを追います。

## 次の章

[第8章](08-timer-interrupts.md)へ戻れます。次は[第10章: shell](10-shell.md)で、timerとallocatorの
read APIをUART commandへ接続します。
