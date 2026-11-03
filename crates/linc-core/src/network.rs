//! Optional supervision of the machine's internet link.
//!
//! The link this module manages is the way the machine reaches the internet:
//! a WiFi radio, or a second NIC (PCIe or USB). It is **never** the machine's
//! own fieldbus and I/O network, which is internal and must keep running. That
//! is why every default here acts on one named device and why validation
//! rejects anything that reaches for system-wide networking
//! (`crate::config::validate_network`).
//!
//! # Why measure first
//!
//! WiFi traffic costs LinuxCNC latency: beacons, scans and the driver's own
//! interrupt work land in the same machine as the realtime thread. Dropping the
//! link before motion is the intended fix, but the numbers that make it work —
//! how long `nmcli` takes, how long the radio keeps working after it returns,
//! and how much interrupt load actually stops — are properties of the driver
//! and the hardware, not of the profile.
//!
//! So this pass measures instead of gating. `[network] enabled = true` runs one
//! down/verify/settle/up cycle on an idle machine and publishes the phases plus
//! the device's interrupt and packet rates in [`NetworkStatus`]. Nothing here
//! refuses a command or blocks motion: the operator-facing promise of this
//! pass is "it tells you what a toggle costs", and a later pass can arm the
//! gate with `settle_ms` chosen from evidence.
//!
//! The cycle is a worker thread, like the Loobric client, so neither the UI nor
//! the NML loop waits on `nmcli`.

use std::fs;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::MachineBackend;
use crate::command::{MachineCommand, MachineError};
use crate::config::{NetworkConfig, Profile};
use crate::model::{now_ms, InterpState, MachineSnapshot, NetworkStatus};

/// How often the worker re-reads the observation before it starts measuring.
const TICK: Duration = Duration::from_millis(100);

/// Deadline for one `nmcli` invocation. A hung command must not hold the
/// measurement open forever; the cycle still restores the link afterwards.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long each interrupt/packet sample counts for. Test overrides shrink it.
const DEFAULT_SAMPLE: Duration = Duration::from_millis(1000);

/// The two kinds of external link the section knows. The kind only selects the
/// default commands and the state parser; policy is identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Wifi,
    Ethernet,
}

impl LinkKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "wifi" => Some(Self::Wifi),
            "ethernet" => Some(Self::Ethernet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
        }
    }
}

/// Resolved settings for one measurement. `from_profile` returns `None` for a
/// disabled profile, so a machine that does not opt in starts no worker and
/// publishes no status.
#[derive(Clone, Debug)]
pub struct NetworkSettings {
    pub kind: LinkKind,
    pub interface: String,
    /// Each step is an argv vector; steps run in order. No shell is involved,
    /// so an override cannot chain a second command.
    pub down: Vec<Vec<String>>,
    pub up: Vec<Vec<String>>,
    pub state: Vec<String>,
    pub settle: Duration,
    pub verify_timeout: Duration,
    pub measure_on_startup: bool,
    pub keep_up: bool,
    pub irq_match: String,
    pub sample: Duration,
    /// The profile expects a Loobric run at startup, so the measurement waits
    /// for it: pulling the link down under a running client is the one failure
    /// worth avoiding even in a measurement pass.
    pub expect_loobric_startup: bool,
}

impl NetworkSettings {
    pub fn from_profile(profile: &Profile) -> Option<Self> {
        let config = &profile.config.network;
        if !config.enabled {
            return None;
        }
        Some(Self::from_config(
            config,
            profile.config.loobric.enabled && profile.config.loobric.sync_on_startup,
        ))
    }

