# directoryの`rename`（同一dir内のdir改名）

2026-09-17

## ゴール

`rename`をdirectoryへ拡張する。file rename（PR #42）で明示的に非目標とした
dir改名を、同一directory内に限り実現する。

## 設計

### 意味論

- sourceがdirectory、targetが存在しない：entryの8.3名をin-placeで書き換え。
  **同dir内のrenameでは`..`の更新は不要**——`..`は親のcluster番号を指し、
  renameはclusterを動かさない。cross-dir移動は引き続き`CrossDirectory`。
- sourceがdir、targetがfile：`NotDirectory`（POSIXの`rename(dir,file)`は
  `ENOTDIR`）。
- sourceがdir、targetが空dir：置換（POSIX）。targetのentry+LFN runを削除し、
  chainを解放してからsource entryを書き換える。
- sourceがdir、targetが非空dir：`NotEmpty`（POSIXの`ENOTEMPTY`/`EEXIST`系）。
- sourceがfile、targetがdir：`IsDirectory`（既存動作を維持）。
- 同じentryへのrenameはno-op成功（既存動作を維持）。

### 空dir判定

`walk_dir`は`.`/`..`をyieldしないため、entryが0件なら空。`remove_dir`と
同じ規約。

### 置換の順序

`remove_dir`と同じvalidate-before-write：target dirのchainを`chain_tail`で
検証してから書き込みへ入る。空dirにはfile entryが存在しないため、targetの
dir chainを解放してもlive fdを指すentryは消えない——fd失効は不要。

### API名

`rename_file`を`rename`へ改名する（dirも扱うためfile限定の名が不正確）。

## syscall

`Rename = 12`の意味論が拡張されるだけで、番号・引数・errno集合は不変。
dir source/targetが絡む新しいerrnoは`ENOTDIR`と`ENOTEMPTY`——どちらも既存。

## shell

`mv OLD NEW`コマンドを追加し、file/dir両方のrenameを対話経路で往復させる。
`rename`成功時は出力なし、失敗は`print_fat_error`経由。

## guest / harness

`file_rename` guestへdir rename経路を追加：

- `mkdir OLDDIR`→`rename OLDDIR→NEWDIR`成功→旧名`ENOENT`・新dir内fileが
  読める（`..`が正しく引き継がれることの間接証拠）
- dir→fileへのrenameは`ENOTDIR`
- dir→非空dirは`ENOTEMPTY`
- dir→空dirは置換成功（targetが消えsource名が引き継ぐ）
- file→dirは`EISDIR`（既存）
- `rmdir NEWDIR`で消せる

新しいphaseは追加しない——`file-rename`の契約が拡張されるだけ。

## host test

- dir→新名：成功、旧名`NotFound`、新dir内file解決、`..`のcluster不変
- dir→file：`NotDirectory`
- dir→非空dir：`NotEmpty`、target無傷
- dir→空dir：置換、targetのchain解放・entry削除、source改名
- dir→存在しないparent途中file：`NotDirectory`（既存）
- source entry自身へのrename：no-op
