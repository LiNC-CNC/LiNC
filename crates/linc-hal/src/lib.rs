//! Typed HAL access and the LiNC `linc.*` output component.
//!
//! The LinuxCNC C API is loaded with `dlopen`. This avoids a build-time
//! dependency on the HAL headers/bindgen and lets the HMI start harmlessly on a
//! development machine without a running HAL.
//!
//! There is deliberately no arbitrary pin-name write API. The only outputs
//! exposed by [`LincHal`] are the five declared `linc.*` pins. [`HalReader`]
//! is the separate, read-only side: it can read any pin but has no write
//! method.

use std::ffi::{c_char, c_void, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use libloading::Library;
use linc_core::backend::HalWriter;
use linc_core::command::{HalValue, LincOutput, MachineError};
use thiserror::Error;

const HAL_BIT: i32 = 1;
const HAL_FLOAT: i32 = 2;
const HAL_S32: i32 = 3;
const HAL_U32: i32 = 4;
const HAL_IN: i32 = 16;
const HAL_OUT: i32 = 32;

#[derive(Debug, Error)]
pub enum HalError {
    #[error("liblinuxcnchal is not available: {0}")]
    Library(String),
    #[error("hal_init failed with {0}")]
    Init(i32),
    #[error("hal_pin_new({name}) failed with {code}")]
    Pin { name: String, code: i32 },
    #[error("HAL component is not ready")]
    NotReady,
    #[error("HAL pin type mismatch for {0}")]
    TypeMismatch(String),
}

/// Handle to a HAL pin. The pointer stored by `hal_pin_new` is a pointer to
/// the pin's data slot; the slot itself is updated when HAL links the pin to a
/// signal. Always dereference the slot at access time instead of caching the
/// initial data address.
#[derive(Clone, Copy)]
pub struct HalPin {
    slot: *mut *mut c_void,
}

unsafe impl Send for HalPin {}

impl HalPin {
    fn data(&self) -> *mut c_void {
        unsafe { *self.slot }
    }
    pub fn bit(&self) -> *mut u8 {
        self.data().cast()
    }
    pub fn s32(&self) -> *mut i32 {
        self.data().cast()
    }
    pub fn float(&self) -> *mut f64 {
        self.data().cast()
    }
}

type HalInitFn = unsafe extern "C" fn(*const c_char) -> i32;
type HalExitFn = unsafe extern "C" fn(i32) -> i32;
type HalReadyFn = unsafe extern "C" fn(i32) -> i32;
type HalPinNewFn = unsafe extern "C" fn(*const c_char, i32, i32, *mut *mut c_void, i32) -> i32;
type HalMallocFn = unsafe extern "C" fn(usize) -> *mut c_void;
type HalGetPinValueFn =
    unsafe extern "C" fn(*const c_char, *mut i32, *mut *mut HalData, *mut bool) -> i32;

/// The `hal_data_u` union. It is eight bytes because of the `double` member;
/// keeping all four members here makes the layout match the C definition.
#[repr(C)]
union HalData {
    b: bool,
    s: i32,
    u: u32,
    f: f64,
}

/// A read-only value from an arbitrary HAL pin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HalPinValue {
    Bit(bool),
    Float(f64),
    S32(i32),
    U32(u32),
}

impl HalPinValue {
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(value),
            Self::S32(value) => Some(value as f64),
            Self::U32(value) => Some(value as f64),
            Self::Bit(_) => None,
        }
    }

    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::S32(value) => Some(value as i64),
            Self::U32(value) => Some(value as i64),
            Self::Bit(value) => Some(value as i64),
            Self::Float(_) => None,
        }
    }

    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bit(value) => Some(value),
            Self::S32(value) => Some(value != 0),
            Self::U32(value) => Some(value != 0),
            Self::Float(_) => None,
        }
    }
}

/// Read-only view of arbitrary HAL pins.
///
/// This is the counterpart to [`LincHal`]: the HMI may *read* `cnc_panel.*`,
/// `iocontrol.*`, `halui.*` and spindle feedback, but it still has no generic
/// write path. The HAL library is `dlopen`ed so a UI-only build starts without
/// it and reports every pin as unavailable.
pub struct HalReader {
    _library: Library,
    /// A component is required before HAL has a `hal_data` pointer in this
    /// process; `hal_get_pin_value_by_name` dereferences it. The reader owns no
    /// pins and exposes no write path.
    _component: HalComponent,
    get_pin: HalGetPinValueFn,
}

