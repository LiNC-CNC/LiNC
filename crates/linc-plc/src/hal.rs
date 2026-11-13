//! The `linc_plc.*` HAL surface: pin creation and the only legal access to it.
//!
//! Only the daemon's main thread touches these values. Pins are created from
//! the project's I/O bindings — the map is frozen at startup — plus the five
//! control outputs, all before `hal_ready()`.

use linc_hal::{HalComponent, HalPin};

use crate::{Config, Error, IoMap};

/// The HAL type of a bound pin. The variant, not a cast, decides how the pin's
/// data slot is read: reinterpreting an `s32` slot as a bit would silently read
/// the wrong bytes.
enum Pin {
    Bit(HalPin),
    S32(HalPin),
    Float(HalPin),
    /// `--no-hal`: the ladder still scans but nothing is wired.
    Off,
}

/// One project I/O binding and the pin it was created for. The name is kept for
/// the `Unlinked` diagnostic, which has to say which binding lost its slot.
///
/// A float pin is converted at this boundary and never inside the VM:
/// `f64 as i32` truncates toward zero and saturates, so an out-of-range pin
/// clamps rather than wrapping. That is ClassicLadder's `%IF`/`%QF` behaviour.
struct Slot {
    name: String,
    pin: Pin,
}

impl Slot {
    fn read_bit(&self) -> Result<bool, Error> {
        let Pin::Bit(pin) = &self.pin else {
            return self.wrong_type("bit", Ok(false));
        };
        let data = pin.bit();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        Ok(unsafe { *data != 0 })
    }

    fn write_bit(&self, value: bool) -> Result<(), Error> {
        let Pin::Bit(pin) = &self.pin else {
            return self.wrong_type("bit", Ok(()));
        };
        let data = pin.bit();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        unsafe { *data = u8::from(value) };
        Ok(())
    }

    fn read_s32(&self) -> Result<i32, Error> {
        let Pin::S32(pin) = &self.pin else {
            return self.wrong_type("s32", Ok(0));
        };
        let data = pin.s32();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        Ok(unsafe { *data })
    }

    fn write_s32(&self, value: i32) -> Result<(), Error> {
        let Pin::S32(pin) = &self.pin else {
            return self.wrong_type("s32", Ok(()));
        };
        let data = pin.s32();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        unsafe { *data = value };
        Ok(())
    }

    fn read_float_as_i32(&self) -> Result<i32, Error> {
        let Pin::Float(pin) = &self.pin else {
            return self.wrong_type("float", Ok(0));
        };
        let data = pin.float();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        Ok(float_to_i32(unsafe { *data }))
    }

    fn write_float_from_i32(&self, value: i32) -> Result<(), Error> {
        let Pin::Float(pin) = &self.pin else {
            return self.wrong_type("float", Ok(()));
        };
        let data = pin.float();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        unsafe { *data = i32_to_float(value) };
        Ok(())
    }

    /// A slot the compiler bound as one HAL type asked for another. `ok` is the
    /// `--no-hal` answer; a real pin of the wrong type is a fault, not a guess.
    fn wrong_type<T>(&self, expected: &'static str, ok: Result<T, Error>) -> Result<T, Error> {
        match self.pin {
            Pin::Off => ok,
            _ => Err(Error::PinType {
                name: self.name.clone(),
                expected,
            }),
        }
    }
}

/// A float pin's integer view: Rust's `as` cast truncates toward zero and
/// saturates, so 2.7 reads as 2, an out-of-range pin clamps rather than wrapping,
/// and NaN reads as 0. This is ClassicLadder's `%IF` conversion, kept because a
/// project migrated from it must land on the same values.
fn float_to_i32(value: f64) -> i32 {
    value as i32
}

/// The inverse at the output boundary: an integer view written back to a float
/// pin, so `%QF0 = 2` drives 2.0.
fn i32_to_float(value: i32) -> f64 {
    f64::from(value)
}

fn off_slots(names: &[String]) -> Vec<Slot> {
    names
        .iter()
        .map(|name| Slot {
            name: name.clone(),
            pin: Pin::Off,
        })
        .collect()
}

fn write_bit(pin: Option<HalPin>, name: &str, value: bool) -> Result<(), Error> {
    let Some(pin) = pin else {
        return Ok(());
    };
    let data = pin.bit();
    if data.is_null() {
        return Err(Error::Unlinked(name.to_string()));
    }
    unsafe { *data = u8::from(value) };
    Ok(())
}

fn write_s32(pin: Option<HalPin>, name: &str, value: i32) -> Result<(), Error> {
    let Some(pin) = pin else {
        return Ok(());
    };
    let data = pin.s32();
    if data.is_null() {
        return Err(Error::Unlinked(name.to_string()));
    }
    unsafe { *data = value };
    Ok(())
}

