//! The seam between the simulator and the machine.
//!
//! Every consumer of simulator state (HAL component, panel, diagnostics)
//! implements [`Sim3dSignal`]; the simulator only ever pushes four facts into it.
//! The HAL implementation is another wave's job, [`MockSignal`] is the test
//! double, and both are usable through `&dyn Sim3dSignal`.
//!
//! `MockSignal` also exposes getters (`probe()`, `collision_hit()`, `tip()`) that
//! share a name with the trait's setter methods. Calling `mock.probe(true)`
//! therefore resolves to the inherent getter and fails to compile. Reach the
//! setters with `Sim3dSignal::probe(&mock, true)` or through a
//! `&dyn Sim3dSignal` reference; reach the getters directly.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::types::P3;

/// The outputs of the 3D simulator.
///
/// Implementations must be cheap and non-blocking: they are called from the
/// simulator's per-step update.
pub trait Sim3dSignal: Send + Sync {
    /// Probe contact state: `true` while the probe touches the workpiece.
    fn probe(&self, contact: bool);
    /// Collision state: `true` while a moving volume intersects something.
    fn collision(&self, hit: bool);
    /// Current probe tip position, in machine millimetres.
    fn tip(&self, point: P3);
    /// A simulator fault worth showing to the operator. The default ignores it.
    fn fault(&self, message: &str) {
        let _ = message;
    }
}

/// A [`Sim3dSignal`] that records what it was told, for tests and dry runs.
#[derive(Debug, Default)]
pub struct MockSignal {
    probe: AtomicBool,
    collision: AtomicBool,
    tip: [AtomicU64; 3],
    faults: Mutex<Vec<String>>,
}

impl MockSignal {
    /// A signal with no contact, no collision, a zero tip and no faults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Last probe contact state.
    pub fn probe(&self) -> bool {
        self.probe.load(Ordering::Relaxed)
    }

    /// Last collision state.
    pub fn collision_hit(&self) -> bool {
        self.collision.load(Ordering::Relaxed)
    }

    /// Last probe tip position.
    pub fn tip(&self) -> P3 {
        [
            f64::from_bits(self.tip[0].load(Ordering::Relaxed)),
            f64::from_bits(self.tip[1].load(Ordering::Relaxed)),
            f64::from_bits(self.tip[2].load(Ordering::Relaxed)),
        ]
    }

    /// Every fault recorded so far, and forget them.
    pub fn take_faults(&self) -> Vec<String> {
        match self.faults.lock() {
            Ok(mut faults) => std::mem::take(&mut *faults),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}

impl Sim3dSignal for MockSignal {
    fn probe(&self, contact: bool) {
        self.probe.store(contact, Ordering::Relaxed);
    }

    fn collision(&self, hit: bool) {
        self.collision.store(hit, Ordering::Relaxed);
    }

    fn tip(&self, point: P3) {
        for i in 0..3 {
            self.tip[i].store(point[i].to_bits(), Ordering::Relaxed);
        }
    }

    fn fault(&self, message: &str) {
        match self.faults.lock() {
            Ok(mut faults) => faults.push(message.to_owned()),
            Err(poisoned) => poisoned.into_inner().push(message.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_records_every_signal() {
        let mock = MockSignal::new();
        assert!(!mock.probe());
        assert!(!mock.collision_hit());
        assert_eq!(mock.tip(), [0.0, 0.0, 0.0]);
        assert!(mock.take_faults().is_empty());

        // Go through the trait, because the inherent getters share the setters'
        // names.
        let signal: &dyn Sim3dSignal = &mock;
        signal.probe(true);
        signal.collision(true);
        signal.tip([12.5, -3.25, 4.0]);
        signal.fault("probe lost");

        assert!(mock.probe());
        assert!(mock.collision_hit());
        assert_eq!(mock.tip(), [12.5, -3.25, 4.0]);
        assert_eq!(mock.take_faults(), vec!["probe lost".to_string()]);
        assert!(mock.take_faults().is_empty(), "faults are taken once");
    }

    #[test]
    fn mock_is_usable_as_a_shared_signal() {
        fn report(signal: &dyn Sim3dSignal) {
            signal.probe(true);
            signal.tip([1.0, 2.0, 3.0]);
        }
        let mock = MockSignal::default();
        report(&mock);
        assert!(mock.probe());
        assert_eq!(mock.tip(), [1.0, 2.0, 3.0]);
    }
}