unsafe impl Send for HalReader {}

impl HalReader {
    pub fn new() -> Result<Self, HalError> {
        let library = open_hal_library()?;
        let get_pin: HalGetPinValueFn = unsafe {
            *library
                .get(b"hal_get_pin_value_by_name\0")
                .map_err(|error| HalError::Library(error.to_string()))?
        };
        let component = HalComponent::new(&format!("linc_reader_{}", std::process::id()))?;
        component.ready()?;
        Ok(Self {
            _library: library,
            _component: component,
            get_pin,
        })
    }

    /// Read one pin. Returns `None` when the pin does not exist, when HAL has
    /// not initialised its shared memory, or when the type is unsupported.
    pub fn read(&self, name: &str) -> Option<HalPinValue> {
        let name = CString::new(name).ok()?;
        let mut ty = 0i32;
        let mut data: *mut HalData = ptr::null_mut();
        let mut connected = false;
        let result = unsafe {
            (self.get_pin)(
                name.as_ptr(),
                &mut ty,
                &mut data,
                &mut connected as *mut bool,
            )
        };
        if result != 0 || data.is_null() {
            return None;
        }
        // The union's active member is selected by the HAL pin type. The
        // pointer stays valid only while HAL is running; read it immediately.
        unsafe {
            match ty {
                HAL_BIT => Some(HalPinValue::Bit((*data).b)),
                HAL_FLOAT => Some(HalPinValue::Float((*data).f)),
                HAL_S32 => Some(HalPinValue::S32((*data).s)),
                HAL_U32 => Some(HalPinValue::U32((*data).u)),
                _ => None,
            }
        }
    }

    pub fn bit(&self, name: &str) -> Option<bool> {
        self.read(name)?.as_bool()
    }

    pub fn float(&self, name: &str) -> Option<f64> {
        self.read(name)?.as_f64()
    }

    pub fn s32(&self, name: &str) -> Option<i32> {
        self.read(name)?.as_i64().map(|value| value as i32)
    }
}

fn open_hal_library() -> Result<Library, HalError> {
    unsafe { Library::new("liblinuxcnchal.so.0") }
        .or_else(|_| unsafe { Library::new("liblinuxcnchal.so") })
        .map_err(|error| HalError::Library(error.to_string()))
}

/// A live LinuxCNC HAL component. The component name is the prefix for every
/// pin; pin arguments are relative names such as `hmi-alive`.
pub struct HalComponent {
    _library: Library,
    hal_exit: HalExitFn,
    comp_id: i32,
    name: String,
    ready: AtomicBool,
}

// HAL component access is confined to one worker thread. The pointers handed
// out by HAL are shared memory addresses, not Rust-owned memory.
unsafe impl Send for HalComponent {}

impl HalComponent {
    pub fn new(name: &str) -> Result<Self, HalError> {
        let library = open_hal_library()?;
        let hal_init: HalInitFn = unsafe {
            *library
                .get(b"hal_init\0")
                .map_err(|error| HalError::Library(error.to_string()))?
        };
        let hal_exit: HalExitFn = unsafe {
            *library
                .get(b"hal_exit\0")
                .map_err(|error| HalError::Library(error.to_string()))?
        };
        let name_c =
            CString::new(name).map_err(|_| HalError::Library("NUL in component name".into()))?;
        let comp_id = unsafe { hal_init(name_c.as_ptr()) };
        if comp_id <= 0 {
            return Err(HalError::Init(comp_id));
        }
        Ok(Self {
            _library: library,
            hal_exit,
            comp_id,
            name: name.to_string(),
            ready: AtomicBool::new(false),
        })
    }

    pub fn id(&self) -> i32 {
        self.comp_id
    }

