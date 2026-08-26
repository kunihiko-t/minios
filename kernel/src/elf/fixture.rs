//! Deterministic ELF bytes shared only by tests and the ELF QEMU test feature.

const ELF_LEN: usize = 0x2004;

/// Returns a deterministic ELF64 RISC-V executable used by host and QEMU tests.
pub fn valid_riscv64_elf() -> [u8; ELF_LEN] {
    let mut bytes = [0u8; ELF_LEN];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, 243);
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, 0x0010_0000);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 2);

    write_program_header(
        &mut bytes,
        64,
        ProgramHeaderFields {
            flags: 5,
            offset: 0x1000,
            virtual_address: 0x0010_0000,
            file_size: 4,
            memory_size: 4,
            alignment: 0x1000,
        },
    );
    write_program_header(
        &mut bytes,
        120,
        ProgramHeaderFields {
            flags: 6,
            offset: 0x2000,
            virtual_address: 0x0020_0000,
            file_size: 4,
            memory_size: 0x1000,
            alignment: 0x1000,
        },
    );
    bytes[0x1000..0x1004].copy_from_slice(&[0x13, 0x00, 0x00, 0x00]);
    bytes[0x2000..0x2004].copy_from_slice(b"MCB1");
    bytes
}

struct ProgramHeaderFields {
    flags: u32,
    offset: u64,
    virtual_address: u64,
    file_size: u64,
    memory_size: u64,
    alignment: u64,
}

fn write_program_header(bytes: &mut [u8], start: usize, fields: ProgramHeaderFields) {
    put_u32(bytes, start, 1);
    put_u32(bytes, start + 4, fields.flags);
    put_u64(bytes, start + 8, fields.offset);
    put_u64(bytes, start + 16, fields.virtual_address);
    put_u64(bytes, start + 24, 0);
    put_u64(bytes, start + 32, fields.file_size);
    put_u64(bytes, start + 40, fields.memory_size);
    put_u64(bytes, start + 48, fields.alignment);
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
