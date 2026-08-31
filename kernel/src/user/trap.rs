//! U-mode trap分類と、`UserContext`を変更する最小限の前処理。

use super::context::{SSTATUS_SPP, UserContext};

/// user trap一名あたりの分類結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapAction {
    /// U-mode由来の`ecall`。`sepc`は実行済みの`ecall`の次の命令へ進めてある。
    SystemCall,
    /// 継続できない例外または割り込み。原因CSR値をそのまま保持する。
    Fatal { scause: usize, stval: usize },
}

/// U-mode `ecall`だけを継続可能と判定し、それ以外を`Fatal`へ分類する。
///
/// 割り込みbitなし・原因code 8・保存済み`sstatus.SPP=0`の三つが揃った場合だけ
/// `sepc`を4 byte進めて`SystemCall`を返す。それ以外の例外と割り込みは、CSR値を
/// 変更せずに`Fatal`へ渡す。
pub fn classify_user_trap(context: &mut UserContext, scause: usize, stval: usize) -> TrapAction {
    let interrupt = 1_usize << (usize::BITS - 1);
    let from_user = context.sstatus() & SSTATUS_SPP == 0;
    if scause & interrupt == 0 && scause & !interrupt == 8 && from_user {
        let resumed = context
            .sepc()
            .checked_add(4)
            .expect("U-mode sepc always leaves room for the next instruction");
        context.set_sepc(resumed);
        TrapAction::SystemCall
    } else {
        TrapAction::Fatal { scause, stval }
    }
}

/// kernel trap handlerが呼ぶ公開入口。分類契約は`classify_user_trap`と同じである。
pub fn handle_user_trap(context: &mut UserContext, scause: usize, stval: usize) -> TrapAction {
    classify_user_trap(context, scause, stval)
}

#[cfg(test)]
mod tests {
    use super::{TrapAction, classify_user_trap, handle_user_trap};
    use crate::user::context::{SSTATUS_SPP, UserContext};

    // Catches advancing anything other than sepc, advancing by the wrong
    // width, or disturbing a single saved register byte on the ecall path.
    #[test]
    fn ecall_advances_only_sepc_and_preserves_registers() {
        let mut context = UserContext::patterned_for_test(0x0010_0100);
        let before = context.registers_for_test();
        let action = classify_user_trap(&mut context, 8, 0);
        assert_eq!(action, TrapAction::SystemCall);
        assert_eq!(context.sepc(), 0x0010_0104);
        assert_eq!(context.registers_for_test(), before);
    }

    // Catches resuming page faults, treating interrupts as syscalls, or
    // trusting an ecall cause code that arrived from S-mode.
    #[test]
    fn page_faults_interrupts_and_supervisor_traps_are_fatal() {
        for scause in [12_usize, 13, 15] {
            let mut context = UserContext::patterned_for_test(0x0010_0200);
            let before = context.registers_for_test();
            assert_eq!(
                classify_user_trap(&mut context, scause, 0x0bad_c0de),
                TrapAction::Fatal {
                    scause,
                    stval: 0x0bad_c0de
                }
            );
            assert_eq!(context.sepc(), 0x0010_0200);
            assert_eq!(context.registers_for_test(), before);
        }

        let timer_interrupt = (1_usize << (usize::BITS - 1)) | 5;
        let mut context = UserContext::patterned_for_test(0x0010_0300);
        assert_eq!(
            classify_user_trap(&mut context, timer_interrupt, 0),
            TrapAction::Fatal {
                scause: timer_interrupt,
                stval: 0
            }
        );
        assert_eq!(context.sepc(), 0x0010_0300);

        let mut context = UserContext::patterned_for_test(0x0010_0400);
        context.set_sstatus_for_test(context.sstatus() | SSTATUS_SPP);
        assert_eq!(
            classify_user_trap(&mut context, 8, 0),
            TrapAction::Fatal {
                scause: 8,
                stval: 0
            }
        );
        assert_eq!(context.sepc(), 0x0010_0400);
    }

    // The kernel entry point must share the classifier's contract exactly.
    #[test]
    fn handle_user_trap_matches_the_classifier_contract() {
        let mut context = UserContext::patterned_for_test(0x0010_0500);
        assert_eq!(handle_user_trap(&mut context, 8, 0), TrapAction::SystemCall);
        assert_eq!(context.sepc(), 0x0010_0504);

        let mut context = UserContext::patterned_for_test(0x0010_0600);
        assert_eq!(
            handle_user_trap(&mut context, 15, 0x10),
            TrapAction::Fatal {
                scause: 15,
                stval: 0x10
            }
        );
    }
}
