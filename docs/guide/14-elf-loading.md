# 14. ELFを実行前アドレス空間へ配置する

## 学習目標

ELF64 headerと`PT_LOAD` program headerから、ファイル内の配置と仮想メモリー内の配置を区別できるようになります。
segmentを4 KiBページへ丸め、file byte、partial pageのゼロ領域、BSS、64 KiBのuser stackを構築する順序を追います。
検証または配置に失敗したとき、ページテーブルを含む所有フレームをrollbackする理由も確認します。

## 背景

ELFのfile layoutは、実行ファイル内のbyteがどのoffsetに何byteあるかを示します。
一方、memory layoutは、各`PT_LOAD`をどの仮想アドレスへ何byte置き、どのページ権限を与えるかを示します。

`p_filesz`より`p_memsz`が大きい部分はファイルにbyteを持たず、ロード時にゼロで埋めるBSSです。
segmentの先頭と末尾がページ境界に一致しない場合は、周囲を含むページを確保してsegmentのfile byteだけを対応するoffsetへコピーします。
検証前にページを割り当てると、後のheaderで不正を見つけた際に部分的なアドレス空間が残るため、MiniOSは計画確定と配置を分けます。

## 実装

`ElfImage::parse`は入力byte sliceを借用し、ELF64、little-endian、RISC-V、`ET_EXEC`、program header表の範囲を検査します。
MiniOSは動的loaderを持たないため、`PT_INTERP`または`PT_DYNAMIC`を含む実行ファイルを静的ELFとして受理しません。
**`LoadPlan`**は最大8個の`PT_LOAD`を走査し、`p_filesz <= p_memsz`、alignment、`p_vaddr`と`p_offset`の合同条件、checked range、page単位の重複、user範囲、stackとの衝突、`W+X`、entry pointを検査します。
空でないload segmentのpage合計は2,048ページまでで、すべてのheaderを検証し終えるまで物理フレームを確保しません。

`load_image`は検証済み計画をmaterializeし、各user pageをゼロ化してからfile byteをコピーします。
segmentのleafはELF flagsから導いた最小権限と`U=1`を持ち、user stackは`0x3fff_0000..0x4000_0000`へ`R+W+U`で16ページを写像します。
直下の`0x3ffe_f000..0x3fff_0000`はguard pageとして未写像のまま残します。

成功時に返す**`LoadedImage`**は、inactiveな`AddressSpace`、検証済みentry、user stack上端を保持します。
inactiveとは、root page tableが`satp`へ設定されず、CPUがそのユーザー空間で命令を実行していない状態です。
`LoadedImage::destroy`は所有フレームをアロケーターへ返し、誤ったアロケーターが拒否した場合は完全なimageを返して再試行できる所有権を保ちます。
構築途中では`AddressSpaceBuilder`のrollbackが、root、中間table、user page、stack pageを回収します。

## 実行と確認

ELFの配置と回収をQEMU上で確認するには、リポジトリのルートで次のコマンドを実行します。

```console
$ cargo xtask test elf
...
[MINIOS_TEST] elf: ok
```

`elf`経路は再利用する物理フレームを先に非ゼロ値で汚し、決定的なfixtureを新しいinactiveなアドレス空間へロードします。
その後、entry、textとdataのbyte、partial page、BSS、stack、guard page、kernelとuserの`U` bitをS-modeから検査します。
最後に`LoadedImage`を破棄し、アロケーター統計と固定容量storageが開始時へ戻った場合だけ`[MINIOS_TEST] elf: ok`を出します。
このマーカーはELFの配置と回収を示しますが、U-mode遷移、`write`、`exit`の実行は示しません。

## よくある失敗

- `p_filesz > p_memsz`を受け入れる：ファイルから読むbyteがmemory rangeを超えるため、配置前に`FilesLargerThanMemory`として拒否します。
- 合同条件をoffsetの整列と取り違える：`p_align > 1`では`p_vaddr % p_align == p_offset % p_align`を検査し、両方が単独で0になることは要求しません。
- byte範囲だけで重複を調べる：異なるsegmentが同じ4 KiBページを共有すると一つのPTEへ矛盾する権限を要求できるため、pageへ丸めた範囲で拒否します。
- `W+X`を許す：ユーザーpageが書き換え可能な命令領域になるため、ELF flagsとPTE生成の両方で拒否します。
- entryが任意のload segment内にあればよいとする：開始命令をfetchできるよう、実行可能な`PT_LOAD`のmemory range内だけを受け入れます。
- 途中失敗でframeを失う：header検証をallocationより先に完了し、materialize中の失敗はbuilderが所有記録からrollbackします。

## 演習

`kernel::elf::fixture`が作る正常なELFを基準にし、program headerのfieldを一つずつ変更するホスト試験を追加してください。
`p_filesz`、`p_memsz`、`p_align`、`p_vaddr`、`p_offset`、flags、entryを個別に壊し、対応する`ElfError`だけが返ることを確認します。
allocation後の失敗を注入する試験では、実行前後の`FrameStats`と`AddressSpaceStorage::len()`を比較し、回収漏れを観測できるようにします。
試験後は`cargo test -p minios-kernel elf:: --locked`と`cargo xtask test elf`を実行してください。

## 次の章

次の章では、inactiveな`LoadedImage`をkernel mappingとtrap stackへ結び、`sret`でU-modeへ入ります。
最初のsystem callはUARTへ出力する`write`と終了状態を返す`exit`に絞り、未知の番号と不正なuser pointerも検査します。
[第15章「U-modeでELFを実行する」](15-user-mode.md)へ進みます。