/// The five control outputs and the heartbeat.
#[derive(Default)]
pub(crate) struct Ctl {
    alive: Option<HalPin>,
    scan_count: Option<HalPin>,
    scan_time_us: Option<HalPin>,
    fault: Option<HalPin>,
    overruns: Option<HalPin>,
}

impl Ctl {
    pub(crate) fn set_alive(&self, value: bool) -> Result<(), Error> {
        write_bit(self.alive, "linc_plc.alive", value)
    }

    pub(crate) fn set_scan_count(&self, value: i32) -> Result<(), Error> {
        write_s32(self.scan_count, "linc_plc.scan-count", value)
    }

    pub(crate) fn set_scan_time_us(&self, value: i32) -> Result<(), Error> {
        write_s32(self.scan_time_us, "linc_plc.scan-time-us", value)
    }

    pub(crate) fn set_overruns(&self, value: i32) -> Result<(), Error> {
        write_s32(self.overruns, "linc_plc.overruns", value)
    }

    pub(crate) fn set_fault(&self, value: bool) -> Result<(), Error> {
        write_bit(self.fault, "linc_plc.fault", value)
    }
}

/// The component, its I/O pins and the control pins. Dropping this unregisters
/// the component (`hal_exit`).
pub(crate) struct Hal {
    /// Owns the component: the pins are pointers into its shared memory, so it
    /// must outlive them even though nothing reads this field.
    _component: Option<HalComponent>,
    /// Bit points, in scan-image order.
    inputs: Vec<Slot>,
    outputs: Vec<Slot>,
    /// Word points, in word-bank order.
    word_inputs: Vec<Slot>,
    word_outputs: Vec<Slot>,
    /// Float points, in word-bank order.
    float_inputs: Vec<Slot>,
    float_outputs: Vec<Slot>,
    ctl: Ctl,
}

impl Hal {
    pub(crate) fn new(config: &Config, map: &IoMap) -> Result<Self, Error> {
        if config.no_hal {
            log::info!("linc-plc: HAL disabled; no component and no pins");
            return Ok(Self {
                _component: None,
                inputs: off_slots(&map.inputs),
                outputs: off_slots(&map.outputs),
                word_inputs: off_slots(&map.word_inputs),
                word_outputs: off_slots(&map.word_outputs),
                float_inputs: off_slots(&map.float_inputs),
                float_outputs: off_slots(&map.float_outputs),
                ctl: Ctl::default(),
            });
        }

        let component = HalComponent::new("linc_plc")?;
        let mut inputs = Vec::with_capacity(map.inputs.len());
        for name in &map.inputs {
            inputs.push(Slot {
                name: name.clone(),
                pin: Pin::Bit(component.bit_in(name)?),
            });
        }
        let mut outputs = Vec::with_capacity(map.outputs.len());
        for name in &map.outputs {
            outputs.push(Slot {
                name: name.clone(),
                pin: Pin::Bit(component.bit_out(name)?),
            });
        }
        let mut word_inputs = Vec::with_capacity(map.word_inputs.len());
        for name in &map.word_inputs {
            word_inputs.push(Slot {
                name: name.clone(),
                pin: Pin::S32(component.s32_in(name)?),
            });
        }
        let mut word_outputs = Vec::with_capacity(map.word_outputs.len());
        for name in &map.word_outputs {
            word_outputs.push(Slot {
                name: name.clone(),
                pin: Pin::S32(component.s32_out(name)?),
            });
        }
        let mut float_inputs = Vec::with_capacity(map.float_inputs.len());
        for name in &map.float_inputs {
            float_inputs.push(Slot {
                name: name.clone(),
                pin: Pin::Float(component.float_in(name)?),
            });
        }
        let mut float_outputs = Vec::with_capacity(map.float_outputs.len());
        for name in &map.float_outputs {
            float_outputs.push(Slot {
                name: name.clone(),
                pin: Pin::Float(component.float_out(name)?),
            });
        }
        let ctl = Ctl {
            alive: Some(component.bit_out("alive")?),
            scan_count: Some(component.s32_out("scan-count")?),
            scan_time_us: Some(component.s32_out("scan-time-us")?),
            fault: Some(component.bit_out("fault")?),
            overruns: Some(component.s32_out("overruns")?),
        };
        component.ready()?;
        log::info!(
            "linc-plc: HAL component linc_plc ready, {} bit input(s), {} bit output(s), \
             {} word pin(s) and {} float pin(s)",
            inputs.len(),
            outputs.len(),
            word_inputs.len() + word_outputs.len(),
            float_inputs.len() + float_outputs.len()
        );
        Ok(Self {
            _component: Some(component),
            inputs,
            outputs,
            word_inputs,
            word_outputs,
            float_inputs,
            float_outputs,
            ctl,
        })
    }

