//! hart間で共有するstateを守るtest-and-set spinlock。
//!
//! 割り込みは止めないため、同じhartでlock保持中に割り込みhandlerが
//! 同じlockを取るとdeadlockする。保持中に割り込み経路が触れる対象へは
//! 使わないこと（`console`のprint lockと割り込みhandlerがprintしない
//! 規約の組み合わせが先例）。
//!
//! `wfi`とは違い`spin_loop`はCPUを遊ばせるだけなので、長い待機ではなく
//! 「他hartがすぐ離す短い区画」の保護に向く。

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// test-and-setの`SpinLock`。`lock`がguardを返し、dropで解放する。
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// lock経由でしか中身に触れないので、Send可能な値ならhartをまたいで
// 共有して良い。
unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// 解放されるまでspinして排他guardを返す。
    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // 占有中はloadで回してcache lineをinvalidationから守る。
            while self.locked.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
        SpinGuard { lock: self }
    }

    /// 占有中なら`None`、空いていれば獲得してguardを返す。
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(SpinGuard { lock: self })
    }
}

/// `SpinLock::lock`の排他guard。dropで解放する。
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // Safety: guardの生存中はこのhartだけがvalueへ触れる。
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // Safety: guardの生存中はこのhartだけがvalueへ触れる。
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::vec::Vec;

    use super::SpinLock;

    #[test]
    fn spin_lock_serializes_concurrent_increments() {
        let lock = Arc::new(SpinLock::new(0usize));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..1_000 {
                        *lock.lock() += 1;
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(*lock.lock(), 4_000);
    }

    #[test]
    fn try_lock_reports_contention_and_release() {
        let lock = SpinLock::new(0u8);
        let guard = lock.lock();
        assert!(lock.try_lock().is_none());
        drop(guard);
        assert!(lock.try_lock().is_some());
    }
}
