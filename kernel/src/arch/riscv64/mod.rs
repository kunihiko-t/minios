// このアセンブリは RISC-V 64 のみで有効であり、ホストテストへは混入しない。
core::arch::global_asm!(include_str!("entry.S"));

pub mod sbi;
