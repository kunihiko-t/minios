# `mkdir`/`rmdir`（directory作成と削除）

2026-09-17

## 背景

file lifecycleは`create`〜`rename`まで揃った。directoryは既存のもの
（`DOCS`）を読めるだけで、guestから新しいdirを作る・消す手段がない。
`create_file`/`unlink_file`の機構を再利用しつつ、`.`/`..` entryの
生成と「空dirのみ削除可」という新しい責任を教える。

## 目標

- `Fat32::create_dir(path)`：新dirへ1 clusterを割り当て、`.`と`..`を
  初期化してから親dirへentryを追加する。
- `Fat32::remove_dir(path)`：`.`/`..`のみの空dirを`unlink`と同じ手順で
  削除する。
- syscall `Mkdir = 13`、`Rmdir = 14`：`a0=path_ptr, a1=path_len`。
- 新errno `EEXIST = -17`（同名entryが既存）、`ENOTEMPTY = -39`
  （空でないdirへのrmdir）。
- shell `mkdir`/`rmdir`コマンド + guest `file_mkdir` + QEMU phase。

## 設計

### FAT32層（`kernel/src/storage/fat32.rs`）

`FatError`に`Exists`と`NotEmpty`を追加。

`create_dir(path) -> Result<(), FatError>`:

1. 全要素を`is_valid_component`で検証し、最終要素を`to_short_name`で
   8.3正規化（`.`/`..`やLFN名は`InvalidName`——`create`と同じ規約）。
2. 親dirを`resolve_path`で解決。fileなら`NotDirectory`。
3. 正規化名で`walk_dir`照合：同名entryがあればfile/dirを問わず
   `Exists`（POSIXの`EEXIST`）。
4. **新clusterを先に割り当てる**（`alloc_cluster`はzero-fill済み）。
   先に親entryを書くと、失敗時にcluster 0を指すdangling entryが残る。
5. 新clusterの先頭2 recordへ`.`（self）と`..`（親）を書く。`..`の
   `first_cluster`はFAT32規約で、親がrootなら0、そうでなければ親の
   first_cluster。
6. 親dirへ`find_free_dir_slot` + entry書き込み（attr `0x10`、size 0、
   `first_cluster`は新cluster）。**失敗時は割当てたclusterを
   `free_chain`で解放してから返す**——leakさせない。
7. entry書き込みの終端処理は`create_file`と同じ規約。重複する
   終端移動ロジックは`place_dir_entry`helperへ切り出して共有する。

`remove_dir(path) -> Result<(u32, u32), FatError>`:

1. `resolve_path`。fileなら`NotDirectory`（POSIXの`rmdir(file)`は
   `ENOTDIR`）。rootはpathで名指せず、`.`/`..`は`is_valid_component`
   が拒否するため到達不能。
2. `walk_dir`で`.`/`..`以外のentryを探し、あれば`NotEmpty`。
   LFN recordはentryに随伴するため、record単位でなくentry単位で数える。
3. 空なら`unlink_file`と同じ手順：LFN run削除→entry`0xe5`→chain解放。
   削除したentryの物理位置を返す（fd失効との対称性——dirはfdを持て
   ないので現状no-opだが、戻り値の形は揃える）。

### syscall層

- `ControlSource::make_dir`/`remove_dir`、default `ENOSYS`。
- `dispatch_unlink`を`dispatch_path_op`へ一般化し、`Unlink`/`Mkdir`/
  `Rmdir`で共有する（3つとも`a0`/`a1`の単一path規約が同じ）。

### bin層

- `UartControlSource::{make_dir, remove_dir}`を`FILE_STORAGE` sessionへ
  委譲。`fat_errno`に`Exists => EEXIST`、`NotEmpty => ENOTEMPTY`を追加。

### shell

- `mkdir PATH`/`rmdir PATH`を追加し、対話経路でdir lifecycleを往復
  させる（`ls`で新dirが現れ、rmdir後に消える）。

### guest + xtask

- `guest/src/bin/file_mkdir.rs`：`NEWDIR`作成→再作成`EEXIST`→
  既存file名`EEXIST`→`NEWDIR/F.TXT`へcreate/write/close→再openで
  内容照合→非空`rmdir`で`ENOTEMPTY`→unlink→`rmdir`成功→
  fileへの`rmdir`で`ENOTDIR`→file下の`mkdir`で`ENOTDIR`→
  非8.3名で`EINVAL`→stdoutへ`mkdir verified`→exit(42)。
- `TestKind::FileMkdir` + `file-mkdir` phase + frame列verifier。

## host test

- `create_dir`：新dirが`.`/`..`を持つ（`.`は自己、`..`は親）、
  `resolve_path`で潜れる、その中へ`create_file`できる
- root直下のdirの`..`が0を指す規約
- 同名file/dirへの`Exists`、file親への`NotDirectory`、非8.3名拒否
- `find_free_dir_slot`失敗（dir満杯でNoSpace）時にclusterを残さない
- `remove_dir`：空dir削除でentry`0xe5`+chain解放+loc返却、
  非空`NotEmpty`、file`NotDirectory`、不在`NotFound`、
  LFN付きdir名でもrunごと削除
- dispatch：Mkdir/Rmdirがsourceへ届く、検証規約、default `ENOSYS`

## 非目標

- dirのrename（`..`更新が必要——rename specの非目標のまま）
- `.`/`..`をpath要素として受理（`is_valid_component`が拒否する現状維持）
- LFN名のdir（`create`と同じ8.3規約）

## 検証

- `cargo fmt --all -- --check`
- `cargo test -p minios-kernel --lib`、abi、xtask
- `cargo clippy`（lib・RV64 bin・RV32 bin・guest、`-D warnings`）
- RV64/RV32 build、guest build
- `cargo xtask check` 全42フェーズ