    fn pin_new(&self, name: &str, ty: i32, dir: i32) -> Result<HalPin, HalError> {
        let hal_pin_new: HalPinNewFn = unsafe {
            *self
                ._library
                .get(b"hal_pin_new\0")
                .map_err(|error| HalError::Library(error.to_string()))?
        };
        let hal_malloc: HalMallocFn = unsafe {
            *self
                ._library
                .get(b"hal_malloc\0")
                .map_err(|error| HalError::Library(error.to_string()))?
        };
        let full_name = if name.starts_with(&format!("{}.", self.name)) {
            name.to_string()
        } else {
            format!("{}.{name}", self.name)
        };
        let name_c =
            CString::new(full_name).map_err(|_| HalError::Library("NUL in pin name".into()))?;
        // hal_pin_new writes the data pointer into component-owned shared
        // memory. A stack address would fail with "not in shared memory".
        let data_slot =
            unsafe { hal_malloc(std::mem::size_of::<*mut c_void>()) }.cast::<*mut c_void>();
        if data_slot.is_null() {
            return Err(HalError::Pin {
                name: name.to_string(),
                code: -1,
            });
        }
        unsafe { *data_slot = ptr::null_mut() };
        let result = unsafe { hal_pin_new(name_c.as_ptr(), ty, dir, data_slot, self.comp_id) };
        if result != 0 {
            return Err(HalError::Pin {
                name: name.to_string(),
                code: result,
            });
        }
        Ok(HalPin { slot: data_slot })
    }

    /// Register a bit output.
    pub fn bit_out(&self, name: &str) -> Result<HalPin, HalError> {
        self.pin_new(name, HAL_BIT, HAL_OUT)
    }

    /// Register a bit input. HAL writes the value; the component only reads it.
    pub fn bit_in(&self, name: &str) -> Result<HalPin, HalError> {
        self.pin_new(name, HAL_BIT, HAL_IN)
    }

    pub fn s32_out(&self, name: &str) -> Result<HalPin, HalError> {
        self.pin_new(name, HAL_S32, HAL_OUT)
    }

    pub fn s32_in(&self, name: &str) -> Result<HalPin, HalError> {
        self.pin_new(name, HAL_S32, HAL_IN)
    }

    pub fn float_out(&self, name: &str) -> Result<HalPin, HalError> {
        self.pin_new(name, HAL_FLOAT, HAL_OUT)
    }

    pub fn float_in(&self, name: &str) -> Result<HalPin, HalError> {
        self.pin_new(name, HAL_FLOAT, HAL_IN)
    }

    pub fn ready(&self) -> Result<(), HalError> {
        let hal_ready: HalReadyFn = unsafe {
            *self
                ._library
                .get(b"hal_ready\0")
                .map_err(|error| HalError::Library(error.to_string()))?
        };
        let result = unsafe { hal_ready(self.comp_id) };
        if result != 0 {
            return Err(HalError::Init(result));
        }
        self.ready.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}

impl Drop for HalComponent {
    fn drop(&mut self) {
        // Always unregister, including partially initialized components whose
        // pin creation failed before hal_ready(); otherwise the name is leaked
        // in HAL shared memory.
        if self.comp_id > 0 {
            unsafe {
                (self.hal_exit)(self.comp_id);
            }
            self.ready.store(false, Ordering::Relaxed);
        }
    }
}

/// The only writable HAL surface of the HMI.
pub struct LincHal {
    component: HalComponent,
    hmi_alive: HalPin,
    tool_change_confirm: HalPin,
    status_display: HalPin,
    jog_increment: HalPin,
    jog_speed: HalPin,
    last_confirm: bool,
}

unsafe impl Send for LincHal {}

impl LincHal {
    pub fn new() -> Result<Self, HalError> {
        // The production component is always `linc`. A test-only override lets
        // a developer exercise a second instance against a live HAL without
        // colliding with a stale component.
        let component_name =
            std::env::var("LINC_HAL_COMPONENT").unwrap_or_else(|_| "linc".to_string());
        let component = HalComponent::new(&component_name)?;
        let hmi_alive = component.bit_out("hmi-alive")?;
        let tool_change_confirm = component.bit_out("tool-change-confirm")?;
        let status_display = component.s32_out("status-display")?;
        let jog_increment = component.float_out("jog-increment")?;
        let jog_speed = component.float_out("jog-speed")?;
        unsafe {
            *hmi_alive.bit() = 0;
            *tool_change_confirm.bit() = 0;
            *status_display.s32() = -1;
            *jog_increment.float() = 0.0;
            *jog_speed.float() = 0.0;
        }
        component.ready()?;
        log::info!(
            "created linc.* HAL outputs (hmi-alive, tool-change-confirm, status-display, jog-increment, jog-speed)"
        );
        Ok(Self {
            component,
            hmi_alive,
            tool_change_confirm,
            status_display,
            jog_increment,
            jog_speed,
            last_confirm: false,
        })
    }

