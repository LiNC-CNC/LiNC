//! The `linc_plc.*` HAL surface: pin creation and the only legal access to it.
//!
//! Only the daemon's main thread touches these values. Pins are created from
//! the project's I/O bindings — the map is frozen at startup — plus the five
//! control outputs, all before `hal_ready()`.

use linc_hal::{HalComponent, HalPin};

use crate::{Config, Error, IoMap};

/// One project I/O binding and the pin it was created for. `pin` is `None` in
/// `--no-hal` mode, where the ladder still scans but nothing is wired.
struct Slot {
    name: String,
    pin: Option<HalPin>,
}

impl Slot {
    fn read_bit(&self) -> Result<bool, Error> {
        let Some(pin) = self.pin else {
            return Ok(false);
        };
        let data = pin.bit();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        Ok(unsafe { *data != 0 })
    }

    fn write_bit(&self, value: bool) -> Result<(), Error> {
        let Some(pin) = self.pin else {
            return Ok(());
        };
        let data = pin.bit();
        if data.is_null() {
            return Err(Error::Unlinked(self.name.clone()));
        }
        unsafe { *data = u8::from(value) };
        Ok(())
    }
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
    inputs: Vec<Slot>,
    outputs: Vec<Slot>,
    ctl: Ctl,
}

impl Hal {
    pub(crate) fn new(config: &Config, map: &IoMap) -> Result<Self, Error> {
        if config.no_hal {
            log::info!("linc-plc: HAL disabled; no component and no pins");
            return Ok(Self {
                _component: None,
                inputs: map
                    .inputs
                    .iter()
                    .map(|name| Slot {
                        name: name.clone(),
                        pin: None,
                    })
                    .collect(),
                outputs: map
                    .outputs
                    .iter()
                    .map(|name| Slot {
                        name: name.clone(),
                        pin: None,
                    })
                    .collect(),
                ctl: Ctl::default(),
            });
        }

        let component = HalComponent::new("linc_plc")?;
        let mut inputs = Vec::with_capacity(map.inputs.len());
        for name in &map.inputs {
            inputs.push(Slot {
                name: name.clone(),
                pin: Some(component.bit_in(name)?),
            });
        }
        let mut outputs = Vec::with_capacity(map.outputs.len());
        for name in &map.outputs {
            outputs.push(Slot {
                name: name.clone(),
                pin: Some(component.bit_out(name)?),
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
            "linc-plc: HAL component linc_plc ready, {} input(s) and {} output(s)",
            inputs.len(),
            outputs.len()
        );
        Ok(Self {
            _component: Some(component),
            inputs,
            outputs,
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

    /// Drive every ladder output false. Used by Stop, by a fault and on exit.
    pub(crate) fn drive_outputs_false(&self) -> Result<(), Error> {
        for slot in &self.outputs {
            slot.write_bit(false)?;
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
