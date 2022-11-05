#![allow(unused)]

pub(super) struct Backoff;

// On Apple ARM devices, a single WFE deschedules the CPU better than (multiple) YIELD.
#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
impl Backoff {
    pub(super) fn spin_loop_hint() {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) }
    }

    pub(super) fn yield_now() {
        Self::spin_loop_hint()
    }
}

#[cfg(not(all(target_vendor = "apple", target_arch = "aarch64")))]
impl Backoff {
    pub(super) fn spin_loop_hint() {
        core::hint::spin_loop()
    }

    // Simple (racy) LCG randomization to spin for a random but bounded amount of time.
    // Tries to prevent threads from resonating at the same frequency and permanently contending.
    // https://github.com/apple/swift-corelibs-libdispatch/blob/main/src/shims/yield.h#L105
    pub(super) fn yield_now() {
        use core::sync::atomic::{AtomicUsize, Ordering};
        static RNG_SEED: AtomicUsize = AtomicUsize::new(1);

        let seed = RNG_SEED.load(Ordering::Relaxed);
        RNG_SEED.store(seed.wrapping_mul(1103515245).wrapping_add(12345), Ordering::Relaxed);

        const MAX_SPINS: usize = 128 - 1;
        const MIN_SPINS: usize = 32 - 1;

        let spins = ((seed >> 24) & MAX_SPINS) | MIN_SPINS;
        for _ in 0..spins {
            Self::spin_loop_hint();
        }
    }
}
