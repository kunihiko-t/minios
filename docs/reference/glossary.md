# 用語集

## ABI

**Application Binary Interface（ABI）**は、関数名、レジスター、スタックのアラインメント、戻り値など、バイナリー同士の規約です。
MiniOSでは、OpenSBIが`a0/a1`で渡す起動引数、SBI呼び出しで使うレジスター、アセンブリーコードからRustへ移るC ABIが該当します。

## BSS

**BSS**は、ゼロで初期化する静的データを置くELFセクションです。
ファイルにはゼロの並びを保持せず、MiniOSの`entry.S`がRustへ入る前に`__bss_start..__bss_end`をゼロ化します。

## CSR

**Control and Status Register（CSR）**は、RISC-Vの特権状態を読み書きするレジスター群です。
MiniOSは`scause`、`sepc`、`stval`、`stvec`、`sstatus`、`sie`、`time`を使います。

## DTB

**Device Tree Blob（DTB）**は、CPU、RAM、機器のアドレス、割り込みなど、ハードウェア構成を表すバイナリーです。
OpenSBIはそのアドレスを`a1`で渡しますが、現在のMiniOSはまだ内容を解析しません。

## hart

**ハート**は、RISC-Vで命令を独立して実行するハードウェアスレッドです。
MiniOSはQEMUを`-smp 1`で起動し、IDが0の起動ハートだけを扱います。

## ISA

**Instruction Set Architecture（ISA）**は、CPUが実行できる命令、レジスター、特権の仕様です。
ゲストのターゲットは、RISC-V 64のRV64GCです。

## MMIO

**Memory-Mapped I/O（MMIO）**は、機器のレジスターを物理アドレスへ割り当てる方式です。
UARTのベースアドレス`0x1000_0000`はRAMではなく、その読み書きが機器への操作になります。

## OpenSBI

**OpenSBI**は、RISC-V Supervisor Binary Interfaceのファームウェア実装です。
QEMUからM-modeで起動し、MiniOSをS-modeへ渡した後、タイマーの予約とシステムリセットを仲介します。

## page

**ページ**は、メモリーを固定サイズで管理する単位です。
MiniOSの物理ページは4 KiBで、先頭アドレスは4096バイト境界にそろえます。
`PhysFrame`は、アロケーターから払い出された1ページの排他所有を表す値です。
`Clone`と`Copy`を実装せず、解放時に消費されます。
この値を作る`unsafe`な境界は、同じアドレスを表す別の値と、別のサブシステムによる所有がないことを呼び出し側へ要求します。
アラインメントは検査できますが、ハードウェア上の所有者重複は自動検出できません。

## privilege mode

**特権モード**は、RISC-Vにおける権限レベルです。
OpenSBIはMachineモード、MiniOSのカーネルはSupervisorモードで動きます。
Userモードはロードマップに含まれますが、まだ実装していません。

## SBI

**Supervisor Binary Interface（SBI）**は、S-modeのソフトウェアがファームウェアへタイマーやリセットを依頼する標準ABIです。
MiniOSはTIME拡張とSRST拡張を使います。

## trap

**トラップ**は、例外または割り込みにより、通常の制御の流れから特権ハンドラーへ移る事象です。
`scause`が種類、`sepc`が中断位置、`stval`が追加情報を表します。

## UART

**Universal Asynchronous Receiver/Transmitter（UART）**は、MiniOSのコンソールに使う、1バイト単位のシリアル機器です。
QEMU `virt`の16550互換実装を標準入出力へ接続します。

## volatile access

**volatileアクセス**は、この読み書き自体を省略または統合してはいけないとコンパイラーへ伝えるメモリーアクセスです。
MMIOには必要ですが、スレッド間の同期と不可分な操作を保証するものではありません。