    fn from_config(config: &NetworkConfig, expect_loobric_startup: bool) -> Self {
        // Validation already rejected an unknown kind; falling back keeps a
        // hand-built config from panicking.
        let kind = LinkKind::parse(&config.kind).unwrap_or(LinkKind::Wifi);
        let interface = config.interface.trim().to_string();
        let down = match &config.down_cmd {
            Some(command) => vec![split_command(command)],
            None => default_down(kind, &interface),
        };
        let up = match &config.up_cmd {
            Some(command) => vec![split_command(command)],
            None => default_up(kind, &interface, config.connection.as_deref()),
        };
        let state = match &config.state_cmd {
            Some(command) => split_command(command),
            None => default_state(kind, &interface),
        };
        Self {
            kind,
            interface: interface.clone(),
            down,
            up,
            state,
            settle: Duration::from_millis(config.settle_ms),
            verify_timeout: Duration::from_millis(config.verify_timeout_ms),
            measure_on_startup: config.measure_on_startup,
            keep_up: config.keep_up,
            irq_match: config
                .irq_match
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&interface)
                .to_string(),
            sample: DEFAULT_SAMPLE,
            expect_loobric_startup,
        }
    }
}

/// `nmcli radio wifi off` acts on the radio, not on system networking, so the
/// machine's fieldbus link keeps running.
fn default_down(kind: LinkKind, interface: &str) -> Vec<Vec<String>> {
    match kind {
        LinkKind::Wifi => vec![argv(&["nmcli", "radio", "wifi", "off"])],
        LinkKind::Ethernet => vec![argv(&["nmcli", "device", "disconnect", interface])],
    }
}

fn default_up(kind: LinkKind, interface: &str, connection: Option<&str>) -> Vec<Vec<String>> {
    match kind {
        LinkKind::Wifi => {
            let mut steps = vec![argv(&["nmcli", "radio", "wifi", "on"])];
            if let Some(connection) = connection.map(str::trim).filter(|value| !value.is_empty()) {
                steps.push(argv(&["nmcli", "connection", "up", connection]));
            }
            steps
        }
        LinkKind::Ethernet => vec![argv(&["nmcli", "device", "connect", interface])],
    }
}