    /// Read every ladder input, in the compiled input order.
    pub(crate) fn read_inputs(&self) -> Result<Vec<bool>, Error> {
        self.inputs
            .iter()
            .map(Slot::read_bit)
            .collect::<Result<Vec<bool>, Error>>()
    }

    /// Read every word input, in word-bank order. The VM writes these into the
    /// bank before the ops pass, so a rung sees the field value this scan.
    pub(crate) fn read_word_inputs(&self) -> Result<Vec<i32>, Error> {
        self.word_inputs
            .iter()
            .map(Slot::read_s32)
            .collect::<Result<Vec<i32>, Error>>()
    }

    /// Read every float input as its integer view. `f64 as i32` truncates toward
    /// zero and saturates, so `%IF0 = 2.7` reads as 2 and an out-of-range pin
    /// clamps rather than wrapping.
    pub(crate) fn read_float_inputs(&self) -> Result<Vec<i32>, Error> {
        self.float_inputs
            .iter()
            .map(Slot::read_float_as_i32)
            .collect::<Result<Vec<i32>, Error>>()
    }

    /// Write every ladder output, in the compiled output order.
    pub(crate) fn write_outputs(&self, values: &[bool]) -> Result<(), Error> {
        if values.len() != self.outputs.len() {
            return Err(Error::Layout {
                expected: self.outputs.len(),
                got: values.len(),
            });
        }
        for (slot, value) in self.outputs.iter().zip(values) {
            slot.write_bit(*value)?;
        }
        Ok(())
    }

    /// Write every word output, in word-bank order.
    pub(crate) fn write_word_outputs(&self, values: &[i32]) -> Result<(), Error> {
        if values.len() != self.word_outputs.len() {
            return Err(Error::Layout {
                expected: self.word_outputs.len(),
                got: values.len(),
            });
        }
        for (slot, value) in self.word_outputs.iter().zip(values) {
            slot.write_s32(*value)?;
        }
        Ok(())
    }

    /// Write every float output, converting the bank's integer view to `f64`.
    pub(crate) fn write_float_outputs(&self, values: &[i32]) -> Result<(), Error> {
        if values.len() != self.float_outputs.len() {
            return Err(Error::Layout {
                expected: self.float_outputs.len(),
                got: values.len(),
            });
        }
        for (slot, value) in self.float_outputs.iter().zip(values) {
            slot.write_float_from_i32(*value)?;
        }
        Ok(())
    }

    /// Drive every ladder output — bit, word and float — to its zero value. Used
    /// by Stop, by a fault, by a Load and on exit. A word output left at its
    /// last value would be a command nobody re-issued.
    pub(crate) fn drive_outputs_false(&self) -> Result<(), Error> {
        for slot in &self.outputs {
            slot.write_bit(false)?;
        }
        for slot in &self.word_outputs {
            slot.write_s32(0)?;
        }
        for slot in &self.float_outputs {
            slot.write_float_from_i32(0)?;
        }
        Ok(())
    }

    pub(crate) fn ctl(&self) -> &Ctl {
        &self.ctl
    }

    /// Last act before the component is unregistered: heartbeat off, outputs
    /// false, fault on. Failures here are logged, not propagated — the daemon is
    /// leaving.
    pub(crate) fn force_safe(&self) {
        if let Err(error) = self.ctl.set_alive(false) {
            log::error!("linc-plc: cannot stop the heartbeat on exit: {error}");
        }
        if let Err(error) = self.drive_outputs_false() {
            log::error!("linc-plc: cannot drive the outputs false on exit: {error}");
        }
        if let Err(error) = self.ctl.set_fault(true) {
            log::error!("linc-plc: cannot raise the fault pin on exit: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{float_to_i32, i32_to_float};

    #[test]
    fn a_float_pin_truncates_toward_zero_on_the_way_in() {
        assert_eq!(float_to_i32(2.7), 2);
        assert_eq!(float_to_i32(-2.7), -2);
        assert_eq!(float_to_i32(2.0), 2);
    }

    #[test]
    fn a_float_pin_saturates_rather_than_wrapping() {
        assert_eq!(float_to_i32(f64::INFINITY), i32::MAX);
        assert_eq!(float_to_i32(f64::NEG_INFINITY), i32::MIN);
        assert_eq!(float_to_i32(1.0e30), i32::MAX);
        assert_eq!(float_to_i32(f64::NAN), 0);
    }

    #[test]
    fn an_integer_view_writes_back_as_a_float() {
        assert_eq!(i32_to_float(2), 2.0);
        assert_eq!(i32_to_float(-7), -7.0);
    }
}
