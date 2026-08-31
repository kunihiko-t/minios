# 15. U-modeでELFを実行する

## 学習目標

`LoadedImage`が所有するentryとuser stackから、U-modeへ遷移する初期contextを説明できるようになります。
`sscratch`を使うstack交換と、`ecall`からsystem call dispatcherへ至るregister規約を追います。
user pointerをkernelが直接dereferenceせず、page tableを検証してcopyする理由を確認します。
終了後にuser imageとkernel trap stackを回収する順序も確認します。

## 背景

ELF loaderが作る`LoadedImage`は、user pageとpage tableを所有しますが、そのままではCPUが実行するaddress spaceではありません。
実行時には、user pageだけでなく、U-modeからS-modeへ戻るtrap入口、kernel code、UART、kernel trap stackも到達可能である必要があります。

`UserContext`はentryを`sepc`へ、user stack上端を`x2`へ置き、`sstatus.SPP`を0にした初期状態を表します。
この状態で`sret`を実行すると、CPUはU-modeへ入ります。

## 実装

[`UserContext::new`](../../kernel/src/user/context.rs)がentry、stack、`sstatus.SPIE`を初期化します。
[`__run_user`](../../kernel/src/arch/riscv64/user.S)はkernel stack topを`sscratch`へ保存し、user用の`satp`、`sepc`、`sstatus`、整数registerを復元して`sret`します。
ELF loaderは実行pageを通常のstoreで書くため、`__run_user`は`sret`の前に`fence.i`を実行し、同じhartの後続instruction fetchへ書き込みを反映します。

U-modeでtrapが起きると、[`__user_trap_entry`](../../kernel/src/arch/riscv64/user.S)は最初の命令で`sp`と`sscratch`を交換します。
この交換により、user stackを信用して保存領域を確保せず、kernel trap stackへ全register、`sepc`、`sstatus`を保存できます。

system call番号は`a7`です。
`write`は`a0=fd`、`a1=buffer`、`a2=len`を受け、`exit`は`a0=code`を受けます。
[`dispatch_syscall`](../../kernel/src/user/syscall.rs)はこの規約を読み、戻り値またはerrnoを`a0`へ書き戻します。

[`copy_from_user`](../../kernel/src/user/memory.rs)は`start + len`のoverflowを先に拒否します。
その後はpageごとにSv39変換を行い、`U=1`かつreadableなPTEだけから最大4 KiBのkernel bufferへcopyします。
このため、user pointerをRustの参照やraw pointerとして扱わず、未写像page、S-mode専用page、read不可pageを`EFAULT`として返せます。

[`UserRun::finish_exit`](../../kernel/src/user/run.rs)はExit control frameを送ってから`reclaim`を呼びます。
[`UserRun::reclaim`](../../kernel/src/user/run.rs)はkernel trap stackを返し、続いて`LoadedImage::destroy`でuser pageとpage tableを返します。
回収に失敗した値はrun内へ戻るため、呼び出し側は所有権を失わず再試行できます。
回収済みの`UserRun`を再実行すると、解放済みのpage tableとtrap stackを参照するため、二回目の`execute`はarchitecture入口へ進む前に拒否されます。

## 実行と確認

次のQEMU testはU-modeへの遷移だけを確認します。

```console
$ cargo xtask test user-entry
...
[MINIOS_TEST] user-entry: reached
```

次のQEMU testは、S-mode専用pageへのU-mode accessをtrapとして拒否します。

```console
$ cargo xtask test user-trap
...
[MINIOS_TEST] user-trap: rejected
```

`cargo xtask test user-syscall`はstdout、stderr、未知番号、不正pointerのdispatchを確認します。
`cargo xtask test user-exit`はstdout、stderr、Exit frameとframe回収を確認します。

## よくある失敗

- `sstatus.SPP`を残す：`sret`後もS-modeに留まるため、初期contextではSPPを0にします。
- `sscratch`へuser stackを残す：trap入口が信頼できないuser stackへ保存するため、U-modeへ入る直前にkernel stack topを置きます。
- `a7`以外からsystem call番号を読む：RISC-Vの呼び出し規約とずれ、`write`と`exit`を区別できません。
- user pointerを直接読む：PTEの`U`とread権限を確認せずS-modeがaccessするため、page単位のchecked copyを使います。
- Exit frameの前にimageを破棄する：終了codeの報告中に必要な状態を失うため、frame送信後に回収します。

## 演習

[`kernel/src/user/memory.rs`](../../kernel/src/user/memory.rs)のtest fixtureへ、page境界をまたぐread-only user bufferを追加してください。
copyが二つのpageから順にbyteを得ることと、二枚目をS-mode専用へ変えた場合に`Permission`を返すことを確認します。
試験後は`cargo test -p minios-kernel user::memory::tests --locked`を実行してください。

## 次の章

U-mode実行の入力になるMiniBundleは、allocatorの外に予約された物理windowから読みます。
[第16章「boot payloadを実行する」](16-boot-payload.md)では、その範囲検証、read-only mapping、QEMU loaderを追います。
