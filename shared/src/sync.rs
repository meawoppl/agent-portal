//! Poison-tolerant `std::sync::Mutex` locking shared across crates.
//!
//! Single home for the repeated
//! `.lock().unwrap_or_else(PoisonError::into_inner)` shape so the call sites
//! cannot drift (e.g. one arm recovering while another panics on a poisoned
//! manifest lock). Everything here is std-only so the crate keeps compiling
//! for `wasm32-unknown-unknown`.

use std::sync::{Mutex, MutexGuard};

/// Lock `mutex`, recovering the guarded value when a previous holder panicked.
///
/// A poisoned lock only means another holder panicked mid-critical-section;
/// the guarded value itself is intact, so these call sites keep going instead
/// of panicking. Poisoning is sticky: without recovery every later lock would
/// fail and the subsystem behind the mutex would wedge (a dead connection
/// never noticed, a manifest never written).
///
/// This is for locks whose critical section has no cross-thread invariant a
/// panic could have broken mid-write. Locks that need poison to mean something
/// else (e.g. the archive scan cache, where poison reads as a cold cache)
/// keep their own handling.
pub fn lock_recovering_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_recovering_poison_reads_through_a_panic() {
        let mutex = Mutex::new(7u32);
        let _ = std::panic::catch_unwind(|| {
            let _guard = mutex.lock().unwrap();
            panic!("simulated holder panic");
        });
        assert!(mutex.is_poisoned());

        // Recovery, not a wedge: the guarded value is intact and writable.
        assert_eq!(*lock_recovering_poison(&mutex), 7);
        *lock_recovering_poison(&mutex) = 8;
        assert_eq!(*lock_recovering_poison(&mutex), 8);
    }
}
