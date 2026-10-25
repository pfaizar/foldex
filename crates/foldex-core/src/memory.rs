use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Shared reservations include retained entry chunks plus bounded ordering/query headroom.
/// They remain charged while any progressive snapshot or UI clone owns the entry store.
#[derive(Debug)]
pub(crate) struct MemoryBudget {
    limit: usize,
    used: AtomicUsize,
}
impl MemoryBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
    pub fn available(&self) -> usize {
        self.limit.saturating_sub(self.used())
    }
    pub fn reservation(self: &Arc<Self>) -> Arc<MemoryReservation> {
        Arc::new(MemoryReservation {
            budget: self.clone(),
            charged: AtomicUsize::new(0),
        })
    }
}

#[derive(Debug)]
pub(crate) struct MemoryReservation {
    budget: Arc<MemoryBudget>,
    charged: AtomicUsize,
}
impl MemoryReservation {
    pub fn grow_to(&self, bytes: usize) -> bool {
        const QUANTUM: usize = 16 * 1024;
        let Some(rounded) = bytes
            .checked_add(QUANTUM - 1)
            .map(|value| value / QUANTUM * QUANTUM)
        else {
            return false;
        };
        let previous = self.charged.load(Ordering::Acquire);
        if rounded <= previous {
            return true;
        }
        let increase = rounded - previous;
        if self
            .budget
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(increase)
                    .filter(|&total| total <= self.budget.limit)
            })
            .is_err()
        {
            return false;
        }
        // One scan builder grows a reservation. Other threads only hold or inspect it.
        self.charged.store(rounded, Ordering::Release);
        true
    }

    pub fn shrink_to(&self, bytes: usize) {
        const QUANTUM: usize = 16 * 1024;
        let rounded = bytes.saturating_add(QUANTUM - 1) / QUANTUM * QUANTUM;
        let previous = self.charged.load(Ordering::Acquire);
        if rounded < previous {
            self.charged.store(rounded, Ordering::Release);
            self.budget
                .used
                .fetch_sub(previous - rounded, Ordering::AcqRel);
        }
    }
}
impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.budget
            .used
            .fetch_sub(self.charged.load(Ordering::Relaxed), Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_reservations_enforce_global_cap_and_release_on_last_owner() {
        let budget = MemoryBudget::new(64 * 1024);
        let first = budget.reservation();
        assert!(first.grow_to(30 * 1024));
        let pinned = first.clone();
        let second = budget.reservation();
        assert!(second.grow_to(32 * 1024));
        assert!(!second.grow_to(33 * 1024));
        drop(first);
        assert_eq!(budget.used(), 64 * 1024);
        drop(pinned);
        assert_eq!(budget.used(), 32 * 1024);
        drop(second);
        assert_eq!(budget.used(), 0);
    }
}
