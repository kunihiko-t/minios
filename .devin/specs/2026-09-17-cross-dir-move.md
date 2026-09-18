# cross-directory move（`rename`の完全POSIX化）

2026-09-17

## ゴール

`rename`を別directoryへの移動へ拡張し、`EXDEV`拒否を解消する。
file/dirのentryをsource dirからtarget dirへ移し、移動したdirの`..`を
新しい親へ更新する。開いているfile fdはentryの物理位置の更新によって
追従させ、POSIXの「open fileはrenameしても有効」をmoveでも維持する。

## 設計

### 移動の手順（validate-before-write）

1. source解決・target parent解決（dir必須）。同dirなら従来のin-place
   renameへ流す。
2. cycle検査：sourceがdirの場合、新parentの`..`chainをrootまで辿り、
   sourceのclusterが現れたら`MoveIntoItself`（→`EINVAL`）で拒否。
   `mv A A/...`と`mv A B/A`（BはAの子孫）を両方防ぐ。
3. target dir内の同名衝突を、従来のmatrixで処理（file→dir`IsDirectory`、
   dir→file`NotDirectory`、dir→非空dir`NotEmpty`、file→file/dir→空dirは置換）。
4. target dirのfree slotを特定（`find_free_dir_slot`——chain拡張が起きても
   空の終端済みclusterが残るだけなので後続失敗を許容できる）。
5. source LFN runの範囲を確定（検証のみ）。
6. 書き込み順：source runを`0xe5`化→置換targetの削除+chain解放→target
   slotへrecordを書く（name=新8.3名、attr/first_cluster/sizeはsource
   recordから複写）→sourceがdirなら`..`のcluster fieldを新parentへ
   （root親なら0）更新。

### `..`更新

dirのfirst clusterのsector 0、record index 1が`..         `であることを
確認してからfirst_cluster fieldを書き換える。見つからなければ
`CorruptChain`。

### fd追従（relocate）

`FileDesc`の`dir_cluster`/`dir_index`はwrite-back先の物理位置。moveで
entryが動くと古い位置は解放済みslotになるため、fdを指す全processの
descriptorを新しいlocへ書き換える（`relocate_file_fds`）。dir内のfileは
中身のentryが動かないため影響なし。置換で消えたtargetは従来通り
`revoke_file_fds`。

戻り値を`(Option<(旧loc,新loc)>, Option<置換loc>)`へ変更し、control層が
relocate→revokeの順に適用する。

### errnoの整理

- 新`FatError::MoveIntoItself`→`EINVAL`（POSIXの"cannot move into itself"）。
- `CrossDirectory`/`EXDEV`は到達不能になるため、variant・定数・errno表・
  guest期待をすべて撤去する。

## shell

`mv OLD NEW`は`session.rename`へ委譲済みのため変更不要。対話QEMU scriptに
`mv HELLO.TXT DOCS/MOVED.TXT`→`cat`→移動し戻す往復を追加する。

## guest

`file_rename`の`EXDEV`checkを実move検証へ置き換える：

- `rename(VICTIM.TXT→DOCS/MOVED.TXT)`→0→旧名`ENOENT`→新名でread照合
- 戻す`rename(DOCS/MOVED.TXT→RESTORED.TXT)`→read照合
- **fd追従**：moveしたfileを開いたままmoveし、fdでreadが継続すること
- dirのmove：`mkdir SRC`→file入り→`rename(SRC→DOCS/SRCD)`→中身を新pathで
  read→`rename(DOCS/SRCD→SRC2)`でrootへ戻す→`rmdir`
- cycle：`rename(DOCS→DOCS/X)`→`EINVAL`
- 既存の同名fileへのmove置換→target fd`EBADF`

## host test

- file move往復：新dir内で解決、旧dirから消える、fd用loc更新の返却
- dir move+`..`byte検査（root親は0、非root親はparent cluster）
- `..`がrecord 1に無いdirは`CorruptChain`
- cycle（自身・子孫）→`MoveIntoItself`
- move置換：file→file、dir→空dir、dir→非空`NotEmpty`、dir→file`NotDirectory`
- target dir満杯時のchain拡張経由move
