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

// ===== user syscall probe fixture =====

// RV64Iの整数register番号。fixtureはt0/t1/s0/a0..a2/a7だけを使う。
const X0: u32 = 0;
const SP: u32 = 2;
const T0: u32 = 5;
const T1: u32 = 6;
const S0: u32 = 8;
const A0: u32 = 10;
const A1: u32 = 11;
const A2: u32 = 12;
const A7: u32 = 17;

const PROBE_CODE_WORDS: usize = 51;
// fail blockの先頭。すべての結果検査branchはここへ飛ぶ。
const PROBE_FAIL_INDEX: usize = 49;
const USER_SYSCALL_ELF_LEN: usize = 0x1000 + PROBE_CODE_WORDS * 4;

fn addi(rd: u32, rs1: u32, imm: i16) -> u32 {
    ((imm as u32) & 0xfff) << 20 | (rs1 << 15) | (rd << 7) | 0x0013
}

fn sb(rs2: u32, rs1: u32, imm: i16) -> u32 {
    let imm = imm as u32;
    (((imm >> 5) & 0x7f) << 25) | (rs2 << 20) | (rs1 << 15) | ((imm & 0x1f) << 7) | 0x0023
}

fn sd(rs2: u32, rs1: u32, imm: i16) -> u32 {
    let imm = imm as u32;
    (((imm >> 5) & 0x7f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (0b011 << 12)
        | ((imm & 0x1f) << 7)
        | 0x0023
}

fn lui(rd: u32, imm20: u32) -> u32 {
    ((imm20 & 0xfffff) << 12) | (rd << 7) | 0x0037
}

fn bne(rs1: u32, rs2: u32, offset: i32) -> u32 {
    debug_assert!(offset % 4 == 0 && (-4096..4096).contains(&offset));
    let value = offset as u32;
    (((value >> 12) & 0x1) << 31)
        | (((value >> 5) & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (0b001 << 12)
        | (((value >> 1) & 0xf) << 8)
        | (((value >> 11) & 0x1) << 7)
        | 0x0063
}

fn ecall() -> u32 {
    0x0000_0073
}

fn ebreak() -> u32 {
    0x0010_0073
}

fn jal_self() -> u32 {
    0x0000_006f
}

// 検査列: stdout/stderrへのwriteは書いたbyte数を、EBADF、EINVAL、EFAULT、
// ENOSYSは各errnoをa0へ返す。user codeは戻り値を検証し、どれかが違えば
// fail block (未map先0x0へのsd → store page fault) へ飛ぶ。
// 全検査を通した後のebreakが、kernel側の成功marker信号である。
fn assemble_syscall_probe() -> [u32; PROBE_CODE_WORDS] {
    let mut code = [0u32; PROBE_CODE_WORDS];
    let mut i = 0;
    let branch_to_fail = |from: usize| bne(A0, T1, ((PROBE_FAIL_INDEX - from) * 4) as i32);

    // s0 = sp - 64 (0x3fff_ffc0)。"MK4"をuser stackへ組み立てる。
    code[i] = addi(S0, SP, -64);
    i += 1;
    for (offset, byte) in [(0_i16, 0x4d_i16), (1, 0x4b), (2, 0x34)] {
        code[i] = addi(T0, X0, byte);
        i += 1;
        code[i] = sb(T0, S0, offset);
        i += 1;
    }

    // write(1, s0, 3) → 3
    code[i] = addi(A0, X0, 1);
    i += 1;
    code[i] = addi(A1, S0, 0);
    i += 1;
    code[i] = addi(A2, X0, 3);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;
    code[i] = addi(T1, X0, 3);
    i += 1;
    code[i] = branch_to_fail(i);
    i += 1;

    // write(2, s0, 3) → 3
    code[i] = addi(A0, X0, 2);
    i += 1;
    code[i] = addi(A1, S0, 0);
    i += 1;
    code[i] = addi(A2, X0, 3);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;
    code[i] = addi(T1, X0, 3);
    i += 1;
    code[i] = branch_to_fail(i);
    i += 1;

    // write(3, s0, 3) → EBADF (-9)
    code[i] = addi(A0, X0, 3);
    i += 1;
    code[i] = addi(A1, S0, 0);
    i += 1;
    code[i] = addi(A2, X0, 3);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;
    code[i] = addi(T1, X0, -9);
    i += 1;
    code[i] = branch_to_fail(i);
    i += 1;

    // write(1, s0, 4097) → EINVAL (-22)
    code[i] = addi(A0, X0, 1);
    i += 1;
    code[i] = addi(A1, S0, 0);
    i += 1;
    code[i] = lui(A2, 1);
    i += 1;
    code[i] = addi(A2, A2, 1);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;
    code[i] = addi(T1, X0, -22);
    i += 1;
    code[i] = branch_to_fail(i);
    i += 1;

    // write(1, 0x0, 4) → EFAULT (-14)
    code[i] = addi(A0, X0, 1);
    i += 1;
    code[i] = addi(A1, X0, 0);
    i += 1;
    code[i] = addi(A2, X0, 4);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;
    code[i] = addi(T1, X0, -14);
    i += 1;
    code[i] = branch_to_fail(i);
    i += 1;

    // 番号999 → ENOSYS (-38)
    code[i] = addi(A0, X0, 0);
    i += 1;
    code[i] = addi(A7, X0, 999);
    i += 1;
    code[i] = ecall();
    i += 1;
    code[i] = addi(T1, X0, -38);
    i += 1;
    code[i] = branch_to_fail(i);
    i += 1;

    // 成功: kernelがmarkerを出して正常shutdownする。
    code[i] = ebreak();
    i += 1;
    assert!(i == PROBE_FAIL_INDEX, "syscall probe layout drifted");
    code[PROBE_FAIL_INDEX] = sd(X0, X0, 0);
    code[PROBE_FAIL_INDEX + 1] = jal_self();
    code
}

/// `minictr` QEMU testが使うのと同じuser write契約を、1本の決定的ELFで
/// 検査するためのfixtureである。1 segment R+X、entry 0x0010_0000。
pub fn user_syscall_probe_elf() -> [u8; USER_SYSCALL_ELF_LEN] {
    let code = assemble_syscall_probe();
    let mut bytes = [0u8; USER_SYSCALL_ELF_LEN];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&243u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    let header = 64;
    bytes[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes[header + 4..header + 8].copy_from_slice(&5u32.to_le_bytes());
    bytes[header + 8..header + 16].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 16..header + 24].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[header + 32..header + 40].copy_from_slice(&(PROBE_CODE_WORDS * 4).to_le_bytes());
    bytes[header + 40..header + 48].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 48..header + 56].copy_from_slice(&0x1000u64.to_le_bytes());
    for (index, word) in code.iter().enumerate() {
        let offset = 0x1000 + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

const EXIT_PROBE_CODE_WORDS: usize = 22;
const EXIT_PROBE_FAIL_INDEX: usize = 20;
const USER_EXIT_ELF_LEN: usize = 0x1000 + EXIT_PROBE_CODE_WORDS * 4;

fn assemble_exit_probe() -> [u32; EXIT_PROBE_CODE_WORDS] {
    let mut code = [0u32; EXIT_PROBE_CODE_WORDS];
    let mut i = 0;

    // s0 = sp - 64へ"MK5"を置く。
    code[i] = addi(S0, SP, -64);
    i += 1;
    for (offset, byte) in [(0_i16, 0x4d_i16), (1, 0x4b), (2, 0x35)] {
        code[i] = addi(T0, X0, byte);
        i += 1;
        code[i] = sb(T0, S0, offset);
        i += 1;
    }

    // write(1, s0, 3)
    code[i] = addi(A0, X0, 1);
    i += 1;
    code[i] = addi(A1, S0, 0);
    i += 1;
    code[i] = addi(A2, X0, 3);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;

    // write(2, s0, 3)
    code[i] = addi(A0, X0, 2);
    i += 1;
    code[i] = addi(A1, S0, 0);
    i += 1;
    code[i] = addi(A2, X0, 3);
    i += 1;
    code[i] = addi(A7, X0, 1);
    i += 1;
    code[i] = ecall();
    i += 1;

    // exit(42)。kernelが復帰した場合だけfail blockへ落ちる。
    code[i] = addi(A0, X0, 42);
    i += 1;
    code[i] = addi(A7, X0, 2);
    i += 1;
    code[i] = ecall();
    i += 1;
    assert!(i == EXIT_PROBE_FAIL_INDEX, "exit probe layout drifted");
    code[i] = sd(X0, X0, 0);
    code[i + 1] = jal_self();
    code
}

/// stdout、stderr、`exit(42)`とkernel側resource回収を実QEMUで検査するfixture。
pub fn user_exit_probe_elf() -> [u8; USER_EXIT_ELF_LEN] {
    let code = assemble_exit_probe();
    let mut bytes = [0u8; USER_EXIT_ELF_LEN];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&243u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    let header = 64;
    bytes[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes[header + 4..header + 8].copy_from_slice(&5u32.to_le_bytes());
    bytes[header + 8..header + 16].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 16..header + 24].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[header + 32..header + 40].copy_from_slice(&(EXIT_PROBE_CODE_WORDS * 4).to_le_bytes());
    bytes[header + 40..header + 48].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 48..header + 56].copy_from_slice(&0x1000u64.to_le_bytes());
    for (index, word) in code.iter().enumerate() {
        let offset = 0x1000 + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod probe_tests {
    use super::{
        PROBE_FAIL_INDEX, USER_SYSCALL_ELF_LEN, X0, assemble_syscall_probe, ebreak, sd,
        user_exit_probe_elf, user_syscall_probe_elf,
    };
    use crate::elf::{ElfImage, LoadPlan};

    // Catches an ELF envelope that the kernel loader or planner would reject.
    #[test]
    fn syscall_probe_elf_parses_and_plans_with_one_executable_segment() {
        let bytes = user_syscall_probe_elf();
        let image = ElfImage::parse(&bytes).unwrap();
        let plan = LoadPlan::new(&image).unwrap();

        assert_eq!(image.entry().as_u64(), 0x0010_0000);
        assert_eq!(plan.segments().count(), 1);
        let segment = plan.segments().into_iter().next().unwrap();
        assert_eq!(segment.first_page().start().as_u64(), 0x0010_0000);
        assert_eq!(segment.flags().user(), true);
        assert_eq!(segment.flags().read(), true);
        assert_eq!(segment.flags().execute(), true);
        assert_eq!(segment.flags().write(), false);
    }

    // Catches layout drift that would silently desynchronize the fail branch
    // offsets or drop the success signal from the code page.
    #[test]
    fn syscall_probe_code_ends_with_the_success_signal_and_the_fail_block() {
        let bytes = user_syscall_probe_elf();
        let success_slot = 0x1000 + (PROBE_FAIL_INDEX - 1) * 4;
        let fail_slot = 0x1000 + PROBE_FAIL_INDEX * 4;
        let mut word = [0u8; 4];
        word.copy_from_slice(&bytes[success_slot..success_slot + 4]);
        assert_eq!(word, ebreak().to_le_bytes());
        word.copy_from_slice(&bytes[fail_slot..fail_slot + 4]);
        // fail blockは未map先0x0へのsd (0x00003023... sd x0,0(x0)) である。
        assert_eq!(word, sd(X0, X0, 0).to_le_bytes());
        assert_eq!(bytes.len(), USER_SYSCALL_ELF_LEN);
    }

    // Catches an off-by-one branch displacement that routes a failed syscall
    // result to the success ebreak instead of the faulting fail block.
    #[test]
    fn every_syscall_result_branch_targets_the_fail_block() {
        let code = assemble_syscall_probe();
        let mut branch_count = 0;

        for (index, word) in code.into_iter().enumerate() {
            if word & 0x7f != 0x63 {
                continue;
            }
            branch_count += 1;
            let immediate = (((word >> 31) & 0x1) << 12)
                | (((word >> 7) & 0x1) << 11)
                | (((word >> 25) & 0x3f) << 5)
                | (((word >> 8) & 0xf) << 1);
            let signed_immediate = ((immediate as i32) << 19) >> 19;
            let target = index as i32 * 4 + signed_immediate;

            assert_eq!(target, PROBE_FAIL_INDEX as i32 * 4);
        }

        assert_eq!(branch_count, 6);
    }

    // Catches an exit fixture envelope that the real loader rejects before
    // the QEMU test can exercise stdout, stderr, exit, and reclamation.
    #[test]
    fn exit_probe_elf_is_a_loadable_user_executable() {
        let bytes = user_exit_probe_elf();
        let image = ElfImage::parse(&bytes).unwrap();
        let plan = LoadPlan::new(&image).unwrap();

        assert_eq!(image.entry().as_u64(), 0x0010_0000);
        assert_eq!(plan.segments().count(), 1);
        let segment = plan.segments().into_iter().next().unwrap();
        assert!(segment.flags().user());
        assert!(segment.flags().read());
        assert!(segment.flags().execute());
        assert!(!segment.flags().write());
    }
}
