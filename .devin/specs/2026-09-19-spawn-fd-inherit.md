# spawn時のfd継承

## 背景

`spawn`で生成されるprocessのfd tableは現在`FileFdTable::new()`で空の状態から
始まる。POSIXではfork/spawnしたchildはparentのopen fileを引き継ぐのが自然で、
将来のpipe等のprocess間連携（親が作ったchannelをchildがfd経由で使う）の前提と
なる。`FileFd`/`FileDesc`は`Copy`なので、tableのsnapshotをchildへ渡すだけで
実現できる。

## 目標

- `spawn` syscallで生成されるprocessが、呼び出しprocessのfd tableの
  snapshotを引き継ぐ（常時継承、flagや引数の追加はしない）。
- 継承はcopyであり、spawn後のoffset変更は互いに影響しない
  （POSIXのshared file descriptionではなくsnapshot semanticsと明記する）。
- manifestから起動する初期processは従来どおり空tableで開始する。

## 設計

### `Process::spawn`の契約変更

`spawn`に`file_fds: FileFdTable`パラメータを追加し、`FileFdTable::new()`の
代わりに受け取ったtableをそのまま初期tableとする。spawn側はtableの中身を
解釈しないため、継承か新規かはcallerの責務となる。

`FileFdTable`に`Clone`と`Copy`をderiveする（RV64は`[Option<FileFd>; 4]`、
RV32はZSTのまま）。`pub fn spawn`の引数型となるため`pub`へ引き上げる。

### trap窓accessor

`spawn_process`（`kernel/src/main.rs`）が`CURRENT_PROC`のfd tableを
`Process::file_fds_snapshot()`（`FileFdTable`のcopyを返す）で取得し、
`Process::spawn`へ渡す。`CURRENT_PROC`が未設定なら空tableを使う
（syscall経路では必ず設定済みだが、defensiveにfallbackする）。
manifest起動経路（`main.rs`の初期spawn）は`FileFdTable::new()`を渡す。

### 既存のtable全体fd管理との整合

`unlink`の`revoke_file_fds`とcross-directory `rename`の
`relocate_file_fds`はすでに`ProcessTable`内の全processへ効くため、
childが引き継いだfdが親のunlink/renameでstale化することはない。
この前提をhost testで固定する。

## エラーとエッジケース

- table満杯（`EMFILE`相当）はchild側では起きない。parentのtableが
  有効なslotだけを持つ限りcopyは常に成功する。
- parentがspawn後にfdをclose/seekしてもchild側のcopyへは影響しない。
- child側の`read`/`write`はparentと同じdir entry位置を指すため、
  file contentのwrite-back先は一致する（offsetだけが独立）。

## 検証

### host test

- `Process::spawn`へtableを渡した場合、childのtableがsnapshotを持つ
  （offsetを進めたfdがその位置を引き継ぐ）。
- manifest経路相当の`FileFdTable::new()`指定ではchildが空tableを持つ。
- spawn後にparent側で`advance`/`close`してもchild側のcopyは変わらない。
- `revoke_file_fds`/`relocate_file_fds`が継承したfdを持つchildにも効く。

### QEMU検証（`file-fdinherit` phase）

`DOCS/FDCHILD.ELF`（手組みELF64、cluster 8に新規追加）をdisk fixtureへ
追加する。codeは`read(3, sp-64, 13)` → 13 byteなら`write(1, buf, 13)` →
`exit(42)`、それ以外は`exit(70)`。bufferはR+X segmentでは書けないため
`sp`上に取る。

guest `file_fdinherit`は:

1. `open("DOCS/NOTE.TXT")` → fd 3。
2. `lseek(3, 4)`でoffsetを4へ（snapshotにoffsetが乗ることの確認）。
3. `spawn("DOCS/FDCHILD.ELF")` → child pid。
4. `waitpid(pid)`でblockしchild完了を待つ。
5. parent側で`read(3, buf, 64)`が`" inside docs\n"`（offset 4からの
   13 byte、childのoffset進行に影響されない）を返すことを確認。
6. `fd-inherit verified`をstdoutへ出して`exit(42)`。

childは継承したfd 3から`" inside docs\n"`をstdoutへ写す。parentが
`waitpid`でblockするため、childのstdout+Exitはparentのmarkerより必ず先に
出る——frameのexact順序照合が可能。

### fixtureの連鎖更新

`DOCS`に3件目のentryが増えるため、既存fixture期待値を更新する:

- `file-readdir` guestのDOCS列挙期待（2件→3件、順序はslot順で
  NOTE.TXT, CHILD.ELF, FDCHILD.ELF）。
- shell対話testの`ls DOCS`期待行に`       200 FDCHILD.ELF`を追加
  （0xc8 = 200 byte）。
- `xtask/src/disk.rs`のDOCS entry一覧assert。

## ドキュメント

- `minicontainer-abi.md`: `spawn`行にfd table snapshot継承を明記。
- `architecture.md`/`11-test-harness.md`/`roadmap.md`: phase 48の追加と
  継承semanticsを反映。

## 非目標

- `spawn`へのargv引き渡し（childがfd番号を動的に知る仕組み）。
  継承fdのslot番号を規約で固定して回す。
- pipe等のkernel object本体。本specはその前提となる継承機構のみ。
- fork的なaddress space共有。imageは常に新規loadする。
