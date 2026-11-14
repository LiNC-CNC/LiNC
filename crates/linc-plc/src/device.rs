//! Where the field meets the ladder.
//!
//! Before M5 the scan loop talked to `hal.rs` directly, so the only way to run
//! the I/O path was to own a HAL component — which meant `cargo test` could not
//! see it, and an operator could not run a ladder against a scripted field.
//!
//! [`Device`] is the seam: the daemon reads an [`IoImage`], scans, writes the
//! image, and tells the device how much time passed. [`Hal`](crate::hal::HalDevice)
//! is the field; [`TestDevice`] is a scripted one, which is what makes the
//! handoff's differential tests cheap — a test says what the field presents and
//! asserts what settled on a pin.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::Error;

/// One scan's field I/O, in compiled order.
///
/// Inputs and outputs are separate halves on purpose: a device fills the inputs
/// in [`Device::read`] and consumes the outputs in [`Device::write`], and a
/// single `bits` field would let a bug write a read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IoImage {
    pub input_bits: Vec<bool>,
    pub output_bits: Vec<bool>,
    pub input_words: Vec<i32>,
    pub output_words: Vec<i32>,
    pub input_floats: Vec<i32>,
    pub output_floats: Vec<i32>,
}

impl IoImage {
    /// An image sized for the bindings the pin map froze.
    pub(crate) fn for_map(map: &crate::IoMap) -> Self {
        Self {
            input_bits: vec![false; map.inputs.len()],
            output_bits: vec![false; map.outputs.len()],
            input_words: vec![0; map.word_inputs.len()],
            output_words: vec![0; map.word_outputs.len()],
            input_floats: vec![0; map.float_inputs.len()],
            output_floats: vec![0; map.float_outputs.len()],
        }
    }

    /// The output halves the last scan produced, ready to hand to `write`.
    pub(crate) fn take_outputs(&mut self, vm: &linc_plc_core::Vm) {
        let bits = vm.outputs();
        let words = vm.word_outputs();
        let floats = vm.float_outputs();
        if bits.len() == self.output_bits.len() {
            self.output_bits.copy_from_slice(bits);
        }
        if words.len() == self.output_words.len() {
            self.output_words.copy_from_slice(words);
        }
        if floats.len() == self.output_floats.len() {
            self.output_floats.copy_from_slice(floats);
        }
    }
}

/// The host's view of the scan loop: the heartbeat, the counters and the fault.
///
/// One struct rather than five pin writes, so a device that has nowhere to put
/// them can record them instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status {
    /// The ladder is executing: the heartbeat toggles only while Running.
    pub alive: bool,
    /// A HAL failure or an internal error, kept raised until Load or Stop.
    pub fault: bool,
    pub scan_count: u64,
    pub scan_time_us: u32,
    pub overruns: u64,
}

/// What the scan loop needs from the field.
///
/// Three methods are the scan's (`read`, `write`, `step`), one is its clock
/// (`now`, so a test can make a timer deterministic instead of racing the
/// wall), and two are the host's (`safe`, `set_status`) — the daemon already
/// performed both against HAL pins, and naming them here is what lets the whole
/// path run without a HAL component.
pub trait Device {
    /// Fill the image's input halves with what the field presents now.
    fn read(&mut self, image: &mut IoImage) -> Result<(), Error>;

    /// Drive the image's output halves onto the field.
    fn write(&mut self, image: &IoImage) -> Result<(), Error>;

    /// The device's clock, elapsed since it was created. The daemon measures the
    /// scan delta from it, so a scripted device makes timers reproducible.
    fn now(&self) -> Duration;

    /// Advance the device's own model by one scan. The field itself ignores it;
    /// a plant model uses it to move.
    fn step(&mut self, dt: Duration);

    /// Every bound output to zero. Used by Stop, by a fault, by a load and on
    /// exit — a word output left at its last value would be a command nobody
    /// re-issued.
    fn safe(&mut self) -> Result<(), Error>;

    /// Publish the heartbeat and the counters.
    fn set_status(&mut self, status: Status) -> Result<(), Error>;
}

/// A scripted field: the test writes what the inputs present, reads what the
/// outputs settled to, and owns the clock.
///
/// Cloning gives another handle on the same device, so a test can keep one while
/// the daemon runs in a thread. Every handle sees the same state.
#[derive(Clone, Default)]
pub struct TestDevice {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    scripted_bits: Vec<bool>,
    scripted_words: Vec<i32>,
    scripted_floats: Vec<i32>,
    output_bits: Vec<bool>,
    output_words: Vec<i32>,
    output_floats: Vec<i32>,
    status: Status,
    clock: Duration,
    steps: Vec<Duration>,
    reads: u64,
    writes: u64,
    safes: u64,
}

impl TestDevice {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // A test that panics while holding the lock poisons it; the next test
        // still deserves to see the device rather than a second panic.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What the inputs present from now on. A script shorter than the compiled
    /// input list leaves the rest false; a longer one is ignored past the end.
    pub fn script_bits(&self, bits: &[bool]) {
        self.state().scripted_bits = bits.to_vec();
    }

    pub fn script_words(&self, words: &[i32]) {
        self.state().scripted_words = words.to_vec();
    }

    pub fn script_floats(&self, floats: &[i32]) {
        self.state().scripted_floats = floats.to_vec();
    }

    /// The bit outputs the ladder last drove.
    pub fn outputs(&self) -> Vec<bool> {
        self.state().output_bits.clone()
    }

