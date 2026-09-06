# MiniOS NEORV32 read-only FAT32 design

## Status

This specification defines the first storage milestone for MiniOS on the
Tang Nano 20K. MiniOS continues to boot from its existing image. After boot,
the RV32 shell can inspect files on the board's built-in microSD slot.

## Goal

Add two RV32 shell commands:

- `ls` lists entries in the FAT32 root directory.
- `cat NAME.EXT` writes one regular root-directory file to the console.

The implementation must preserve the existing UART shell and must fit in the
NEORV32 internal memories defined by `kernel/linker_neorv32.ld`.

## Architecture

The implementation has three small layers inside the MiniOS repository:

1. A NEORV32 GPIO-SPI bus drives the Tang Nano 20K microSD pins.
2. An SD protocol module initializes an SDHC or SDXC card and reads a
   512-byte sector with CMD17 and CRC16 verification.
3. A read-only FAT32 module parses the partition, boot sector, FAT and root
   directory through a sector-reader interface that can also be supplied by
   host tests.

The shell owns one SD/FAT32 session and invokes it synchronously. No heap,
background task, cache, VFS or third-party crate is introduced.

The FPGA repository remains responsible for HDL, pin constraints and the
bitstream. MiniOS does not depend on FPGA-repository source files or scripts.
The existing FPGA `sd_probe` remains a standalone hardware diagnostic; its
proven protocol behavior is adapted into MiniOS and covered by MiniOS tests.

## Hardware contract

The already-tested GPIO mapping is fixed for this milestone:

| GPIO | SD signal | Tang Nano 20K pin |
| --- | --- | --- |
| 0 | CLK | 83 |
| 1 | CMD/MOSI | 82 |
| 2 | logical active-high select, inverted by the FPGA to DAT3/CS | 81 |
| 3 | DAT0/MISO | 84 |

DAT1 and DAT2 are unused in SPI mode. Reset and every error path must leave
the card deselected, CLK low and MOSI high. Initialization runs at no more
than 375 kHz with the 96 MHz CPU clock.

The SD layer supports only SDHC and SDXC block addressing. It sends only
CMD0, CMD8, CMD55/ACMD41, CMD58 and CMD17. It does not implement media write,
erase or format commands.

## Storage layout

Every logical sector is exactly 512 bytes. The FAT32 volume is found by one
of these rules:

1. If LBA 0 is a valid FAT32 boot sector, mount it as a superfloppy.
2. Otherwise require an MBR with signature `55aa`, scan its four primary
   entries in order, and mount the first non-empty type `0x0b` or `0x0c`
   partition.

GPT, extended partitions, protective MBRs, FAT12, FAT16 and exFAT are
unsupported. A 64 GB card may therefore need to be reformatted as FAT32
outside MiniOS after its existing contents have been checked.

The FAT32 parser validates all values before address arithmetic. At minimum
it requires 512-byte sectors, a power-of-two sectors-per-cluster value in the
FAT32 range, a nonzero reserved-sector count, one or two FATs, a nonzero
FAT32 FAT size, a zero FAT16 root-entry count and FAT size, a root cluster of
at least 2, and data/FAT ranges that remain inside the selected volume.
Arithmetic uses checked operations. Invalid or cyclic cluster chains fail
instead of reading outside the volume; traversal is bounded by the computed
data-cluster count.

## File behavior

The first version supports the FAT32 root directory and 8.3 short names
only. It ignores deleted entries, long-file-name entries and volume labels.
`ls` prints each regular file or directory with its normalized short name;
regular files also include their byte length.

`cat` performs an ASCII case-insensitive short-name lookup in the root. It
accepts exactly one path-free 8.3 name, rejects directories, and streams at
most the directory entry's declared file size through a single 512-byte
buffer. An empty file succeeds without reading a data cluster. Subdirectory
traversal and long-name lookup are unsupported.

## Interfaces

The SD protocol keeps the existing testable byte-bus boundary:

```rust
trait Bus {
    fn select(&mut self, active: bool) -> Result<(), SdError>;
    fn transfer(&mut self, tx: u8) -> Result<u8, SdError>;
    fn delay_ms(&mut self, milliseconds: u32);
}
```

The FAT32 parser depends on one sector boundary:

```rust
trait SectorReader {
    type Error;
    fn read_sector(
        &mut self,
        lba: u32,
        destination: &mut [u8; 512],
    ) -> Result<(), Self::Error>;
}
```

The production implementation is a NEORV32 GPIO SD reader. The traits exist
only to isolate host tests from MMIO; they are not general device or VFS
frameworks.

## Errors and console output

SD and FAT32 errors are typed internally. The shell maps them to short,
stable messages prefixed with `sd:`. Required user-visible cases are:

- card initialization or sector read failure;
- unsupported or invalid partition/filesystem;
- root entry not found;
- requested entry is a directory;
- invalid `cat` argument.

An error returns to the `minios> ` prompt and releases chip select. No error
may panic the kernel or issue a write command to the card.

## Tests

Development follows red-green-refactor. Host tests cover:

- the proven SD initialization frames, block address, CRC16, timeout and
  chip-select cleanup behavior;
- superfloppy detection and ordered MBR primary-partition selection;
- rejection of unsupported layouts and malformed or overflowing BPB values;
- root-directory iteration across sector and cluster boundaries;
- deleted, LFN and volume-label filtering;
- 8.3 name normalization and case-insensitive lookup;
- empty, one-sector and multi-cluster file streaming;
- bad, out-of-range and cyclic cluster chains;
- `ls` and `cat` command parsing without changing RV64 command behavior.

The build gate is the existing host test suite plus a locked RV32 release
build. The resulting ELF must fit the effective 32 KiB (32,768-byte) IMEM
and the unchanged 16,192-byte DMEM contract in
`kernel/linker_neorv32.ld`. The FPGA top level and linker now both declare
32,768 bytes. This is the power-of-two address range implemented by NEORV32;
the former 24,288-byte non-power-of-two request was rounded up to 32 KiB.
Existing place-and-route reports show this address range without an additional
BSRAM cost. The explicit 32 KiB contract therefore matches the hardware
address range while leaving DMEM unchanged.

The hardware acceptance test uses a FAT32 card containing a known root file.
After the existing MiniOS image starts:

1. `ls` prints the known short name and correct size.
2. `cat` with either upper- or lower-case spelling prints the exact contents.
3. `cat` of a missing name prints the stable not-found error.
4. The shell remains responsive after each success and failure.

## Out of scope

- Booting MiniOS or another program from SD.
- SD write, erase, format, file creation or file update.
- exFAT, FAT12, FAT16, GPT and extended partitions.
- Long file names and subdirectory traversal.
- A VFS, block cache, asynchronous I/O or hardware SPI redesign.
- Moving the FPGA/OpenOCD build and load workflow into `xtask`; that is a
  separate follow-up after the storage path works on hardware.