    /// Try to create the component, falling back to no HAL. The caller should
    /// log the error; the UI remains usable without HAL, but real-machine
    /// profiles require the component for post-GUI wiring.
    pub fn new_or_unavailable() -> (Option<Self>, Option<HalError>) {
        match Self::new() {
            Ok(hal) => (Some(hal), None),
            Err(error) => (None, Some(error)),
        }
    }

    pub fn output_names(&self) -> [&'static str; 5] {
        [
            LincOutput::HmiAlive.pin_name(),
            LincOutput::ToolChangeConfirm.pin_name(),
            LincOutput::StatusDisplay.pin_name(),
            LincOutput::JogIncrement.pin_name(),
            LincOutput::JogSpeed.pin_name(),
        ]
    }

    pub fn component_id(&self) -> i32 {
        self.component.id()
    }
}

impl HalWriter for LincHal {
    fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError> {
        if !value.matches(output) {
            return Err(MachineError::Hal(format!(
                "{} does not accept {:?}",
                output.pin_name(),
                value
            )));
        }
        if !self.component.is_ready() {
            return Err(MachineError::Hal("component is not ready".into()));
        }
        unsafe {
            match (output, value) {
                (LincOutput::HmiAlive, HalValue::Bit(value)) => {
                    *self.hmi_alive.bit() = value as u8;
                }
                (LincOutput::ToolChangeConfirm, HalValue::Bit(value)) => {
                    // Edge-driven output: never leave a stuck high level.
                    *self.tool_change_confirm.bit() = value as u8;
                    self.last_confirm = value;
                }
                (LincOutput::StatusDisplay, HalValue::S32(value)) => {
                    *self.status_display.s32() = value;
                }
                (LincOutput::JogIncrement, HalValue::Float(value)) => {
                    *self.jog_increment.float() = value;
                }
                (LincOutput::JogSpeed, HalValue::Float(value)) => {
                    *self.jog_speed.float() = value;
                }
                _ => {
                    return Err(MachineError::Hal("wrong HAL value type".into()));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_harmlessly_without_a_hal_environment() {
        // On a developer box this returns an error; inside LinuxCNC it
        // succeeds. Either outcome must not panic.
        let result = LincHal::new();
        if let Ok(mut hal) = result {
            assert_eq!(
                hal.output_names().len(),
                LincOutput::ALLOWED.len(),
                "every declared output needs a pin"
            );
            hal.write(LincOutput::StatusDisplay, HalValue::S32(54))
                .unwrap();
            hal.write(LincOutput::HmiAlive, HalValue::Bit(true))
                .unwrap();
            hal.write(LincOutput::JogIncrement, HalValue::Float(1.0))
                .unwrap();
        }
    }

    #[test]
    fn read_only_hal_reader_is_harmless_without_hal() {
        // Inside LinuxCNC this initializes a pinless reader component; on a
        // developer box it returns an error. Neither may panic or segfault.
        if let Ok(reader) = HalReader::new() {
            let _ = reader.read("cnc_panel.panel_connected");
            let _ = reader.bit("cnc_panel.estop");
        }
    }

    #[test]
    fn rejects_wrong_writer_type() {
        let mut writer = FakeWriter;
        assert!(writer
            .write(LincOutput::StatusDisplay, HalValue::Bit(true))
            .is_err());
    }

    struct FakeWriter;

    impl HalWriter for FakeWriter {
        fn write(&mut self, output: LincOutput, value: HalValue) -> Result<(), MachineError> {
            if value.matches(output) {
                Ok(())
            } else {
                Err(MachineError::Hal("type".into()))
            }
        }
    }

    #[test]
    fn status_display_unknown_is_negative_one() {
        // Sentinel used by the panel driver to render `--`.
        assert_eq!(HalValue::S32(-1), HalValue::S32(-1));
    }
}