    pub fn word_outputs(&self) -> Vec<i32> {
        self.state().output_words.clone()
    }

    pub fn float_outputs(&self) -> Vec<i32> {
        self.state().output_floats.clone()
    }

    /// The last status the daemon published.
    pub fn status(&self) -> Status {
        self.state().status
    }

    /// The device's clock, as the daemon measured it.
    pub fn clock(&self) -> Duration {
        self.state().clock
    }

    /// How many scans the daemon completed (one write per scan).
    pub fn writes(&self) -> u64 {
        self.state().writes
    }

    /// How many times the daemon pulled the image.
    pub fn reads(&self) -> u64 {
        self.state().reads
    }

    /// The deltas the daemon stepped through, in order.
    pub fn steps(&self) -> Vec<Duration> {
        self.state().steps.clone()
    }

    /// How many times every output was forced safe.
    pub fn safes(&self) -> u64 {
        self.state().safes
    }
}

impl Device for TestDevice {
    fn read(&mut self, image: &mut IoImage) -> Result<(), Error> {
        let mut state = self.state();
        state.reads += 1;
        for (slot, value) in image
            .input_bits
            .iter_mut()
            .zip(state.scripted_bits.iter().chain(std::iter::repeat(&false)))
        {
            *slot = *value;
        }
        for (slot, value) in image
            .input_words
            .iter_mut()
            .zip(state.scripted_words.iter().chain(std::iter::repeat(&0)))
        {
            *slot = *value;
        }
        for (slot, value) in image
            .input_floats
            .iter_mut()
            .zip(state.scripted_floats.iter().chain(std::iter::repeat(&0)))
        {
            *slot = *value;
        }
        Ok(())
    }

    fn write(&mut self, image: &IoImage) -> Result<(), Error> {
        let mut state = self.state();
        state.writes += 1;
        state.output_bits = image.output_bits.clone();
        state.output_words = image.output_words.clone();
        state.output_floats = image.output_floats.clone();
        Ok(())
    }

    fn now(&self) -> Duration {
        self.state().clock
    }

    fn step(&mut self, dt: Duration) {
        let mut state = self.state();
        state.steps.push(dt);
        state.clock = state.clock.saturating_add(dt);
    }

    fn safe(&mut self) -> Result<(), Error> {
        let mut state = self.state();
        state.safes += 1;
        state
            .output_bits
            .iter_mut()
            .for_each(|value| *value = false);
        state.output_words.iter_mut().for_each(|value| *value = 0);
        state.output_floats.iter_mut().for_each(|value| *value = 0);
        Ok(())
    }

    fn set_status(&mut self, status: Status) -> Result<(), Error> {
        self.state().status = status;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Device, IoImage, TestDevice};

    fn image() -> IoImage {
        IoImage {
            input_bits: vec![false; 2],
            output_bits: vec![false; 2],
            input_words: vec![0; 1],
            output_words: vec![0; 1],
            input_floats: vec![0; 1],
            output_floats: vec![0; 1],
        }
    }

    #[test]
    fn a_scripted_input_reaches_the_image_and_a_written_output_is_recorded() {
        let mut device = TestDevice::new();
        device.script_bits(&[true, false]);
        device.script_words(&[7]);
        device.script_floats(&[-3]);

        let mut image = image();
        device.read(&mut image).expect("read");
        assert_eq!(image.input_bits, vec![true, false]);
        assert_eq!(image.input_words, vec![7]);
        assert_eq!(image.input_floats, vec![-3]);

        image.output_bits = vec![false, true];
        image.output_words = vec![42];
        image.output_floats = vec![9];
        device.write(&image).expect("write");
        assert_eq!(device.outputs(), vec![false, true]);
        assert_eq!(device.word_outputs(), vec![42]);
        assert_eq!(device.float_outputs(), vec![9]);
    }

    #[test]
    fn a_short_script_leaves_the_rest_of_the_field_false() {
        let mut device = TestDevice::new();
        device.script_bits(&[true]);
        let mut image = image();
        device.read(&mut image).expect("read");
        assert_eq!(
            image.input_bits,
            vec![true, false],
            "an unscripted input reads false, so a test only names what matters"
        );
    }

    #[test]
    fn the_clock_is_the_devices_and_only_step_moves_it() {
        let mut device = TestDevice::new();
        assert_eq!(device.now(), std::time::Duration::ZERO);
        device.step(std::time::Duration::from_millis(20));
        device.step(std::time::Duration::from_millis(5));
        assert_eq!(device.now(), std::time::Duration::from_millis(25));
        assert_eq!(device.steps().len(), 2, "every step is recorded in order");
    }

    #[test]
    fn safe_zeroes_every_output_and_status_is_kept() {
        let mut device = TestDevice::new();
        let mut image = image();
        image.output_bits = vec![true, true];
        image.output_words = vec![5];
        image.output_floats = vec![6];
        device.write(&image).expect("write");

        device.safe().expect("safe");
        assert_eq!(device.outputs(), vec![false, false]);
        assert_eq!(device.word_outputs(), vec![0]);
        assert_eq!(device.float_outputs(), vec![0]);
        assert_eq!(device.safes(), 1);

        let status = super::Status {
            alive: true,
            fault: false,
            scan_count: 9,
            scan_time_us: 250,
            overruns: 1,
        };
        device.set_status(status).expect("status");
        assert_eq!(device.status(), status);
    }
}
