//! RV64 user register context with the exact C ABI layout shared with `user.S`.

use crate::vm::VirtAddr;

/// `sstatus.SPP`: 0は`sret`後のU-mode遷移、1はS-mode残留を意味する。
pub const SSTATUS_SPP: usize = 1 << 8;
/// `sstatus.SPIE`: `sret`がSIEへコピーする、トラップ前の割り込み許可値。
pub const SSTATUS_SPIE: usize = 1 << 5;

/// `user.S`が`sd`/`ld`の固定offsetで直接指す、正確なC ABI layoutである。
/// `registers[0]`は常に0、`registers[2]`はuser stack top、
/// `registers[10..=17]`は`a0..=a7`で、`sepc`と`sstatus`が続く。
#[repr(C)]
pub struct UserContext {
    registers: [usize; 32],
    sepc: usize,
    sstatus: usize,
}

/// `__run_user`がRust callerへ返す実行結果。
/// `user.S`はdiscriminant値をそのまま`a0`へ載せるため、`#[repr(usize)]`で固定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum RunExit {
    Resume = 0,
    ReturnToKernel = 1,
}

impl UserContext {
    /// Builds the initial U-mode frame: PC at `entry`, sp at `stack_top`,
    /// and `sstatus` cleared of SPP so `sret` lands in U-mode with the
    /// pre-trap interrupt state restored through SPIE.
    pub fn new(entry: VirtAddr, stack_top: VirtAddr) -> Self {
        Self {
            registers: [0; 32],
            sepc: usize::try_from(entry.as_u64()).expect("Sv39 addresses fit usize"),
            sstatus: SSTATUS_SPIE,
        }
        .with_user_stack(stack_top)
    }

    fn with_user_stack(mut self, stack_top: VirtAddr) -> Self {
        self.registers[2] = usize::try_from(stack_top.as_u64()).expect("Sv39 addresses fit usize");
        self
    }

    /// Returns the raw register value at ABI index `x{index}`.
    pub fn register(&self, index: usize) -> usize {
        self.registers[index]
    }

    /// Returns the program counter `sret` will enter.
    pub const fn sepc(&self) -> usize {
        self.sepc
    }

    /// Returns the `sstatus` value installed before `sret`.
    pub const fn sstatus(&self) -> usize {
        self.sstatus
    }

    /// Updates the program counter. Only the trap classifier advances it,
    /// by exactly one `ecall` instruction width.
    pub(crate) fn set_sepc(&mut self, value: usize) {
        self.sepc = value;
    }

    /// Writes one register slot. Only the syscall dispatcher records the
    /// guest-visible return value through this boundary.
    pub(crate) fn set_register(&mut self, index: usize, value: usize) {
        self.registers[index] = value;
    }
}

#[cfg(test)]
impl UserContext {
    /// Builds a context whose every register slot holds a distinct pattern.
    ///
    /// Trap-classification tests use it to prove that only `sepc` moves and
    /// every saved register byte survives the round trip.
    pub(crate) fn patterned_for_test(sepc: usize) -> Self {
        let mut context = Self {
            registers: [0; 32],
            sepc,
            sstatus: SSTATUS_SPIE,
        };
        for (index, slot) in context.registers.iter_mut().enumerate() {
            *slot = 0x5150_0000_0000_0000 | index;
        }
        context
    }

    /// Returns a copy of the raw register file for exact-equality assertions.
    pub(crate) fn registers_for_test(&self) -> [usize; 32] {
        self.registers
    }

    /// Forces `sstatus`, used to model a trap that originated in S-mode.
    pub(crate) fn set_sstatus_for_test(&mut self, sstatus: usize) {
        self.sstatus = sstatus;
    }
}

#[cfg(test)]
mod tests {
    use super::{SSTATUS_SPIE, SSTATUS_SPP, UserContext};
    use crate::vm::VirtAddr;

    // Catches an entry/stack swap, a leftover SPP=1, a missing SPIE, or any
    // layout drift away from the 34-word frame that `user.S` indexes by hand.
    #[test]
    fn initial_context_uses_entry_stack_and_user_sstatus() {
        let context = UserContext::new(
            VirtAddr::try_new(0x0010_0000).unwrap(),
            VirtAddr::try_new(0x4000_0000).unwrap(),
        );
        assert_eq!(context.register(2), 0x4000_0000);
        assert_eq!(context.sepc(), 0x0010_0000);
        assert_eq!(context.sstatus() & SSTATUS_SPP, 0);
        assert_eq!(context.sstatus() & SSTATUS_SPIE, SSTATUS_SPIE);
        assert_eq!(size_of::<UserContext>(), 34 * size_of::<usize>());
    }

    // Catches writing the stack pointer into any slot other than x2, or
    // leaving a nonzero value in the hardwired-zero x0 slot.
    #[test]
    fn zero_register_and_argument_slots_start_at_zero() {
        let context = UserContext::new(
            VirtAddr::try_new(0x0010_0000).unwrap(),
            VirtAddr::try_new(0x4000_0000).unwrap(),
        );
        assert_eq!(context.register(0), 0);
        for index in [1, 3, 10, 17, 31] {
            assert_eq!(context.register(index), 0, "x{index} must start at zero");
        }
    }
}
