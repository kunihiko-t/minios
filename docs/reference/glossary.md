# 用語集

## ABI

Application Binary Interface。関数名、register、stack alignment、戻り値などbinary同士の契約です。
MiniOSではOpenSBIの`a0/a1` entry引数、SBI call register、assemblyからRustへのC ABIが該当します。

## BSS

zero-initialized static dataを置くELF sectionです。fileにはzero bytesを持たず、MiniOSの`entry.S`がRustへ
入る前に`__bss_start..__bss_end`をzero化します。

## CSR

Control and Status Register。RISC-Vの特権状態を読み書きするregister群で、`scause`、`sepc`、`stval`、
`stvec`、`sstatus`、`sie`、`time`を使います。

## DTB

Device Tree Blob。CPU、RAM、device address、interruptなどhardware構成を表すbinaryです。OpenSBIはaddressを
`a1`で渡しますが、現在のMiniOSはまだ解析しません。

## hart

Hardware thread。独立にinstructionを実行するRISC-Vの単位です。MiniOSはQEMUを`-smp 1`で起動し、boot hart
ID 0だけを扱います。

## ISA

Instruction Set Architecture。CPUが実行できるinstructionとregister/privilege仕様です。guest targetはRISC-V
64のRV64GCです。

## MMIO

Memory-Mapped I/O。device registerをphysical addressへ割り当てる方式です。UART base `0x1000_0000`はRAMでは
なく、read/writeがdevice actionになります。

## OpenSBI

RISC-V Supervisor Binary Interfaceのfirmware implementationです。QEMUからM-modeで起動し、MiniOSをS-modeへ
渡し、timer予約とsystem resetを仲介します。

## page

memoryを固定sizeで管理する単位です。MiniOSのphysical pageは4 KiBで、先頭addressは4096-byte alignedです。

## privilege mode

RISC-Vの権限levelです。OpenSBIはMachine mode、MiniOS kernelはSupervisor modeで動きます。User modeはroadmap
項目で、現在は未実装です。

## SBI

Supervisor Binary Interface。S-mode softwareがfirmwareへtimer、resetなどを依頼するstandard ABIです。
MiniOSはTIMEとSRST extensionを使います。

## trap

exceptionまたはinterruptにより通常control flowから特権handlerへ移るeventです。`scause`が種類、`sepc`が
中断位置、`stval`が追加情報を表します。

## UART

Universal Asynchronous Receiver/Transmitter。MiniOSのconsoleに使うbyte-oriented serial deviceです。QEMU
`virt`の16550 compatible implementationをstdioへ接続します。

## volatile access

compilerへ「このread/write自体を省略・統合してはいけない」と伝えるmemory accessです。MMIOには必要ですが、
thread間同期やatomicityを保証するものではありません。
