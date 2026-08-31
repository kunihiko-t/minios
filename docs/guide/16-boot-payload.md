# 16. boot payloadを実行する

## 学習目標

MiniBundleを置く物理予約windowと、allocatorがそのwindowを所有しない理由を説明できるようになります。
header長を先に検証してからmanifestとELFをparseするtwo-stage parseを追います。
payloadの使用pageだけをS-mode read-onlyでmapし、QEMU loaderからproduction ELFを渡す流れを確認します。

## 背景

QEMU `virt`の128 MiB RAMでは、`0x8780_0000..0x8800_0000`をboot payload専用windowとして予約します。
このwindowをallocatorのmanaged rangeから外すため、user imageのpageやpage tableがpayload byteを上書きしません。

payloadはカーネルが所有するコピーではなく、予約windowを借用するbyte sliceです。
したがって実行中の所有frameは回収できますが、loaderが配置したpayload自体をallocatorへ返すことはありません。

## 実装

[`BootPayload::from_reserved_window`](../../kernel/src/boot_payload.rs)は最初に固定長headerだけを読みます。
headerの`total_len`が予約windowを超えないことを確認してから、その長さのbyte sliceを作ります。

[`BootPayload::parse`](../../kernel/src/boot_payload.rs)は二段目でheader、manifest range、ELF range、paddingを検証します。
`Manifest::parse`が成功し、ELF rangeが検証済みtotal rangeに収まった場合だけ、loaderへ渡すELF sliceを返します。

[`KernelMapPlan::with_payload_pages`](../../kernel/src/vm/kernel.rs)はpayload先頭から使用lengthを4 KiBへ切り上げます。
そのpageだけを`U=0`かつread-onlyでidentity mapし、予約window全体やwritable mappingを作りません。

[`kernel_main`](../../kernel/src/main.rs)はbare modeでpayload headerを確認し、kernel address spaceの完成後にpayload ELFを`LoadedImage`へ配置します。
[`run_boot_payload`](../../kernel/src/main.rs)はReady frameを送ってU-mode実行を開始し、`UserRun::finish_exit`でExit frameと実行用frameの回収を完了します。

[`qemu_command_with_payload`](../../xtask/src/qemu.rs)は一時MiniBundleをQEMUの`-device loader`へ渡します。
loader argumentは`addr=0x87800000,force-raw=on`を指定し、kernelが検証する予約windowの先頭へraw byteを置きます。

## 実行と確認

次のコマンドは決定的なMiniBundleを一時fileへ作り、QEMU loaderでnormal kernelを起動します。

```console
$ cargo xtask test payload
...
MiniOS payload: ok code=42
```

host harnessはReady、stdout、stderr、Exit、cleanup diagnosticの順序を検証します。
timeout時はQEMU childをkillしてwaitし、出力readerをjoinしてからerrorを返します。

## よくある失敗

- headerを読まずに予約window全体をparseする：攻撃的な`total_len`に対するrange境界がなくなるため、固定長headerを先に検証します。
- payload windowをallocatorへ渡す：user pageやpage tableとpayload byteが同じ物理pageを所有するため、managed rangeの上端をwindow先頭に置きます。
- payload全体をwritableでmapする：ELF inputを実行中に変更できるため、使用pageだけをS-mode read-onlyでmapします。
- QEMU loaderのaddressを変える：kernelが検証するwindowと一致しないため、`0x87800000`を使います。
- payloadを`LoadedImage`と同じ所有物として破棄する：payloadは借用rangeであり、回収するのは実行用に確保したframeです。

## 演習

[`kernel/src/boot_payload.rs`](../../kernel/src/boot_payload.rs)のcanonical bundleを基準に、headerの`total_len`を予約windowより1 byte大きくしてください。
`BootPayload::parse`が`HeaderTooLarge`を返すことを確認します。
試験後は`cargo test -p minios-kernel boot_payload::tests --locked`と`cargo xtask test payload`を実行してください。

## 次の章

このガイドの実装順はここで終わります。
全章の索引は[学習ガイド](README.md)へ戻ります。