fn default_state(kind: LinkKind, interface: &str) -> Vec<String> {
    match kind {
        // `enabled` / `disabled`: the radio's own state, which is what the
        // latency question is about.
        LinkKind::Wifi => argv(&["nmcli", "--terse", "--fields", "WIFI", "radio"]),
        // Carrier state. Route removal would leave the PHY and the driver's
        // interrupt work running.
        LinkKind::Ethernet => argv(&["cat", &format!("/sys/class/net/{interface}/operstate")]),
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

fn split_command(command: &str) -> Vec<String> {
    command.split_whitespace().map(str::to_string).collect()
}

/// One completed measurement cycle.
#[derive(Clone, Debug, Default)]
struct Measurement {
    down: Duration,
    /// `None` when the link never read down before the deadline.
    verify: Option<Duration>,
    settle: Duration,
    up: Duration,
    up_restored: bool,
    link_down: bool,
    irq_up: Option<f64>,
    irq_down: Option<f64>,
    rx_up: Option<f64>,
    rx_down: Option<f64>,
    notes: Vec<String>,
}

impl Measurement {
    fn summary(&self) -> String {
        if !self.link_down {
            return format!("link never read down after {}ms", self.down.as_millis());
        }
        let mut text = format!(
            "down {}ms, verify {}ms, settle {}ms, up {}ms",
            self.down.as_millis(),
            self.verify
                .map(|value| value.as_millis().to_string())
                .unwrap_or_else(|| ">deadline".into()),
            self.settle.as_millis(),
            self.up.as_millis(),
        );
        match (self.irq_up, self.irq_down) {
            (Some(up), Some(down)) => {
                text.push_str(&format!(", irq {up:.0}/s -> {down:.0}/s"));
            }
            _ => text.push_str(", irq not sampled"),
        }
        if !self.up_restored {
            text.push_str("; link did not come back");
        }
        text
    }
}

/// Record a note once. The verify polls repeat the same failure every 25 ms.
fn note(result: &mut Measurement, message: String) {
    if !result.notes.iter().any(|existing| existing == &message) {
        result.notes.push(message);
    }
}

/// `Ok(state)` from the profile's state command, or `Err` describing why the
/// state could not be read. `None` means the output was not a state word.
fn probe_link(settings: &NetworkSettings) -> Result<bool, String> {
    let output = run_capture(&settings.state, COMMAND_TIMEOUT)?;
    parse_state(&output).ok_or_else(|| format!("unrecognised link state: {:?}", shorten(&output)))
}

fn parse_state(output: &str) -> Option<bool> {
    let value = output
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?
        .to_ascii_lowercase();
    // Drop an `nmcli` field prefix such as `WIFI:` when the terse format is not
    // honoured by an older version.
    let value = value
        .rsplit(':')
        .next()
        .unwrap_or(&value)
        .trim()
        .to_string();
    match value.as_str() {
        "enabled" | "on" | "up" | "1" | "true" => Some(true),
        "disabled" | "off" | "down" | "lowerlayerdown" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// Run one down/verify/settle/up cycle. Always tries to leave the link up: the
/// measurement is diagnostic, so the machine must not be left disconnected
/// because a phase misbehaved.
fn measure(settings: &NetworkSettings) -> Measurement {
    let mut result = Measurement::default();

    let (irq_up, rx_up) = sample_rates(settings);
    result.irq_up = irq_up;
    result.rx_up = rx_up;
    if irq_up.is_none() {
        log::warn!(
            "network: no /proc/interrupts line matched {:?}; set network.irq_match \
             to the driver name (for example iwlwifi)",
            settings.irq_match
        );
    }

    match run_steps(&settings.down) {
        Ok(elapsed) => result.down = elapsed,
        Err(error) => note(&mut result, format!("down command failed: {error}")),
    }

    let started = Instant::now();
    let deadline = started + settings.verify_timeout;
    let mut link_down = false;
    loop {
        match probe_link(settings) {
            Ok(false) => {
                link_down = true;
                break;
            }
            Ok(true) => {}
            // The poll runs every 25 ms for up to the verify deadline. One
            // note per distinct failure, not one per poll.
            Err(error) => note(&mut result, error),
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    result.link_down = link_down;
    if link_down {
        result.verify = Some(started.elapsed());
    }

    // The radio's own teardown outlives `nmcli` returning; sample quiet only
    // after the configured window.
    thread::sleep(settings.settle);
    result.settle = settings.settle;
    let (irq_down, rx_down) = sample_rates(settings);
    result.irq_down = irq_down;
    result.rx_down = rx_down;

    match run_steps(&settings.up) {
        Ok(elapsed) => result.up = elapsed,
        Err(error) => note(&mut result, format!("up command failed: {error}")),
    }

    let started = Instant::now();
    let deadline = started + settings.verify_timeout;
    loop {
        match probe_link(settings) {
            Ok(true) => {
                result.up_restored = true;
                break;
            }
            Ok(false) => {}
            Err(error) => note(&mut result, error),
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    if !result.up_restored {
        // Do not sleep on the failure: it is the operator's link.
        log::error!(
            "network: {} {} did not come back within {}ms",
            settings.kind.as_str(),
            settings.interface,
            settings.verify_timeout.as_millis()
        );
    }
    result
}

/// A pair of monotonically increasing counters: interrupts, received packets.
type Counters = (Option<u64>, Option<u64>);

/// Sample the device for one window and return `(interrupts/s, packets/s)`.
/// Both are `None` when the counter cannot be read: a missing sample is not
/// zero, and reporting it as zero would fake the result this pass exists to
/// produce.
fn sample_rates(settings: &NetworkSettings) -> (Option<f64>, Option<f64>) {
    let before = counters(settings);
    thread::sleep(settings.sample);
    let after = counters(settings);
    (
        rate(before.0, after.0, settings.sample),
        rate(before.1, after.1, settings.sample),
    )
}

fn rate(before: Option<u64>, after: Option<u64>, window: Duration) -> Option<f64> {
    let (before, after) = (before?, after?);
    let seconds = window.as_secs_f64();
    if seconds <= 0.0 || after < before {
        return None;
    }
    Some((after - before) as f64 / seconds)
}

fn counters(settings: &NetworkSettings) -> Counters {
    (
        interrupts(&settings.irq_match),
        rx_packets(&settings.interface),
    )
}

/// Sum the counters on every `/proc/interrupts` line that mentions the match.
/// WiFi IRQ lines usually carry the driver name (`iwlwifi`), not the interface,
/// which is why `irq_match` exists as its own key.
fn interrupts(match_text: &str) -> Option<u64> {
    if match_text.trim().is_empty() {
        return None;
    }
    let text = fs::read_to_string("/proc/interrupts").ok()?;
    let mut total = 0u64;
    let mut matched = false;
    for line in text.lines() {
        if !line.contains(match_text) {
            continue;
        }
        matched = true;
        // The leading token is the IRQ number with a colon; every other numeric
        // token is a per-CPU count.
        for token in line.split_whitespace().skip(1) {
            if let Ok(count) = token.parse::<u64>() {
                total = total.saturating_add(count);
            }
        }
    }
    matched.then_some(total)
}

fn rx_packets(interface: &str) -> Option<u64> {
    let text =
        fs::read_to_string(format!("/sys/class/net/{interface}/statistics/rx_packets")).ok()?;
    text.trim().parse().ok()
}

/// The device that carries the default route: the internet path the profile is
/// claiming to control. A mismatch means taking the configured interface down
/// would not stop internet traffic, so the measurement would understate the
/// cost.
fn default_route_device() -> Option<String> {
    let text = fs::read_to_string("/proc/net/route").ok()?;
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let device = fields.next()?;
        let destination = fields.next()?;
        if destination == "00000000" {
            return Some(device.to_string());
        }
    }
    None
}

fn run_steps(steps: &[Vec<String>]) -> Result<Duration, String> {
    let started = Instant::now();
    for step in steps {
        wait_for_command(step, COMMAND_TIMEOUT)?;
    }
    Ok(started.elapsed())
}

/// Run one argv to completion, discarding its output. A non-zero exit is an
/// error: on a kiosk box `nmcli` fails when polkit denies the request, and that
/// must be visible rather than silently unmeasured.
fn wait_for_command(argv: &[String], timeout: Duration) -> Result<(), String> {
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    match wait_for_exit(&mut child, timeout).map_err(|error| error.to_string())? {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(format!("{} exited with {status}", label(argv))),
        None => Err(format!("{} timed out", label(argv))),
    }
}

/// Run one argv and return its combined output. Used for the state command,
/// whose answer is the whole point.
fn run_capture(argv: &[String], timeout: Duration) -> Result<String, String> {
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    let stdout = child.stdout.take().map(read_pipe);
    let stderr = child.stderr.take().map(read_pipe);
    let status = wait_for_exit(&mut child, timeout).map_err(|error| error.to_string())?;
    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    match status {
        Some(status) if status.success() => Ok(if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        }),
        Some(status) => Err(format!(
            "{} exited with {status}: {}",
            label(argv),
            shorten(stderr.trim())
        )),
        None => Err(format!("{} timed out", label(argv))),
    }
}

fn read_pipe(mut pipe: impl std::io::Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    })
}

/// `Ok(Some(status))` when the command exited, `Ok(None)` on timeout.
fn wait_for_exit(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn label(argv: &[String]) -> String {
    argv.join(" ")
}

fn shorten(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let mut shortened: String = text.chars().take(MAX - 3).collect();
    shortened.push_str("...");
    shortened
}

/// What the backend wrapper tells the worker each frame: the two facts that
/// decide whether a measurement may start.
#[derive(Clone, Copy, Debug, Default)]
struct Observation {
    idle: bool,
    loobric_running: bool,
    loobric_started: bool,
}

struct SupervisorHandle {
    status: Arc<Mutex<NetworkStatus>>,
    observation: Arc<Mutex<Observation>>,
}

impl SupervisorHandle {
    fn start(settings: NetworkSettings) -> Self {
        let status = Arc::new(Mutex::new(NetworkStatus {
            enabled: true,
            kind: settings.kind.as_str().to_string(),
            interface: settings.interface.clone(),
            message: "not measured yet".into(),
            ..NetworkStatus::default()
        }));
        let observation = Arc::new(Mutex::new(Observation::default()));
        let worker_status = Arc::clone(&status);
        let worker_observation = Arc::clone(&observation);
        thread::Builder::new()
            .name("linc-network".into())
            .spawn(move || {
                run_worker(settings, &worker_status, &worker_observation);
            })
            .expect("spawn network worker");
        Self {
            status,
            observation,
        }
    }

    fn observe(&self, snapshot: &MachineSnapshot) {
        let idle = snapshot.task.interp == InterpState::Idle && !snapshot.task.running;
        let (running, started) = match &snapshot.loobric {
            Some(loobric) => (loobric.running, loobric.completed_runs >= 1),
            None => (false, true),
        };
        *lock(&self.observation) = Observation {
            idle,
            loobric_running: running,
            loobric_started: started,
        };
    }

    fn status(&self) -> NetworkStatus {
        lock(&self.status).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn run_worker(
    settings: NetworkSettings,
    status: &Arc<Mutex<NetworkStatus>>,
    observation: &Arc<Mutex<Observation>>,
) {
    let route = default_route_device();
    let mismatch = route
        .as_deref()
        .is_some_and(|device| device != settings.interface);
    {
        let mut status = lock(status);
        status.route_mismatch = mismatch;
    }
    if mismatch {
        log::warn!(
            "network: interface {} is not the default-route device {}; \
             taking it down will not stop internet traffic",
            settings.interface,
            route.unwrap_or_default()
        );
    }

    match probe_link(&settings) {
        Ok(up) => {
            log::info!(
                "network: {} {} link is {}",
                settings.kind.as_str(),
                settings.interface,
                if up { "up" } else { "down" }
            );
            lock(status).link_up = Some(up);
        }
        Err(error) => {
            log::warn!("network: cannot read the link state: {error}");
            lock(status).message = error;
        }
    }

    if settings.keep_up {
        lock(status).message = "kept up (network.keep_up)".into();
        log::info!("network: keep_up set, leaving the link alone");
        return;
    }
    if !settings.measure_on_startup {
        lock(status).message = "measurement disabled".into();
        return;
    }

    loop {
        let observation = *lock(observation);
        let loobric_ready = !observation.loobric_running
            && (observation.loobric_started || !settings.expect_loobric_startup);
        if observation.idle && loobric_ready {
            break;
        }
        thread::sleep(TICK);
    }

    lock(status).measuring = true;
    lock(status).last_measure_ms = Some(now_ms());
    let measurement = measure(&settings);

    let summary = measurement.summary();
    {
        let mut status = lock(status);
        status.measuring = false;
        status.down_ms = Some(measurement.down.as_millis() as u64);
        status.verify_ms = measurement.verify.map(|value| value.as_millis() as u64);
        status.settle_ms = Some(measurement.settle.as_millis() as u64);
        status.up_ms = Some(measurement.up.as_millis() as u64);
        status.irq_per_s_up = measurement.irq_up;
        status.irq_per_s_down = measurement.irq_down;
        status.rx_per_s_up = measurement.rx_up;
        status.rx_per_s_down = measurement.rx_down;
        status.message = summary.clone();
    }
    for note in &measurement.notes {
        log::warn!("network: {note}");
    }
    log::info!(
        "network: measured {} {}: {summary}",
        settings.kind.as_str(),
        settings.interface
    );
}

/// Wrap the selected backend when `[network] enabled` is true. A disabled
/// profile returns the backend untouched: no worker, no status, no measurement.
pub fn wrap_backend(
    backend: Box<dyn MachineBackend>,
    profile: &Profile,
) -> Box<dyn MachineBackend> {
    let Some(settings) = NetworkSettings::from_profile(profile) else {
        return backend;
    };
    Box::new(NetworkBackend::new(backend, settings))
}

/// Backend wrapper that publishes [`NetworkStatus`]. It is deliberately a
/// pass-through: no command is inspected, refused or delayed.
pub struct NetworkBackend<B> {
    inner: B,
    supervisor: SupervisorHandle,
}

impl<B: MachineBackend> NetworkBackend<B> {
    pub fn new(inner: B, settings: NetworkSettings) -> Self {
        Self {
            inner,
            supervisor: SupervisorHandle::start(settings),
        }
    }
}

impl<B: MachineBackend> MachineBackend for NetworkBackend<B> {
    fn snapshot(&mut self) -> MachineSnapshot {
        let mut snapshot = self.inner.snapshot();
        self.supervisor.observe(&snapshot);
        snapshot.network = Some(self.supervisor.status());
        snapshot
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        self.inner.send(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use crate::model::{MachineVariant, TaskState};

    fn script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path.display().to_string()
    }

    /// A fake link whose state lives in a file: `state.sh` prints it,
    /// `down.sh`/`up.sh` write it. The defaults under test are replaced, so the
    /// suite never touches the real network.
    fn fixture(dir: &Path) -> (NetworkSettings, std::path::PathBuf) {
        let state_file = dir.join("state.txt");
        fs::write(&state_file, "up\n").unwrap();
        let state = script(dir, "state.sh", &format!("cat {}", state_file.display()));
        let down = script(
            dir,
            "down.sh",
            &format!("printf 'down\\n' > {}", state_file.display()),
        );
        let up = script(
            dir,
            "up.sh",
            &format!("printf 'up\\n' > {}", state_file.display()),
        );
        let settings = NetworkSettings {
            kind: LinkKind::Ethernet,
            interface: "test0".into(),
            down: vec![vec![down]],
            up: vec![vec![up]],
            state: vec![state],
            settle: Duration::from_millis(10),
            verify_timeout: Duration::from_millis(500),
            measure_on_startup: true,
            keep_up: false,
            irq_match: "test0".into(),
            sample: Duration::from_millis(10),
            expect_loobric_startup: false,
        };
        (settings, state_file)
    }

    #[test]
    fn defaults_act_on_one_device_per_kind() {
        let wifi = NetworkSettings::from_config(
            &NetworkConfig {
                enabled: true,
                kind: "wifi".into(),
                interface: "wlp0s20f3".into(),
                connection: Some("OpenWrt".into()),
                ..NetworkConfig::default()
            },
            false,
        );
        assert_eq!(
            wifi.down,
            vec![argv(&["nmcli", "radio", "wifi", "off"])],
            "wifi must switch the radio, never system networking"
        );
        assert_eq!(
            wifi.up,
            vec![
                argv(&["nmcli", "radio", "wifi", "on"]),
                argv(&["nmcli", "connection", "up", "OpenWrt"]),
            ]
        );

        let ethernet = NetworkSettings::from_config(
            &NetworkConfig {
                enabled: true,
                kind: "ethernet".into(),
                interface: "usb0".into(),
                ..NetworkConfig::default()
            },
            false,
        );
        assert_eq!(
            ethernet.down,
            vec![argv(&["nmcli", "device", "disconnect", "usb0"])]
        );
        assert_eq!(
            ethernet.state,
            argv(&["cat", "/sys/class/net/usb0/operstate"]),
            "carrier state, not a route deletion"
        );
    }

    #[test]
    fn measurement_records_every_phase_and_leaves_the_link_up() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, state_file) = fixture(dir.path());
        let measurement = measure(&settings);
        assert!(measurement.link_down, "{measurement:?}");
        assert!(measurement.verify.is_some(), "{measurement:?}");
        assert_eq!(measurement.settle, Duration::from_millis(10));
        assert!(measurement.up_restored, "{measurement:?}");
        assert!(measurement.notes.is_empty(), "{measurement:?}");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
        // The fake interrupts match nothing, so the rates are honestly absent
        // rather than zero.
        assert!(measurement.irq_up.is_none(), "{measurement:?}");
    }

    #[test]
    fn a_link_that_never_drops_is_reported_and_still_restored() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, state_file) = fixture(dir.path());
        // A down command that does nothing: the state never changes.
        let noop = script(dir.path(), "noop.sh", "exit 0");
        settings.down = vec![vec![noop]];
        settings.verify_timeout = Duration::from_millis(100);
        let measurement = measure(&settings);
        assert!(!measurement.link_down, "{measurement:?}");
        assert!(measurement.verify.is_none(), "{measurement:?}");
        assert!(measurement.up_restored, "{measurement:?}");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
        assert!(measurement.summary().contains("never read down"));
    }

    #[test]
    fn a_failing_down_command_is_a_note_not_a_silent_pass() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, _) = fixture(dir.path());
        let failing = script(dir.path(), "fail.sh", "exit 1");
        settings.down = vec![vec![failing]];
        let measurement = measure(&settings);
        assert!(
            measurement
                .notes
                .iter()
                .any(|note| note.contains("down command failed")),
            "{measurement:?}"
        );
    }

    #[test]
    fn keep_up_starts_no_measurement() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, state_file) = fixture(dir.path());
        settings.keep_up = true;
        let supervisor = SupervisorHandle::start(settings);
        // The worker probes the state and returns without toggling.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if supervisor.status().message.contains("keep_up") {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let status = supervisor.status();
        assert!(status.message.contains("keep_up"), "{status:?}");
        assert_eq!(status.link_up, Some(true), "{status:?}");
        assert!(status.last_measure_ms.is_none(), "{status:?}");
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");
    }

    #[test]
    fn the_wrapper_measures_only_when_idle_and_after_loobric() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, state_file) = fixture(dir.path());
        settings.expect_loobric_startup = true;
        let supervisor = SupervisorHandle::start(settings);
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.task = TaskState::default();
        snapshot.task.interp = InterpState::Idle;
        // A sync is still to come: the measurement must not pull the link down
        // under it.
        snapshot.loobric = Some(crate::model::LoobricStatus {
            enabled: true,
            ..Default::default()
        });
        supervisor.observe(&snapshot);
        thread::sleep(Duration::from_millis(300));
        assert!(supervisor.status().last_measure_ms.is_none());
        assert_eq!(fs::read_to_string(&state_file).unwrap().trim(), "up");

        snapshot.loobric = Some(crate::model::LoobricStatus {
            enabled: true,
            completed_runs: 1,
            ..Default::default()
        });
        supervisor.observe(&snapshot);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if supervisor.status().down_ms.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let status = supervisor.status();
        assert!(status.last_measure_ms.is_some(), "{status:?}");
        assert!(status.down_ms.is_some(), "{status:?}");
        assert!(status.up_ms.is_some(), "{status:?}");
    }

    struct FakeBackend {
        sends: Arc<Mutex<u64>>,
        snapshot: MachineSnapshot,
    }

    impl MachineBackend for FakeBackend {
        fn snapshot(&mut self) -> MachineSnapshot {
            self.snapshot.clone()
        }

        fn send(&mut self, _command: MachineCommand) -> Result<(), MachineError> {
            *lock(&self.sends) += 1;
            Ok(())
        }
    }

    #[test]
    fn the_wrapper_never_blocks_a_command_and_publishes_status() {
        let dir = tempfile::tempdir().unwrap();
        let (mut settings, _) = fixture(dir.path());
        settings.keep_up = true;
        let sends = Arc::new(Mutex::new(0));
        let backend = FakeBackend {
            sends: Arc::clone(&sends),
            snapshot: MachineSnapshot::empty(MachineVariant::Mill),
        };
        let mut backend = NetworkBackend::new(backend, settings);
        let snapshot = backend.snapshot();
        assert!(snapshot.network.is_some(), "status must be published");
        backend.send(MachineCommand::ReloadToolTable).unwrap();
        assert_eq!(*lock(&sends), 1, "the command must pass straight through");
    }

    #[test]
    fn the_state_parser_reads_nmcli_and_operstate_words() {
        assert_eq!(parse_state("enabled\n"), Some(true));
        assert_eq!(parse_state("disabled\n"), Some(false));
        assert_eq!(parse_state("up\n"), Some(true));
        assert_eq!(parse_state("lowerlayerdown\n"), Some(false));
        assert_eq!(parse_state("dormant\n"), None);
        assert_eq!(parse_state(""), None);
    }
}
