//! `cnc_panel.*` HAL component for the panel service.
//!
//! All 46 button pins, 56 LED pins, mode/home/proghome outputs, overrides,
//! two tool displays and the status display are created here. The HMI never
//! touches this component.

use linc_core::model::{PanelMode, PanelModeValidity};
use linc_hal::{HalComponent, HalError, HalPin};

use crate::{
    PanelDisplay, PanelOutputs, BUTTON_PINS, HOME_PINS, LED_PINS, MODE_PINS, PROGHOME_PINS,
};

pub struct CncPanelHal {
    component: HalComponent,
    button_pins: Vec<(&'static str, u64, HalPin)>,
    led_pins: Vec<(&'static str, u64, HalPin)>,
    mode_pins: Vec<(&'static str, PanelMode, HalPin)>,
    home_pins: [HalPin; 4],
    proghome_pins: [HalPin; 4],
    step: HalPin,
    feed_override: HalPin,
    rapid_override: HalPin,
    spindle_override: HalPin,
    vel_x: HalPin,
    vel_y: HalPin,
    vel_z: HalPin,
    sseg_magtool: HalPin,
    sseg_spdtool: HalPin,
    sseg_status: HalPin,
    heartbeat: HalPin,
    power_on: HalPin,
    power_off: HalPin,
    home_state_x: HalPin,
    home_state_y: HalPin,
    home_state_z: HalPin,
    communication_error: HalPin,
    panel_connected: HalPin,
}

unsafe impl Send for CncPanelHal {}

impl CncPanelHal {
    pub fn new() -> Result<Self, HalError> {
        let name =
            std::env::var("LINC_PANEL_COMPONENT").unwrap_or_else(|_| "cnc_panel".to_string());
        let component = HalComponent::new(&name)?;
        let mut button_pins = Vec::with_capacity(BUTTON_PINS.len());
        for (name, mask) in BUTTON_PINS {
            let pin = component.bit_out(name)?;
            unsafe { *pin.bit() = 0 };
            button_pins.push((name, mask, pin));
        }
        let mut led_pins = Vec::with_capacity(LED_PINS.len());
        for (name, mask) in LED_PINS {
            let pin = component.bit_in(name)?;
            unsafe { *pin.bit() = 0 };
            led_pins.push((name, mask, pin));
        }
        let mut mode_pins = Vec::with_capacity(MODE_PINS.len());
        for (name, mode) in MODE_PINS {
            let pin = component.bit_out(name)?;
            unsafe { *pin.bit() = 0 };
            mode_pins.push((name, mode, pin));
        }
        let home_pins = [
            component.bit_out(HOME_PINS[0])?,
            component.bit_out(HOME_PINS[1])?,
            component.bit_out(HOME_PINS[2])?,
            component.bit_out(HOME_PINS[3])?,
        ];
        let proghome_pins = [
            component.bit_out(PROGHOME_PINS[0])?,
            component.bit_out(PROGHOME_PINS[1])?,
            component.bit_out(PROGHOME_PINS[2])?,
            component.bit_out(PROGHOME_PINS[3])?,
        ];
        let step = component.bit_out("step")?;
        let feed_override = component.float_out("feed_override")?;
        let rapid_override = component.float_out("rapid_override")?;
        let spindle_override = component.float_out("spindle_override")?;
        let vel_x = component.float_in("vel_x")?;
        let vel_y = component.float_in("vel_y")?;
        let vel_z = component.float_in("vel_z")?;
        let sseg_magtool = component.s32_in("sseg_magtool")?;
        let sseg_spdtool = component.s32_in("sseg_spdtool")?;
        let sseg_status = component.s32_in("sseg_status")?;
        let heartbeat = component.s32_out("heartbeat")?;
        let power_on = component.bit_out("power_on")?;
        let power_off = component.bit_out("power_off")?;
        let home_state_x = component.s32_in("home_state_x")?;
        let home_state_y = component.s32_in("home_state_y")?;
        let home_state_z = component.s32_in("home_state_z")?;
        let communication_error = component.bit_out("communication_error")?;
        let panel_connected = component.bit_out("panel_connected")?;
        unsafe {
            *feed_override.float() = 1.0;
            *rapid_override.float() = 1.0;
            *spindle_override.float() = 1.0;
            *sseg_magtool.s32() = 0;
            *sseg_spdtool.s32() = 0;
            *sseg_status.s32() = -1;
            *heartbeat.s32() = 0;
            *power_on.bit() = 0;
            *power_off.bit() = 0;
            *home_state_x.s32() = 0;
            *home_state_y.s32() = 0;
            *home_state_z.s32() = 0;
            *communication_error.bit() = 1;
            *panel_connected.bit() = 0;
        }
        component.ready()?;
        log::info!("created cnc_panel.* HAL surface (46 buttons, 56 LEDs, modes, displays)");
        Ok(Self {
            component,
            button_pins,
            led_pins,
            mode_pins,
            home_pins,
            proghome_pins,
            step,
            feed_override,
            rapid_override,
            spindle_override,
            vel_x,
            vel_y,
            vel_z,
            sseg_magtool,
            sseg_spdtool,
            sseg_status,
            heartbeat,
            power_on,
            power_off,
            home_state_x,
            home_state_y,
            home_state_z,
            communication_error,
            panel_connected,
        })
    }

    pub fn component_id(&self) -> i32 {
        self.component.id()
    }

    /// Publish the decoded panel state to HAL.
    pub fn apply_outputs(&mut self, outputs: &PanelOutputs) {
        unsafe {
            for (_name, mask, pin) in &self.button_pins {
                let pressed = match *mask {
                    crate::buttons::PROGRAM_PROTECT => outputs.program_protect,
                    crate::buttons::ESTOP => outputs.estop,
                    _ => outputs.buttons & *mask != 0,
                };
                *pin.bit() = pressed as u8;
            }
            for (_name, mode, pin) in &self.mode_pins {
                let active =
                    matches!(&outputs.mode, PanelModeValidity::Valid(active) if active == mode);
                *pin.bit() = active as u8;
            }
            for (index, pin) in self.home_pins.iter().enumerate() {
                *pin.bit() = outputs.home[index] as u8;
            }
            for (index, pin) in self.proghome_pins.iter().enumerate() {
                *pin.bit() = outputs.proghome[index] as u8;
            }
            *self.step.bit() = outputs.step as u8;
            *self.feed_override.float() = outputs.feed_override;
            *self.rapid_override.float() = outputs.rapid_override;
            *self.spindle_override.float() = outputs.spindle_override;
            *self.communication_error.bit() = 0;
            *self.power_on.bit() = outputs.power_on_pulse as u8;
            *self.power_off.bit() = outputs.power_off_pulse as u8;
            *self.heartbeat.s32() = (*self.heartbeat.s32()).wrapping_add(1);
        }
    }

    pub fn set_connected(&mut self, connected: bool, error: bool) {
        unsafe {
            *self.panel_connected.bit() = connected as u8;
            *self.communication_error.bit() = error as u8;
        }
    }

    /// Read the LED, 7-segment and velocity inputs written by HAL.
    pub fn read_display(&self) -> PanelDisplay {
        let mut leds = 0u64;
        for (_name, mask, pin) in &self.led_pins {
            if unsafe { *pin.bit() != 0 } {
                leds |= *mask;
            }
        }
        PanelDisplay {
            leds,
            sseg_tool: unsafe { *self.sseg_magtool.s32() },
            sseg_next_tool: unsafe { *self.sseg_spdtool.s32() },
            sseg_status: unsafe { *self.sseg_status.s32() },
            vel: [
                unsafe { *self.vel_x.float() },
                unsafe { *self.vel_y.float() },
                unsafe { *self.vel_z.float() },
            ],
            homing: [&self.home_state_x, &self.home_state_y, &self.home_state_z]
                .iter()
                .any(|state| {
                    let state = unsafe { *state.s32() };
                    (1..=4).contains(&state)
                }),
        }
    }
}

/// Validate the mode bitmap before writing it to HAL. Used by tests.
pub fn mode_bitmap(mode: PanelModeValidity) -> u8 {
    match mode {
        PanelModeValidity::Valid(mode) => match mode {
            PanelMode::Edit => 1 << 0,
            PanelMode::Auto => 1 << 1,
            PanelMode::Mdi => 1 << 2,
            PanelMode::Jog => 1 << 3,
            PanelMode::Inc => 1 << 4,
            PanelMode::Mpg => 1 << 5,
            PanelMode::Home => 1 << 6,
            PanelMode::ProgHome => 1 << 7,
        },
        PanelModeValidity::Invalid(_) | PanelModeValidity::Unknown => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_bitmap_is_one_hot_or_zero() {
        assert_eq!(mode_bitmap(PanelModeValidity::Valid(PanelMode::Auto)), 0b10);
        assert_eq!(mode_bitmap(PanelModeValidity::Unknown), 0);
        assert_eq!(
            mode_bitmap(PanelModeValidity::Invalid(vec![
                PanelMode::Auto,
                PanelMode::Mdi
            ])),
            0
        );
    }
}
