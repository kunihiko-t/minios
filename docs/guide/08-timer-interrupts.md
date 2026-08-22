# 8. supervisor timer割り込み

## 学習目標

QEMU `virt`の10 MHz timebaseを100 Hz tickへ変換する方法、SBI TIMEの絶対deadline、STIE/SIEの
許可順、atomic tickの読み方とtestが保証する範囲を説明できるようになります。

## 背景

S-modeはread-onlyの`time` CSRから現在時刻を読めますが、M-modeのtimer registerへ直接書きません。
OpenSBIへ次の発火時刻を依頼し、supervisor timer interrupt code 5をtrap handlerで受けます。
10,000,000 Hz / 100 Hzなので1 tickは100,000 cycle、10 msです。

## 実装

[`time`](../../kernel/src/time.rs)は初期化時とhandlerごとに`time`を読み、`now + 100_000`をSBI TIME
extension `0x54494D45` function 0へ渡します。過去のdeadlineへ追いつく方式ではなく、処理時点から
次の10 msを予約します。先にtrap entry、次に初回deadline、`sie.STIE` bit 5、最後に
`sstatus.SIE` bit 1を有効にし、準備前の割り込みを避けます。

handlerは`AtomicU64`を`Relaxed`でincrementし、次のdeadlineを再予約します。現在はsingle hartで
tickは他stateの公開flagではないため、このorderingで十分です。SBI errorには回復規約がないので、
初回予約も再予約も数値errorをemergency UARTへ出してfailure resetします。

## 実行と確認

```console
$ cargo xtask test timer
...
[MINIOS_TEST] timer: ok
```

kernelは3 tick以上を観測してmarkerを出します。これはtimer割り込みがhandlerへ複数回入りtickが
進むことを確認しますが、3回という短い観測だけで「毎回、新しい将来deadlineへ正しく再予約した」
ことを独立に証明するものではありません。pending状態の再trapでもcounterだけは進み得るためです。
再予約の根拠はhandlerの`read_time + CYCLES_PER_TICK`実装、SBI error経路、手動受入で時間を空けた
2回の`uptime`増加を合わせて判断します。

## よくある失敗

- timer test timeout: `time` CSR、SBI extension ID、STIE/SIE、cause 5 dispatchの順に調べます。
- 即座にtrapを繰り返す: absolute deadlineではなく相対値を渡していないか確認します。
- tickは増えるが時間換算が違う: `TIMEBASE_HZ`と`TICKS_PER_SECOND`から100,000 cycleを再計算します。
- 3 tick markerだけでrearmを断言する: testの観測範囲とimplementation inspectionを区別します。

## 演習

`ticks_to_millis(250)`を手計算し、host testの期待値2,500 msと照合してください。次にQEMU shellで
`uptime`を2回、1秒以上空けて実行し、差が概ね1,000 ms以上になることを確認します。

## 次の章

[第7章](07-traps-and-interrupts.md)へ戻れます。次は
[第9章: 物理memory](09-physical-memory.md)で、kernel imageより後ろのRAMをpage単位に管理します。
