# LiNC MCP Server — Implementation Handoff

Status: design handoff, implementation not started  
Date: 2026-10-22  
Target: LiNC `main`  
License: GPL-2.0-or-later

---

## 0. Executive summary

Build a local MCP server, `linc-mcp`, that gives an agent full visibility into
LiNC and `linc-panel`:

- machine and task state
- panel link, raw frames and simulator state
- NML and Python bridge health
- HAL components, pins and signals
- program, editor, preview and toolpath
- offsets, tool table, probe and ATC
- UI state, page-local state and screenshots
- events, logs, command history and state transitions
- motion history and timing
- configuration, INI, HAL files and validation
- simulator control and fault injection
- diagnostic export bundles

"Full debugging" means complete observation and simulator control. It does not
mean unrestricted real-machine actuation.

Architecture:

```text
┌────────────────────┐   diag.sock    ┌───────────┐   MCP stdio   ┌───────┐
│ linc               │◄──────────────►│           │◄─────────────►│ agent │
│ UI + backend       │                │  linc-mcp │                └───────┘
│ UI state publisher │                │           │
└────────────────────┘                │  tools    │
┌────────────────────┐  panel.sock    │  resources│
│ linc-panel         │◄──────────────►│  prompts  │
│ serial or sim panel│                │  audit    │
└────────────────────┘                └───────────┘
```

Hard rules:

1. Do not put MCP inside `linc-ui` or on the UI thread.
2. Do not put MCP inside `linc-panel`'s control loop.
3. Do not open the panel serial port from `linc-mcp`.
4. Do not open a second NML status reader.
5. Do not expose arbitrary HAL writes or arbitrary NML sends.
6. Real machine is read-only by default.
7. Simulator writes require explicit flags.
8. Every write is gated by the same checks as the HMI backend.
9. Every tool call is audited.
10. Stale or unknown state fails closed for write decisions.

---

## 1. Why this is worth building

LiNC already has most of the data an agent needs:

- `MachineSnapshot` is `Clone + Serialize` and carries task, machine, positions,
  offsets, tool table, spindles, feed, program, tool change, probe, ATC, pendant,
  panel, HAL outputs, alarms and live trail.
- `BackendHandle` owns the backend worker and publishes snapshots at
  `backend.status_hz`.
- Diagnostics already aggregates panel link, NML, HAL, motion, turret, I/O and
  logs in `crates/linc-ui/src/screens/diagnostics.rs`.
- `crates/linc-core/src/sim_ipc.rs` already demonstrates atomic local JSON IPC.
- The panel service already owns the RS-485 link and simulator panel.

The missing pieces are instrumentation, a local diagnostic endpoint, and the
MCP adapter.

The highest-value capability is not "read a snapshot". It is:

```text
why_blocked(action) -> exact failing check + actual value + remediation
```

That turns "cycle start does nothing" or "MDI is rejected" into a precise answer.

---

## 2. Definition of "full debugging"

| Area | Required capability |
|---|---|
| Machine state | Current snapshot, history, diffs, transitions |
| Timing | Status age, command duration, panel cycle, motion history, UI frame time |
| Causality | Event timeline, command results, alarm history, state transitions |
| Configuration | Resolved profile, INI, HAL files, hashes, validation |
| Program | Content, line range, tokens, preview, toolpath, editor state |
| Offsets/tools | Full tables, revisions, active selection, probe/ATC state |
| Panel | Decoded snapshot, link counters, raw frames, simulator control |
| NML | Channel connectivity, ages, echo serial, bridge stderr, restart count |
| HAL | Declared outputs, real pin/signal snapshot, component/thread health |
| UI | Page, focus, modal, page-local state, screenshot |
| Logs | Filtered ring, bridge logs, panic records |
| Reproduction | Simulator state control, button injection, fault injection |
| Export | Redacted diagnostic bundle |
| Control | Simulator only; real machine read-only by default |

Not in scope:

- arbitrary Rust evaluation
- arbitrary shell commands
- arbitrary file read/write
- raw NML send
- arbitrary `hal setp` / `halcmd` mutation
- panel serial access from the MCP process
- real-machine jog, cycle, spindle, coolant, override or E-stop control
- replacing the diagnostics UI
- cloud connectivity
- safety certification

---

## 3. Current state and gaps

| Area | Current | Needed |
|---|---|---|
| External diagnostic access | None except sim panel JSON files | Unix diagnostic socket in `linc` and `linc-panel` |
| Backend events | `BackendHandle` queue drained by UI | Broadcast/tap that does not steal UI events |
| Command history | UI sees results; history not retained | Bounded command records with duration and result |
| Logs | `env_logger` writes stderr | Tee into bounded in-memory ring + crash file |
| UI state | Private fields on `LincApp` | Serializable `UiSnapshot` publisher |
| Screenshot | External X11 `import` in scripts | In-process egui screenshot request |
| Panel frames | Counters only | Raw RX/TX frame ring with decoded fields |
| Panel simulator control | Window and CLI startup state | Diagnostic commands for mode/buttons/overrides/faults |
| HAL pins | Synthetic list built from snapshot | Real read-only pin/signal enumeration |
| NML diagnostics | Connectivity booleans and ages | Bridge process details, stderr tail, request latency |
| Motion history | Live trail only | Time-series ring for following error, load, velocity |
| Shared gates | Repeated in backend and snapshot methods | One pure gate evaluator used by backend and MCP |
| Schema versioning | None | Versioned diagnostic envelope |
| MCP | None | `linc-mcp` crate and binary |
| Audit | None | Append-only local audit log |

Important: the HAL pin browser in Diagnostics is currently synthetic. It maps
known snapshot values to expected pin names; it does not enumerate the live HAL.
Do not describe it as real HAL introspection.

---

## 4. Architecture

### 4.1 Processes

```text
linc
  - egui UI
  - BackendHandle worker
  - MachineSnapshot publisher
  - UI state publisher
  - log ring
  - diagnostic socket server
  - optional simulator control bridge

linc-panel
  - RS-485 driver or sim panel
  - cnc_panel.* HAL surface
  - panel snapshot publisher
  - raw frame ring
  - diagnostic socket server
  - simulator fault injection

linc-mcp
  - connects to one `linc` socket and one `linc-panel` socket
  - merges state into one diagnostic view
  - exposes MCP tools, resources and prompts
  - enforces read-only / sim-only policy
  - audits calls
  - no HAL, NML, serial or LinuxCNC linkage
```

`linc-mcp` must not depend on `linc-linuxcnc`, `linc-hal` or `linc-panel`. It
only speaks the diagnostic protocol. This keeps it usable with both the current
Python bridge and the future Rust NML client.

### 4.2 Crates

| Crate | Responsibility |
|---|---|
| `linc-core` | Add `gates` and `diag` modules: pure gate evaluation, stable DTOs, ring buffers, event/log records, `UiSnapshot` types. No sockets, no MCP. |
| `linc-diag` | New crate. Sync Unix-socket JSON-RPC client/server, discovery, framing, envelope, error model. Depends on `linc-core`. |
| `linc-mcp` | New crate/binary. MCP adapter, tool/resource/prompt registration, policy, audit, bundle assembly. Depends on `linc-diag` and `linc-core`. |
| `linc-app` | Start diagnostic server, install log/panic taps, create UI publisher, add CLI flags. |
| `linc-panel` | Start panel diagnostic server, add frame ring and sim control methods. |
| `linc-ui` | Publish `UiSnapshot`, service screenshot/action requests. No MCP/network dependency. |
| `linc-hal` | Add read-only HAL snapshot/pin enumeration. Do not add an arbitrary write API. |
| `linc-linuxcnc` | Publish bridge diagnostics, command latency, stderr tail and restart count. No second NML reader. |

Dependency direction:

```text
linc-core <- linc-diag <- linc-mcp
    ^
    ├── linc-ui
    ├── linc-hal
    ├── linc-linuxcnc
    ├── linc-panel
    └── linc-app
```

`linc-ui` may use `linc-core::diag` types. It must not depend on `linc-diag` or
`linc-mcp`.

### 4.3 Attach model

Primary mode: attach to already-running processes.

- `linc` creates `$XDG_RUNTIME_DIR/linc/<instance>/linc.sock`.
- `linc-panel` creates `$XDG_RUNTIME_DIR/linc/<instance>/panel.sock`.
- Each writes `instance.json` with:
  - schema version
  - pid
  - profile name and path
  - build ID
  - started_at
  - socket paths
  - process role
- `linc-mcp` reads `instance.json`, validates the pid, connects.
- `--socket` and `--panel-socket` override discovery.
- `LINC_DIAG_DIR` overrides the runtime directory for tests.
- Stale sockets are removed only after checking the pid is not alive.

Fallback when `XDG_RUNTIME_DIR` is unset: `/tmp/linc-<uid>/`. Directory mode
`0700`; socket mode `0600`.

Secondary mode: `linc-mcp --launch-sim --config configs/sim.toml` may spawn
`linc-panel --sim` and `linc --sim`, wait for sockets, and shut them down on
exit. This is for tests and demos, not for the real machine.

Do not make `linc-mcp` spawn a second `linc` that owns the real backend. That
would duplicate NML/HAL ownership.

---

## 5. Data plane

### 5.1 Snapshot tap

Add to `linc-core::diag`:

```rust
pub struct DiagTap {
    shared: Arc<DiagShared>,
}

pub struct DiagShared {
    latest: Mutex<Option<DiagFrame>>,
    history: Mutex<VecDeque<DiagFrame>>,
    events: Mutex<VecDeque<DiagEvent>>,
    logs: Mutex<VecDeque<LogRecord>>,
    dropped_events: AtomicU64,
    dropped_logs: AtomicU64,
    snapshot_seq: AtomicU64,
    event_seq: AtomicU64,
}
```

`BackendHandle` currently owns `Shared` and publishes snapshots to the UI. Add a
cloneable `DiagTap` to `Shared`. On each publish:

1. Clone the snapshot.
2. Increment `snapshot_seq`.
3. Publish a `DiagFrame` to the tap.
4. Publish state-transition events.
5. Do not serialize on the backend thread.

`DiagFrame`:

```rust
pub struct DiagFrame {
    pub sequence: u64,
    pub captured_at_ms: u64,
    pub age_ms: u64,
    pub stale: bool,
    pub snapshot: MachineSnapshot, // full snapshot; cheap clone due Arc fields
    pub dropped_events: u64,
    pub dropped_logs: u64,
}
```

History policy:

- One frame per second while idle.
- One frame on every discrete state transition.
- One frame immediately before and after every command.
- History capacity default 600 frames (10 minutes at 1 Hz).
- Full snapshots are acceptable because `program.content` and `live_trail` use
  `Arc`; but do not serialize them by default.

Do not store a frame at 20 Hz with full copies. Use the separate motion history
ring for high-rate data.

### 5.2 Event tap

The current `BackendHandle` queue is drained by the UI. Add a tap that copies
events rather than consuming them.

`DiagEvent`:

```rust
pub struct DiagEvent {
    pub sequence: u64,
    pub at_ms: u64,
    pub kind: DiagEventKind,
    pub severity: Option<AlarmSeverity>,
    pub source: Option<AlarmSource>,
    pub summary: String,
    pub data: serde_json::Value,
}
```

Required event kinds:

- `command_started`
- `command_finished`
- `command_rejected`
- `alarm`
- `log`
- `snapshot_lost`
- `snapshot_stale`
- `panel_connected`
- `panel_disconnected`
- `panel_crc_error`
- `panel_timeout`
- `panel_reconnect`
- `panel_button_edge`
- `panel_mode_changed`
- `panel_protect_changed`
- `nml_connected`
- `nml_lost`
- `nml_status_stale`
- `bridge_restart`
- `task_power_changed`
- `task_mode_changed`
- `interp_changed`
- `program_loaded`
- `program_saved`
- `program_started`
- `program_paused`
- `program_stopped`
- `program_line_changed` (rate-limited; not every poll)
- `tool_change_started`
- `tool_change_prompt`
- `tool_change_confirmed`
- `tool_change_complete`
- `tool_change_fault`
- `probe_started`
- `probe_result`
- `offset_changed`
- `tool_table_changed`
- `hal_write`
- `hal_write_error`
- `ui_page_changed`
- `ui_modal_opened`
- `ui_modal_closed`
- `ui_action`
- `config_reloaded`
- `process_started`
- `process_stopping`
- `panic`

Event capacity default 4096. Drop oldest; increment `dropped_events`.

### 5.3 Command history

`BackendEvent::Command` currently stores only the command label. Add a
diagnostic record with sanitized arguments and timing.

```rust
pub struct CommandRecord {
    pub id: u64,
    pub action: String,
    pub args: serde_json::Value,
    pub origin: Option<MdiOrigin>,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub duration_ms: u64,
    pub result: Result<(), MachineError>,
    pub result_kind: Option<&'static str>,
    pub request_id: Option<String>,
}
```

Rules:

- `SaveProgram` content is redacted by default; include a SHA-256 and byte count.
- `LoadProgram` path is included; it is already local.
- MDI block is included; it is essential debugging data.
- Panel-originated actions do not pass through `MachineCommand`; detect them as
  panel button edges and state transitions.
- Capacity default 2048.

### 5.4 Log tap

`env_logger` writes to stderr. Full debugging needs an in-memory ring.

Implementation:

- Custom `log::Log` wrapper around the existing `env_logger` logger.
- Forward every record to `env_logger` unchanged.
- Push a `LogRecord` into the diag ring.
- Filter at the `log` facade level so disabled records cost nothing.
- Capture `target`, `level`, `message`, timestamp, thread name.
- Do not log from inside the ring push path.
- Bridge stderr lines use target `linc_linuxcnc.bridge`.
- Python helper fatal/protocol errors are also pushed as `DiagEvent`.

`LogRecord`:

```rust
pub struct LogRecord {
    pub sequence: u64,
    pub at_ms: u64,
    pub level: log::Level,
    pub target: String,
    pub thread: Option<String>,
    pub message: String,
}
```

Capacity default 4096; drop oldest; increment `dropped_logs`.

### 5.5 Panic and crash records

Install `std::panic::set_hook` in `linc`, `linc-panel` and `linc-mcp`.

- Write a JSON crash record to
  `$XDG_STATE_HOME/linc/crash-<role>-<pid>.json`.
- Include timestamp, pid, role, build, panic message, location, backtrace if
  available.
- Push a `panic` event if the process survives long enough.
- `linc-mcp` exposes `linc_get_crash_records` and includes them in bundles.
- Release profile uses `panic = "abort"`; the hook still runs before abort.

### 5.6 Motion history

High-rate debugging needs a time series, not just the latest snapshot.

`MotionSample` at configurable 10 Hz (default) for 120 seconds:

```text
at_ms, sequence,
positions: { axis: { work, machine, dtg, velocity } },
commanded, actual, following_error, servo_load,
spindle_speed, spindle_load, spindle_override,
feed, feed_override, rapid_override,
task_mode, interp, program_line, motion_line,
estop, power, panel_mode
```

Capacity default 1200 samples. Downsample for MCP responses. Use this for:

- following error spikes
- servo load
- stalls
- velocity mismatch
- program-line vs motion-line lag
- spindle load correlation

Do not store the full live trail in this ring; it is already in the snapshot.

### 5.7 Panel frame ring

Add to `linc-panel`:

```rust
pub struct PanelFrameRecord {
    pub sequence: u64,
    pub at_ms: u64,
    pub direction: PanelDirection, // Rx | Tx
    pub bytes: Vec<u8>,
    pub decoded: Option<serde_json::Value>,
    pub crc_ok: Option<bool>,
    pub error: Option<String>,
    pub synthetic: bool,
}
```

Instrument `crates/linc-panel/src/serial.rs`:

- Request frame: `EB 01 80 8E`.
- Response: 12 bytes.
- LED/7-segment frame: 14 bytes plus 4-byte command.
- Record every request, response and write.
- Record CRC failures and timeouts with the raw bytes.
- Record reconnect attempts and results.
- Never log raw frames at info level; ring only.

Capacity default 2048 frames. At 20 Hz that is roughly 100 seconds of traffic.
Sim panel frames are marked `synthetic: true`; do not fabricate raw bytes.

Add `PanelSnapshot.sequence` and `PanelSnapshot.generated_at_ms` if missing, so
the HMI-side copy and the panel-side copy can be correlated.

### 5.8 UI state publisher

`LincApp` is not `Send` and must not be accessed from the diagnostic thread.
Publish a serializable `UiSnapshot` from the UI frame instead.

```rust
pub struct UiSnapshot {
    pub sequence: u64,
    pub at_ms: u64,
    pub page: String,
    pub last_non_mode_page: String,
    pub focus_region: String,
    pub nav_focus: usize,
    pub modal: Option<String>,
    pub toasts: Vec<String>,
    pub viewport: ViewportInfo,
    pub frame_time_ms: f32,
    pub fps: f32,
    pub design_scale: f32,
    pub program: ProgramUiSnapshot,
    pub files: FilesUiSnapshot,
    pub tools: ToolsUiSnapshot,
    pub offsets: OffsetsUiSnapshot,
    pub probe: ProbeUiSnapshot,
    pub mdi: MdiUiSnapshot,
    pub help: HelpUiSnapshot,
    pub diagnostics: DiagnosticsUiSnapshot,
    pub settings: SettingsUiSnapshot,
    pub page_state: serde_json::Value,
}
```

Required fields:

- `page`, `last_non_mode_page`
- `focus_region`, `nav_focus`
- `modal`, `toasts`
- `viewport { width, height, pixels_per_point, fullscreen }`
- `frame_time_ms`, `fps`, `design_scale`
- `program`: view, editor path, dirty, revision, cursor line, selection,
  search, undo/redo depth, selected line, graph view/zoom/pan, fit pending
- `files`: active pane, current paths, selection, mount state, copy progress
- `tools`: selected row, target tool, current/prepared tool
- `offsets`: selected row, active WCS, DRO
- `probe`: category, operation, parameter draft, result, error
- `mdi`: input, history, last result, focus
- `help`: query, anchor, history
- `diagnostics`: tab, search, severity/source filters
- `settings`: density
- `page_state`: page-local values not covered above

Publish at 10 Hz or on change. Capacity default 100 UI snapshots for history.
Include UI transitions in `DiagEvent`.

### 5.9 Screenshot path

Use egui's own screenshot request, not X11 `import`, in production.

- `linc-mcp` sends `diag.screenshot`.
- `linc` enqueues a `ScreenshotRequest` with id, page, scale and timeout.
- UI thread requests a viewport screenshot.
- On the next frame, return PNG bytes plus `UiSnapshot`, viewport size and DPI.
- Timeout after 5 seconds.
- Support cancellation.
- Real machine: current screen only. Simulator: optional `page` navigation
  before capture.
- Return MCP `ImageContent` with `image/png`; do not base64-encode inside JSON
  unless the client requires it.

### 5.10 Build and configuration information

`BuildInfo`:

```text
linc version, linc-mcp version, diag schema version,
git commit, git describe, dirty flag, build time, rustc, target, profile,
backend kind, panel driver, LinuxCNC version, Python version,
process role, pid, started_at, uptime,
profile path + sha256, INI path + sha256, HAL file paths + sha256,
default status_hz, actual status rate
```

Add a build script or environment injection for the git commit. Do not shell out
at runtime for build info.

Configuration tools must expose:

- raw profile TOML and the `extends` chain
- resolved profile values
- parsed INI sections
- HAL file list and contents
- tool table path, columns, changer kind
- validation results from `Profile::load` / `validate`
- `LincOutput::ALLOWED` and `config.hal.allowed_outputs`
- post-GUI net expectations

`Config` and `MachineIni` are not all `Serialize` today. Add `Serialize` derives
or generate stable DTOs. Do not expose Rust debug formatting as the API.

### 5.11 Ring buffer and response budgets

| Buffer | Default capacity | Drop policy |
|---|---:|---|
| Snapshot history | 600 frames | oldest |
| Events | 4096 | oldest |
| Logs | 4096 | oldest |
| Commands | 2048 | oldest |
| Motion samples | 1200 (10 Hz × 120 s) | oldest |
| Panel frames | 2048 | oldest |
| UI snapshots | 100 | oldest |
| Audit log | 5 × 10 MB files | rotate |

Budgets:

- Snapshot clone on backend thread: under 1 ms.
- Summary serialization: under 5 ms.
- Full snapshot without program content/trail: under 20 ms.
- Inline MCP response: 1 MB default; larger data uses a resource or export file.
- Screenshot: under 2 seconds.
- Bundle: under 10 seconds for 10 MB.
- Total diagnostic memory: target under 50 MB.
- No buffer may block the backend or UI thread.
- All ring pushes use `try_lock` or a short critical section; on contention,
  increment a drop counter rather than stalling.

---

## 6. Shared gates and `why_blocked`

### 6.1 The problem

Gate logic is currently spread across:

- `MachineSnapshot::can_modify_machine`
- `MachineSnapshot::writes_allowed`
- `MachineSnapshot::program_editing_allowed`
- `MachineSnapshot::mdi_allowed`
- `MachineSnapshot::issue_allowed`
- `NmlBackend::validate_issue`
- `NmlBackend::validate_write`
- `SimBackend::require_issue`
- individual command arms

If `why_blocked` reimplements these, it will drift and lie. Refactor first.

### 6.2 Gate model

Add `linc-core::gates`:

```rust
pub enum Action {
    IssueMdi,
    IssueToolChange,
    ConfirmManualToolChange,
    SetCurrentTool,
    ToggleLimitsOverride,
    LoadProgram,
    SaveProgram,
    SetWorkOffset,
    SetG92Offset,
    ZeroWorkOffset,
    SetWorkPosition,
    SetToolTableRow,
    ApplyProbeResult,
    ResetProbeCalibration,
    RunFromLine,
    ClearTrail,
    // panel-owned, observed only:
    CycleStart,
    FeedHold,
    CycleStop,
    Jog,
    Home,
    ProgHome,
    Spindle,
    Coolant,
    Override,
    EstopReset,
    PowerOn,
}

pub struct GateContext {
    pub tool: Option<u16>,
    pub line: Option<u32>,
    pub path: Option<PathBuf>,
    pub axis: Option<String>,
    pub expected_revision: Option<u64>,
}

pub struct Check {
    pub id: &'static str,
    pub ok: bool,
    pub actual: serde_json::Value,
    pub expected: &'static str,
    pub reason_code: &'static str,
    pub remediation: Option<&'static str>,
}

pub struct GateReport {
    pub action: Action,
    pub authority: Authority, // Hmi | Panel | ViewOnly
    pub allowed: bool,
    pub checks: Vec<Check>,
    pub first_failure: Option<usize>,
    pub machine_error: Option<MachineError>,
    pub spec_refs: Vec<&'static str>,
}

pub fn evaluate(action: Action, snapshot: &MachineSnapshot, ctx: &GateContext) -> GateReport;
```

Rules:

- Pure function. No I/O, no clock, no logging.
- Uses only `MachineSnapshot` and `GateContext`.
- Backends call `evaluate` and map `machine_error` to their existing
  `MachineError`.
- MCP calls the same function.
- Tests assert backend rejections and gate failures match for representative
  states.

Panel-owned actions return `authority: Panel` and are never executable through
MCP. `why_blocked` can still explain why the panel action would be refused.

### 6.3 Action catalog

| Action | Authority | Notes |
|---|---|---|
| `issue_mdi` | HMI | Requires panel MDI mode, machine on, homed, idle, no program |
| `issue_tool_change` | HMI | `Tn M6` via issue path |
| `confirm_manual_tool_change` | HMI | Panel connected, valid mode, pending prompt, matching token |
| `set_current_tool` | HMI | `M61` path; not a tool change |
| `toggle_limits_override` | HMI | Controller-owned toggle |
| `load_program` | HMI | Requires not running, safe path |
| `save_program` | HMI | Requires protect off, machine on, idle, EDIT mode, revision match |
| `set_work_offset` | HMI | Requires writes allowed and G54–G59.3 |
| `set_g92_offset` | HMI | Requires writes allowed |
| `zero_work_offset` | HMI | Requires writes allowed and G54–G59.3 |
| `set_work_position` | HMI | Requires writes allowed and known axes |
| `set_tool_table_row` | HMI | Requires writes allowed and revision match |
| `apply_probe_result` | HMI | Requires writes allowed |
| `reset_probe_calibration` | HMI | Requires writes allowed |
| `run_from_line` | HMI | Requires writes allowed, AUTO mode, loaded program, valid line |
| `clear_trail` | View-only | Safe while running |
| `cycle_start` | Panel | Never issued by HMI/MCP |
| `feed_hold` | Panel | Never issued |
| `cycle_stop` | Panel | Never issued |
| `jog` | Panel | Never issued |
| `home` | Panel | Never issued |
| `proghome` | Panel | Panel issues MDI; HMI observes |
| `spindle` | Panel | Never issued |
| `coolant` | Panel | Never issued |
| `override` | Panel | Never issued |
| `estop_reset` | Panel | Hardware/panel only |
| `power_on` | Panel | Panel only |

### 6.4 Check catalog

Use stable IDs and reason codes.

| Check ID | Failure code | Meaning |
|---|---|---|
| `snapshot.fresh` | `STALE_SNAPSHOT` | Snapshot age exceeds threshold or `meta.stale` |
| `panel.connected` | `PANEL_OFFLINE` | Panel service not connected |
| `panel.communication_error` | `PANEL_COMM_ERROR` | Panel reports link error |
| `panel.mode.valid` | `PANEL_MODE_UNKNOWN` | Zero/multiple one-hot modes |
| `panel.mode.required` | `WRONG_PANEL_MODE` | Action requires EDIT/MDI/AUTO |
| `panel.protect.known` | `PROTECT_UNKNOWN` | Protect input stale/unknown |
| `panel.protect.off` | `PROTECT_ON` | Program protect active |
| `task.power.on` | `MACHINE_OFF` | Machine not powered |
| `task.estop.clear` | `ESTOP_ACTIVE` | E-stop latched |
| `task.interp.idle` | `NOT_IDLE` | Interpreter not idle |
| `task.not_running` | `PROGRAM_RUNNING` | Program running |
| `machine.homed` | `NOT_HOMED` | Any axis not homed |
| `program.loaded` | `NO_PROGRAM` | No loaded program |
| `program.line.valid` | `LINE_OUT_OF_RANGE` | Line outside program |
| `program.revision.match` | `REVISION_CONFLICT` | Revision mismatch |
| `tool_change.pending` | `NO_PENDING_TOOL_CHANGE` | No manual prompt |
| `tool_change.token` | `STALE_TOOL_CHANGE_TOKEN` | Token mismatch |
| `probe.enabled` | `PROBE_DISABLED` | Probe feature off |
| `probe.tool.match` | `PROBE_TOOL_MISMATCH` | Probe tool number not expected |
| `path.under_nc_files` | `UNSAFE_PATH` | Path escapes configured root |
| `path.regular_file` | `UNSAFE_PATH` | Symlink, directory or special file |
| `path.extension` | `UNSAFE_PATH` | Extension not allowed |
| `nml.connected` | `NML_LOST` | NML command/status channel lost |
| `hal.available` | `HAL_UNAVAILABLE` | HAL component not available |
| `config.valid` | `CONFIG_INVALID` | Profile/INI validation failed |

The gate must return `unknown` as failure for safety inputs. Never treat stale
protect, stale panel state or unknown mode as pass.

### 6.5 `why_blocked` response

```json
{
  "schema_version": 1,
  "action": "issue_mdi",
  "authority": "hmi",
  "allowed": false,
  "first_failure": {
    "id": "panel.mode.required",
    "reason_code": "WRONG_PANEL_MODE",
    "expected": "MDI",
    "actual": "AUTO",
    "remediation": "Turn the physical panel mode selector to MDI."
  },
  "checks": [
    {
      "id": "snapshot.fresh",
      "ok": true,
      "actual": { "age_ms": 12, "stale": false }
    },
    {
      "id": "panel.connected",
      "ok": true,
      "actual": true
    },
    {
      "id": "panel.mode.required",
      "ok": false,
      "expected": "MDI",
      "actual": "AUTO"
    }
  ],
  "machine_error": "WrongPanelMode",
  "spec_refs": ["SPEC.md §6.9", "SPEC.md §8.2"]
}
```

`why_blocked` must not execute anything. It is a pure read.

### 6.6 Staleness policy

- `snapshot.meta.stale == true` or `age_ms > diagnostics.stale_ms` means
  `STALE_SNAPSHOT`.
- `panel.snapshot_age_ms` older than 2× panel cycle means panel state unknown.
- `machine.status_age_ms` older than threshold means NML status unknown.
- `machine.command_age_ms` older than threshold means command channel suspect.
- `why_blocked` returns `allowed: false` for any write action when the snapshot
  is stale, even if the last known values look safe.
- Read-only tools return stale data with `stale: true` and `age_ms`; they do not
  pretend it is current.
- `linc_health_check` raises a critical finding for stale safety inputs.

---

## 7. Diagnostic protocol

### 7.1 Transport

- Unix domain socket, stream.
- JSON-RPC 2.0, one JSON object per line.
- Request and response IDs are strings or integers.
- Server sends notifications for subscriptions.
- No HTTP inside `linc` or `linc-panel`; `linc-mcp` owns MCP transport.
- Socket mode `0600`; directory `0700`.
- On Linux, check peer credentials with `SO_PEERCRED`; accept only the same uid.
- Limit frame size to 8 MB; reject larger requests.
- Request timeout default 2 seconds; long operations have explicit deadlines.
- Cancel via `diag.cancel` with the request id.

### 7.2 Envelope

Every response:

```json
{
  "schema_version": 1,
  "source": "linc",
  "process": {
    "pid": 1234,
    "role": "hmi",
    "build": {
      "version": "0.1.0",
      "git_commit": "abc1234",
      "git_dirty": false,
      "profile": "sim",
      "backend": "sim"
    }
  },
  "sequence": 4567,
  "captured_at_ms": 1760000000000,
  "age_ms": 12,
  "stale": false,
  "dropped": {
    "events": 0,
    "logs": 0,
    "snapshots": 0,
    "panel_frames": 0
  },
  "data": {}
}
```

Rules:

- `schema_version` is independent of the crate version.
- Additive fields do not bump the version.
- Removing or renaming a field bumps the major version.
- `age_ms` is computed at response time from `captured_at_ms`.
- `stale` is the source's own stale flag, not a guess.
- `data` is the method-specific payload.

### 7.3 Methods

| Method | Purpose |
|---|---|
| `diag.hello` | Protocol version, build, capabilities, limits |
| `diag.ping` | Liveness and clock sample |
| `diag.snapshot` | Latest or historical snapshot, section-filtered |
| `diag.snapshot_diff` | Field-level diff between two sequences |
| `diag.events` | Event page |
| `diag.logs` | Log page |
| `diag.commands` | Command history |
| `diag.alarms` | Alarm page |
| `diag.timeline` | Merged timeline |
| `diag.why_blocked` | Shared gate report |
| `diag.ui` | UI snapshot |
| `diag.screenshot` | PNG screenshot |
| `diag.config` | Profile/INI/HAL/validation |
| `diag.help` | G-code help search/section |
| `diag.program` | Program content/line/preview |
| `diag.files` | Files UI state and restricted file read |
| `diag.offsets` | Offsets |
| `diag.tools` | Tool table, tool change, ATC |
| `diag.probe` | Probe state |
| `diag.panel` | Panel snapshot/link |
| `diag.panel_frames` | Raw panel frames |
| `diag.nml` | NML/bridge diagnostics |
| `diag.hal` | Declared outputs and real HAL snapshot |
| `diag.motion` | Latest motion and history |
| `diag.io` | I/O state |
| `diag.metrics` | Internal performance counters |
| `diag.export_bundle` | Write diagnostic bundle |
| `diag.crash_records` | Crash records |
| `diag.sim.*` | Simulator-only control |
| `diag.subscribe` | Event/resource subscription |
| `diag.cancel` | Cancel a long request |

`linc-mcp` maps these to MCP tools; it should not expose raw `diag.*` methods to
the agent.

### 7.4 Errors

```json
{
  "code": "PANEL_OFFLINE",
  "message": "panel service is not connected",
  "details": { "socket": "/run/user/1000/linc/sim/panel.sock" },
  "retryable": true
}
```

Codes:

- `INVALID_ARGUMENT`
- `NOT_FOUND`
- `STALE_SNAPSHOT`
- `LINC_NOT_RUNNING`
- `PANEL_NOT_RUNNING`
- `UNSAFE_PATH`
- `WRITE_DISABLED`
- `NOT_SIM`
- `CONFIRMATION_REQUIRED`
- `CONFIRMATION_DENIED`
- `TIMEOUT`
- `CANCELLED`
- `SCHEMA_MISMATCH`
- `INTERNAL`

MCP tools return `isError: true` with this object as text content. Do not panic,
abort the server, or return a Rust backtrace as the only error.

### 7.5 Pagination and bounds

List responses accept:

```json
{
  "since_sequence": 1000,
  "until_sequence": 2000,
  "since_ms": 1760000000000,
  "limit": 100,
  "cursor": "opaque"
}
```

Rules:

- Default limit 100; maximum 1000 for logs/events/frames; maximum 100 for
  snapshots.
- Response includes `next_cursor` or `null`.
- Oversized strings are truncated with `truncated: true`.
- Program content supports `line_start` and `line_count`.
- Toolpath supports `max_points` and returns `downsampled: true`.
- Screenshots are never truncated; they are image content.

### 7.6 Subscriptions

`diag.subscribe` topics:

- `snapshot`
- `events`
- `logs`
- `panel`
- `panel_frames`
- `ui`
- `alarms`

Server sends `diag.notify` with the topic, sequence and payload. `linc-mcp` uses
this to keep its cache fresh and to implement MCP resource subscriptions.

If subscriptions are not implemented in the first phase, `linc-mcp` may poll
`diag.events` and `diag.snapshot` at 2–5 Hz. Do not poll at status rate.

---

## 8. MCP server surface

### 8.1 Server metadata

```text
name: "linc"
version: workspace version
title: "LiNC diagnostics"
instructions: |
  Read-only LiNC diagnostics. Real-machine control is disabled by default.
  Check `stale` and `age_ms` before trusting state. Panel and hardware E-stop
  remain authoritative. Use linc_why_blocked before suggesting an action.
```

Capabilities:

- tools: list changed false
- resources: subscribe true
- prompts: list changed false
- logging: optional

Tool names are prefixed `linc_` and use snake_case. Do not use generic names
like `get_state`.

### 8.2 Response rules

- JSON tools return a JSON object in text content.
- Large data returns a resource link or export path, not an inline blob.
- Images return MCP image content.
- Every state response includes `schema_version`, `source`, `captured_at_ms`,
  `age_ms`, `stale`, `dropped`.
- Every list response includes `next_cursor`.
- Every write response includes `audit_id`.
- Do not put logs on stdout in `linc-mcp`; stdout is the MCP transport. Use
  stderr.

### 8.3 Tool annotations

| Tool class | readOnlyHint | destructiveHint | idempotentHint | openWorldHint |
|---|---|---|---|---|
| State/config read | true | false | true | false |
| Screenshot | true | false | false | false |
| Export bundle | false | false | false | false |
| Simulator state change | false | true | false | false |
| Simulator command | false | true | false | false |
| Real machine write | false | true | false | false |

Annotations are hints, not security. Policy is enforced server-side.

---

## 9. Tool catalog

This is the target surface. Implement in the order listed in §14.

### 9.1 Identity and health

| Tool | Input | Returns |
|---|---|---|
| `linc_get_build_info` | none | BuildInfo |
| `linc_get_environment` | none | allowlisted env, runtime dir, display, profile |
| `linc_health_check` | `stale_ms?` | HealthReport with findings and severity |
| `linc_get_metrics` | none | rates, queue depths, drops, memory, UI frame time |
| `linc_get_crash_records` | `limit?` | CrashRecord[] |

`linc_health_check` must check:

- snapshot freshness
- panel connection and communication error
- panel protect known/effective
- NML command/status connectivity and ages
- HAL thread/component availability
- machine fault/estop
- homing state
- tool table load error
- program prefix existence
- config validation result
- disk space for export directory
- log error count
- dropped event/log counters

### 9.2 Machine state

| Tool | Input | Returns |
|---|---|---|
| `linc_get_snapshot` | `sections?`, `include_trail?`, `trail_points?`, `include_program_content?`, `max_bytes?` | SnapshotEnvelope |
| `linc_get_state_summary` | none | operator summary |
| `linc_wait_for_change` | `since_sequence?`, `timeout_ms?` | WaitResult |
| `linc_diff_snapshots` | `from_sequence`, `to_sequence`, `ignore?` | SnapshotDiff |
| `linc_get_timeline` | `around_ms?`, `window_ms?`, `since_sequence?`, `limit?` | Timeline |
| `linc_get_events` | `since_sequence?`, `kind?`, `severity?`, `source?`, `limit?` | EventPage |
| `linc_get_alarms` | `active_only?`, `severity?`, `source?`, `since_ms?`, `limit?` | AlarmPage |
| `linc_get_command_history` | `since_ms?`, `limit?`, `include_args?` | CommandPage |
| `linc_get_logs` | `since_ms?`, `level?`, `target?`, `limit?` | LogPage |

`linc_get_snapshot` sections:

```text
meta, task, machine, positions, offsets, tool_table, spindles, feed,
program, tool_change, probe, atc, pendant, panel, hal, alarms,
live_trail, tool_tip
```

Default excludes `program.content` and `live_trail`. `include_program_content`
is explicit and capped. `trail_points` defaults 200 and caps at 5000.

`linc_get_state_summary` returns:

```json
{
  "machine": "BaierMill",
  "mode": "AUTO",
  "interp": "IDLE",
  "power": "ON",
  "estop": false,
  "panel": "ONLINE",
  "protect": false,
  "program": "part.nc",
  "line": 42,
  "tool": 3,
  "next_tool": 0,
  "wcs": "G54",
  "alarms": 0,
  "blockers": [],
  "stale": false,
  "age_ms": 12
}
```

### 9.3 Diagnosis

| Tool | Input | Returns |
|---|---|---|
| `linc_why_blocked` | `action`, `context?` | GateReport |
| `linc_explain_alarm` | `alarm_id?`, `code?` | AlarmExplanation |
| `linc_compare_panel_views` | none | HMI vs panel snapshot diff |
| `linc_export_diagnostics_bundle` | `name?`, `include_screenshot?`, `include_program?`, `redact?` | BundleResult |

`linc_why_blocked` action values are the snake_case names from §6.3.

`linc_explain_alarm` returns:

- alarm record
- surrounding events
- related panel/NML/HAL state at that time
- config/tool/program context
- spec/help references
- suggested diagnostic tools
- no remediation that bypasses a safety interlock

### 9.4 Configuration

| Tool | Input | Returns |
|---|---|---|
| `linc_get_profile` | `include_raw?`, `include_resolved?` | ProfileInfo |
| `linc_get_ini` | `sections?` | IniInfo |
| `linc_get_hal_files` | none | HalFilesInfo |
| `linc_validate_config` | `include_hal_checks?` | ValidationReport |
| `linc_get_help` | `query?`, `anchor?` | HelpResult |
| `linc_get_spec` | `section?` | SpecDocument |

`linc_get_profile` includes:

- profile path, sha256, extends chain
- resolved TOML
- validation errors
- effective variant, axes, units, changer
- `hal.allowed_outputs` and `LincOutput::ALLOWED`
- machine INI path/hash
- program prefix and tool table path
- preferences path

`linc_validate_config` checks:

- profile merge/validate result
- INI required sections
- axis count/order vs INI
- tool table schema and `P` column for ATC/turret
- program prefix exists
- HAL files exist
- `hal.allowed_outputs` exactly equals `LincOutput::ALLOWED`
- every `linc.*` output has a post-GUI net
- panel pin names present
- duplicate HAL signal assignments if parseable
- `HALFILE` vs `POSTGUI_HALFILE` placement for `linc.*`

Do not claim a full HAL linter unless implemented.

### 9.5 Program, editor and files

| Tool | Input | Returns |
|---|---|---|
| `linc_get_program` | `include_content?`, `line_start?`, `line_count?` | ProgramInfo |
| `linc_get_program_line` | `line`, `context?` | ProgramLine |
| `linc_get_preview` | `include_segments?`, `max_segments?`, `selected_line?` | PreviewInfo |
| `linc_get_toolpath` | `max_points?`, `include_live?` | ToolpathInfo |
| `linc_get_editor_state` | none | EditorInfo |
| `linc_get_files_state` | none | FilesInfo |
| `linc_read_file` | `path`, `max_bytes?` | FileContent |
| `linc_preflight_program` | `path?` | PreflightReport |
| `linc_capture_screenshot` | `page?`, `scale?`, `include_cursor?` | image/png |
| `linc_get_screen_model` | `page` | ScreenModel |

`linc_read_file` restrictions:

- only `NC_FILES`, USB root, config files, INI, HAL files and logs
- canonicalize
- reject symlinks and path escapes
- reject non-regular files
- extension allowlist for G-code
- max 1 MB default; 10 MB max
- no binary files unless explicitly a diagnostic artifact

`linc_preflight_program` checks without motion:

- parse/syntax
- tools referenced vs tool table
- work offsets used vs configured
- units and plane changes
- spindle/coolant commands
- program bounds vs machine travel
- tool changes and probe calls
- missing `P` pocket for turret
- program number and subroutines
- any use of unsupported LinuxCNC features
- no execution

`linc_get_screen_model` returns the current page's view model:

- Machine: axis cards, program card, gauges, health, alarms
- Program: editor, preview, modal, graph
- Offsets: DRO, rows, active WCS
- Tools: table, target, current/prepared
- Probe: category, operation, params, result
- MDI: input, history, last result
- Manual: coordinates, jog state
- Homing: per-axis state
- Files: panes, mount, copy
- Diagnostics: tab, filters
- Settings/Help: visible state

This is for UI debugging without reading pixels.

### 9.6 Offsets, tools, probe and ATC

| Tool | Input | Returns |
|---|---|---|
| `linc_get_offsets` | none | OffsetsInfo |
| `linc_get_tool_table` | `include_raw?` | ToolTableInfo |
| `linc_get_tool_change_state` | none | ToolChangeInfo |
| `linc_get_probe_state` | `include_history?` | ProbeInfo |
| `linc_get_atc_state` | none | AtcInfo |
| `linc_get_spindles` | none | SpindleInfo[] |
| `linc_get_pendant` | none | PendantInfo |

### 9.7 Panel

| Tool | Input | Returns |
|---|---|---|
| `linc_get_panel` | `include_bits?` | PanelInfo |
| `linc_decode_panel_bits` | `kind`, `mask` | decoded names |
| `linc_get_panel_link` | none | PanelLinkInfo |
| `linc_get_panel_frames` | `since_sequence?`, `direction?`, `errors_only?`, `limit?` | PanelFramePage |
| `linc_compare_panel_views` | none | PanelViewDiff |

`linc_get_panel` includes:

- connected, communication_error
- mode validity, label, raw mode field if available
- overrides
- protect raw/known/effective
- estop, power_on
- buttons/leds raw masks and decoded names
- tool displays, status display
- home/proghome/step/jog state
- link stats
- capabilities
- pendant
- snapshot age
- sequence

`linc_get_panel_frames` returns hex and decoded records. Default limit 100,
maximum 1000. Include `crc_ok`, `error`, `synthetic`.

### 9.8 NML, HAL, motion and I/O

| Tool | Input | Returns |
|---|---|---|
| `linc_get_nml` | `include_stderr?` | NmlInfo |
| `linc_get_hal` | none | HalInfo |
| `linc_get_hal_pins` | `filter?`, `component?`, `type?`, `limit?` | HalPinPage |
| `linc_get_hal_signals` | `filter?`, `limit?` | HalSignalPage |
| `linc_get_motion` | none | MotionInfo |
| `linc_get_motion_history` | `seconds?`, `max_samples?` | MotionHistory |
| `linc_get_io` | none | IoInfo |

`linc_get_nml` includes:

- command/status connected
- command/status age ms
- echo serial
- bridge pid, alive, restart count
- last request/reply, latency
- protocol errors
- stderr tail
- fatal error
- backend kind (`python` or `nml`)

`linc_get_hal` includes:

- component name/id, ready, thread ok
- declared outputs and values
- write errors
- heartbeat sequence
- real HAL availability and reason

`linc_get_hal_pins` and `linc_get_hal_signals` require real HAL introspection.
If unavailable, return `available: false` with a reason. Never fabricate pins.

### 9.9 Simulator control

All tools in this section require:

- `profile.machine.kind == "sim"` or `--sim`
- `--allow-sim-control`
- a connected `linc-panel` simulator socket

| Tool | Input | Returns |
|---|---|---|
| `linc_sim_get_state` | none | SimPanelState |
| `linc_sim_set_panel` | connected, mode, overrides, protect, estop, power, buttons, leds, sseg values | PanelInfo |
| `linc_sim_press_button` | `button` or `mask`, `duration_ms?` | PanelInfo |
| `linc_sim_release_button` | `button` or `mask` | PanelInfo |
| `linc_sim_set_raw_mode` | `field` or `bits` | PanelInfo |
| `linc_sim_inject_fault` | `kind`, `count?` | PanelInfo |
| `linc_sim_clear_faults` | none | PanelInfo |
| `linc_sim_reset` | none | PanelInfo |
| `linc_sim_load_program` | `path` | CommandResult |
| `linc_sim_issue_mdi` | `block`, `origin?` | CommandResult |
| `linc_sim_run` | `line?` | CommandResult |
| `linc_sim_pause` | none | CommandResult |
| `linc_sim_resume` | none | CommandResult |
| `linc_sim_stop` | none | CommandResult |
| `linc_sim_command` | typed `MachineCommand` | CommandResult |

Fault kinds:

- `crc_error`
- `timeout`
- `disconnect`
- `stale`
- `invalid_mode`
- `protect_unknown`
- `nml_lost`
- `snapshot_lost`

`linc_sim_command` schema mirrors `MachineCommand`; reject `SetHalOutput` except
for `LincOutput` values if needed for HAL-output tests. Never accept arbitrary
pin names.

### 9.10 UI control and test harness

| Tool | Input | Returns |
|---|---|---|
| `linc_ui_navigate` | `page` | UiSnapshot |
| `linc_ui_send_key` | `key`, `modifiers?` | UiSnapshot |
| `linc_ui_set_focus` | `region?`, `widget_id?` | UiSnapshot |
| `linc_test_wait_for` | `condition`, `timeout_ms?` | WaitResult |
| `linc_test_assert` | `condition` | AssertionResult |
| `linc_test_capture` | `name?`, `screenshot?` | CaptureResult |

Conditions use an allowlisted path + operator grammar:

```json
{ "path": "task.interp", "op": "eq", "value": "idle" }
{ "path": "panel.mode", "op": "eq", "value": "AUTO" }
{ "path": "machine.estop", "op": "eq", "value": false }
{ "path": "program.current_line", "op": "gte", "value": 10 }
{ "path": "alarms.length", "op": "eq", "value": 0 }
```

Operators: `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `contains`, `matches_glob`.
No arbitrary JSONPath, regex injection or code evaluation.

UI control is simulator-only unless explicitly enabled for a test profile. The
real machine UI must not accept synthetic keyboard input from MCP.

### 9.11 Key tool schemas

#### `linc_get_snapshot`

```json
{
  "sections": ["task", "machine", "panel", "program", "alarms"],
  "include_trail": false,
  "trail_points": 200,
  "include_program_content": false,
  "max_bytes": 1000000
}
```

#### `linc_why_blocked`

```json
{
  "action": "run_from_line",
  "context": { "line": 42 }
}
```

#### `linc_get_panel_frames`

```json
{
  "since_sequence": 0,
  "direction": "rx",
  "errors_only": false,
  "limit": 100
}
```

#### `linc_sim_set_panel`

```json
{
  "connected": true,
  "mode": "MDI",
  "feed_override": 1.0,
  "rapid_override": 1.0,
  "spindle_override": 1.0,
  "program_protect": false,
  "estop": false,
  "power": true,
  "buttons": 0,
  "leds": 0,
  "sseg_tool": 3,
  "sseg_next_tool": 0,
  "sseg_status": 54
}
```

#### `linc_export_diagnostics_bundle`

```json
{
  "name": "mdi-rejected-2026-10-22",
  "include_screenshot": true,
  "include_program": true,
  "redact": "paths"
}
```

---

## 10. Resources

| URI | MIME | Content |
|---|---|---|
| `linc://build` | `application/json` | BuildInfo |
| `linc://environment` | `application/json` | EnvironmentInfo |
| `linc://snapshot/current` | `application/json` | latest snapshot |
| `linc://snapshot/{sequence}` | `application/json` | historical snapshot |
| `linc://events` | `application/json` | recent events |
| `linc://alarms` | `application/json` | active alarms |
| `linc://logs` | `text/plain` | recent logs |
| `linc://panel` | `application/json` | panel state |
| `linc://panel/frames` | `application/json` | recent frames |
| `linc://nml` | `application/json` | NML/bridge state |
| `linc://hal` | `application/json` | HAL state |
| `linc://program/current` | `text/plain` | loaded program |
| `linc://program/preview` | `application/json` | preview metadata |
| `linc://profile` | `text/plain` | merged profile TOML |
| `linc://ini` | `application/json` | parsed INI |
| `linc://docs/spec` | `text/markdown` | SPEC.md |
| `linc://docs/config` | `text/markdown` | CONFIGURATION.md |
| `linc://help/gcode/{anchor}` | `application/json` | help section |
| `linc://screenshot/current` | `image/png` | current screenshot |

Resource subscriptions: `linc://snapshot/current`, `linc://alarms`,
`linc://panel`, `linc://logs`.

Rules:

- Resources are read-only.
- Large resources are streamed or paginated by the client; do not return more
  than 1 MB by default.
- `linc://program/current` respects `include_program` policy; if disabled,
  return a resource link error, not partial silent content.
- `linc://docs/spec` and `linc://docs/config` are static files from the build.

---

## 11. Prompts

Prompts are workflow scaffolds. They must instruct the agent to call tools; they
must not embed machine state.

| Prompt | Arguments | Purpose |
|---|---|---|
| `diagnose_blocked_action` | `action`, `context?` | Run health check, why_blocked, timeline, explain |
| `diagnose_alarm` | `alarm_id?` | Alarm context, related events, panel/NML state |
| `diagnose_panel_link` | none | Panel link stats, frames, CRC/timeouts, reconnect |
| `diagnose_tool_change` | none | Tool state, token, panel, HAL, events |
| `diagnose_probe` | none | Probe state, result, offsets, program line |
| `diagnose_nml_loss` | none | NML ages, bridge stderr, restarts, snapshot |
| `preflight_program` | `path?` | Read-only program/tool/offset/travel checks |
| `review_config` | `profile?` | Validate profile, INI, HAL, allowed outputs |
| `reproduce_issue` | `description` | Build a sim scenario, capture state/screenshot/bundle |
| `summarize_shift` | `since_ms?` | Alarms, events, commands, utilization |
| `compare_builds` | `a`, `b` | Diff build/config/state capabilities |
| `ui_regression` | `page` | UI state, screen model, screenshot, layout checks |

Prompts must state the read-only default and must not suggest bypassing
interlocks.

---

## 12. Workflows

### 12.1 "Why won't MDI issue?"

1. `linc_health_check`
2. `linc_get_snapshot` with task, machine, panel, program
3. `linc_why_blocked` with `issue_mdi`
4. `linc_get_command_history` for recent rejections
5. `linc_get_timeline` around the rejection
6. `linc_get_panel` and `linc_get_nml`
7. Return exact failing check and value.

### 12.2 "Cycle start does nothing"

The HMI does not issue cycle start. The answer must identify the panel-owned path.

1. `linc_get_panel` — mode AUTO? connected? cycle start bit seen?
2. `linc_get_panel_frames` — request/response, CRC, timeout
3. `linc_get_snapshot` — task power, estop, homed, interp, program loaded
4. `linc_get_timeline` — button edge and state transitions
5. `linc_why_blocked` with `cycle_start` — authority `panel`; explain checks
6. `linc_get_nml` and `linc_get_hal`
7. Never suggest an on-screen cycle start.

### 12.3 "Tool change stuck"

1. `linc_get_tool_change_state`
2. `linc_get_panel` — current/next tool displays, buttons, mode
3. `linc_get_snapshot` — tool table, `iocontrol` request via HAL
4. `linc_get_command_history` — issue/confirm commands
5. `linc_get_timeline` — prompt, confirm, complete/fault
6. `linc_why_blocked` with `confirm_manual_tool_change`
7. `linc_get_hal_pins` filter `iocontrol` and `linc.tool-change-confirm`

### 12.4 "Probe result wrong"

1. `linc_get_probe_state` with history
2. `linc_get_offsets`
3. `linc_get_program_line` around the probe call
4. `linc_get_preview` and `linc_get_ui_state` for parameter draft
5. `linc_get_command_history` for the issued block
6. `linc_get_timeline` around the result
7. `linc_why_blocked` with `apply_probe_result`

### 12.5 "Panel link flaky"

1. `linc_get_panel_link`
2. `linc_get_panel_frames` errors only
3. `linc_get_timeline` panel events
4. `linc_health_check`
5. `linc_get_nml` to separate panel from controller
6. Do not open the serial port from `linc-mcp`.

### 12.6 "Following error / stall"

1. `linc_get_motion`
2. `linc_get_motion_history` around the event
3. `linc_get_snapshot` task/program/feed/spindle
4. `linc_get_hal_pins` filter servo/axis signals
5. `linc_get_timeline` alarms and mode changes
6. `linc_get_program_line` at `motion_line`

### 12.7 "UI layout bug"

1. `linc_get_ui_state`
2. `linc_get_screen_model` for the page
3. `linc_capture_screenshot`
4. `linc_get_build_info` and viewport/DPI
5. `linc_get_timeline` UI events
6. Compare against SPEC layout rules.

### 12.8 "Reproduce a bug in sim"

1. `linc_sim_reset`
2. `linc_sim_set_panel` release E-stop, power on, set mode
3. `linc_sim_load_program`
4. `linc_sim_issue_mdi` or `linc_sim_run`
5. `linc_test_wait_for` state transition
6. `linc_test_capture` screenshot + snapshot + events
7. `linc_export_diagnostics_bundle`

Simulator starts with E-stop latched and power off. Tests must release/power on
explicitly.

---

## 13. Security and safety

### 13.1 Authority matrix

| Capability | Real machine | Simulator |
|---|---|---|
| Read snapshot/events/logs | yes | yes |
| Read program/config/tool table | yes | yes |
| Read panel link/frames | yes | yes |
| Read HAL pins/signals | yes | yes, synthetic |
| Screenshot | yes | yes |
| Export bundle | yes | yes |
| Set panel mode/overrides | no | yes |
| Press panel buttons | no | yes |
| Release E-stop / power on | no | yes |
| Load/run/MDI commands | no by default | yes |
| Write offsets/tool table | no by default | yes |
| Save program | no by default | yes |
| Arbitrary HAL/NML | never | never |
| Panel serial access | never | never |

### 13.2 Real-machine writes

Recommended: do not implement real-machine writes in the first version.

If the owner later requires them, use a separate, explicit path:

- `--allow-machine-write` plus profile `machine.kind == "real"`.
- Only existing `MachineCommand` variants, never panel-only actions.
- Same `gates::evaluate` as the backend.
- UI modal confirmation on the machine:
  - command summary
  - target values
  - expiry (30 s)
  - Confirm / Deny
- MCP returns `pending_confirmation` with `confirmation_id`.
- Poll `linc_get_confirmation`; timeout returns denied.
- Audit every request/decision/execution.
- Rate limit: one pending write per client, max 10 per minute.
- Never expose `ToggleLimitsOverride`, `SetHalOutput`, `ClearTrail`? `ClearTrail`
  is view-only and may be allowed; `ToggleLimitsOverride` changes safety
  behavior and should stay UI/panel-only.
- Never allow writes while a program is running.
- Never run a write if the snapshot is stale.

### 13.3 Prompt injection

Untrusted text includes:

- G-code comments
- filenames
- alarm/message text
- log lines
- tool descriptions
- program names
- panel labels
- MCP client-supplied prompt text

Rules:

- Tool descriptions are static; never interpolate untrusted text into them.
- Tool results are data, not instructions.
- Prompts tell the agent to treat file/log content as untrusted.
- No tool auto-executes based on content.
- No write tool is reachable from a read-only prompt.
- Keep the write tools in a separate namespace and require explicit policy.

### 13.4 File access

Allowed roots:

- `NC_FILES` (`profile.nc_files_root()`)
- USB mount root from Files UI
- profile/INI/HAL files
- log/crash/export directories

Rules:

- canonicalize
- reject symlinks
- reject `..` escapes
- reject non-regular files
- size cap
- extension allowlist for G-code
- no arbitrary home directory browsing
- no `/proc`, `/sys`, `/dev`, `/etc` except explicit config files

### 13.5 Audit

Write `mcp-audit.jsonl` under the LiNC state directory:

```json
{
  "at_ms": 1760000000000,
  "client": "opencode/1.0",
  "tool": "linc_why_blocked",
  "args_sha256": "...",
  "args_preview": {"action": "issue_mdi"},
  "result": "ok",
  "duration_ms": 4,
  "request_id": "abc",
  "confirmation_id": null,
  "schema_version": 1
}
```

Rules:

- mode `0600`
- rotate at 10 MB, keep 5
- never log program content or full toolpath
- log write confirmations and denials
- expose only the tail via MCP if explicitly requested

### 13.6 Transport security

- Unix socket, same uid only.
- No TCP by default.
- If Streamable HTTP is added:
  - bind `127.0.0.1` only
  - bearer token file `0600`
  - no wildcard CORS
  - no TLS termination in LiNC; use a local reverse proxy if remote
  - still no real-machine writes by default
- Do not expose the diagnostic socket to other users.

---

## 14. Implementation plan

### Phase 0 — shared types and gates

Deliverables:

- `linc-core::gates` with `Action`, `GateContext`, `Check`, `GateReport`,
  `evaluate`.
- Refactor `NmlBackend` and `SimBackend` to call shared gates.
- `linc-core::diag` DTOs and ring buffer types.
- `UiSnapshot` and page-local snapshot types.
- Golden JSON tests for DTOs.

Exit criteria:

- `cargo test --workspace` passes.
- Gate truth-table tests cover every action.
- Backend rejection and gate failure agree.

### Phase 1 — core instrumentation

Deliverables:

- `DiagTap` in `BackendHandle`.
- Event tap, command history, log ring, panic hook.
- Snapshot history and motion history.
- Build info.
- `linc-diag` socket protocol and discovery.
- `linc --diag-socket` and `--diag` flags.
- `linc` diagnostic server.

Exit criteria:

- External client can connect and read snapshot/events/logs.
- UI event drain is unchanged.
- No UI frame regression.
- Socket permissions and stale cleanup tested.

### Phase 2 — MCP read-only surface

Deliverables:

- `linc-mcp` binary with stdio transport.
- Tools for identity, state, diagnosis, config, program, offsets, tools, probe,
  panel, NML, HAL, motion, I/O, logs, events, timeline, diff.
- Resources and prompts.
- Audit.
- Read-only policy.

Exit criteria:

- Agent can answer "why blocked" with exact check.
- Agent can inspect all diagnostics without screenshots.
- MCP Inspector or test client passes.
- No write tools reachable.

### Phase 3 — panel diagnostics and simulator control

Status: implemented, with the deviations listed below.

Deliverables:

- `linc-panel` diagnostic socket.
- Raw frame ring and panel events.
- Sim control: mode, buttons, overrides, protect, E-stop, power, faults.
- `linc_compare_panel_views`.
- Sim test harness tools.

Exit criteria:

- Agent can reproduce panel link faults in sim.
- Agent can press buttons and observe state transitions.
- No real panel writes.
- `linc-mcp` never opens `/dev/ttyUSB0`.

Implemented surface:

- `linc-panel` publishes `panel.sock` next to the HMI socket and answers
  `diag.panel`, `diag.panel_frames`, `diag.panel_events` and `diag.metrics`.
  The frame ring holds every RS-485 request, reply and LED write with its CRC
  verdict; the event ring holds one record per changed field (link, fault,
  mode, E-stop, power, protect, overrides, button mask).
- The simulator adds `diag.sim_state`, `diag.sim_set`, `diag.sim_buttons`,
  `diag.sim_fault`, `diag.sim_clear_faults` and `diag.sim_reset` only when the
  panel service is started with `--allow-sim-control`. A serial panel has no
  control surface at all, so there is nothing for a tool to write.
- `linc-mcp --allow-sim-control --sim` exposes `linc_sim_*`; both ends must
  agree, the tools are annotated as writes, and a refusal is audited as
  `refused:WRITE_DISABLED`.
- `linc_get_panel_events` reads the transition ring, `linc_compare_panel_views`
  diffs the panel's own snapshot against the HMI's copy, and
  `linc_test_wait_for` polls one allowlisted path (`panel.*`, `sim.*`,
  `machine.*`, `task.*`, `program.*`, `alarms.length`, `ui.page`).

Deviations from the table in §9.9:

- The simulator records no raw frames: it has no bytes to record.
  `linc_get_panel_frames` returns the RS-485 traffic and an empty page under
  simulation. Link faults are injected through the simulator state instead.
- `nml_lost` and `snapshot_lost` are not panel faults: the panel service does
  not own the bridge or the HMI's snapshot. `disconnect`, `timeout`,
  `crc_error`, `invalid_mode`, `protect_unknown` and `stale` are available.
- `linc_sim_load_program`, `linc_sim_issue_mdi`, `linc_sim_run`/`pause`/
  `resume`/`stop` and `linc_sim_command` are not exposed: they move the
  machine, so they belong to the guarded-write work (§13.2), not to panel
  diagnostics. The simulator's own controls are the only writes.

### Phase 4 — deep debug and export

Status: implemented, with the deviations listed below.

Deliverables:

- UI state publisher and screenshot.
- Screen model tool.
- Diagnostic bundle.
- Crash records.
- Metrics.
- Motion history tools.
- HAL read-only introspection.
- NML bridge details.

Exit criteria:

- Agent can capture UI state and screenshot.
- Bundle reproduces a fault offline.
- HAL pin values match `halcmd` in a live session.
- Bundle redaction tested.

Implemented surface:

- `UiSnapshot` now carries `viewport` (points, pixels per point, fullscreen),
  `frame_time_ms` and `regions` (the shell's top bar, nav, content and bottom
  bar, in points). `linc_get_screen_model` returns them; the screenshot path
  already existed and is part of the same tool group.
- Motion history: the bus samples `MotionSample` at 10 Hz into a 1200-sample
  ring (120 s) and `linc_get_motion_history` returns a page, evenly downsampled
  to `max_samples`, with positions per axis (work/machine/dtg/velocity),
  commanded/actual/following error/servo load, spindle speed and load,
  overrides, task mode, interpreter, program and motion line, E-stop, power and
  panel mode. Eviction increments `dropped.motion`.
- Crash records: `linc`, `linc-panel` and `linc-mcp` install a panic hook that
  writes `$XDG_STATE_HOME/linc/crash-<role>-<pid>.json` (message, location,
  capped backtrace, build, pid). `linc_get_crash_records` reads the directory
  directly, so a dead HMI still yields its record; the bundle includes them.
- Bundle: 13 files now (`panel.json`, `ui.json`, `motion.json`,
  `crashes.json` added). Every string value is rewritten so the operator's home
  path becomes `~`; a unit test covers the redaction.
- NML: `NmlBackend` publishes `BridgeInfo` in the snapshot (kind `nml`, pid,
  alive, request/reply/error counters, last and maximum latency, last
  request/reply, fatal reason). `linc_get_nml` returns it plus command/status
  connection state, ages and echo serial. The simulator answers
  `available: false` with a reason. `restarts` is always `0`: the in-process
  client is not restarted in place. There is no stderr tail because there is no
  helper process.
- HAL: `linc_get_hal` reports the `linc.*` outputs, the heartbeat and the
  configured component; `linc_get_hal_pins` and `linc_get_hal_signals` answer
  `available: false` with the reason that `linc-hal` resolves only five symbols
  and does not link the full HAL C API. They never fabricate pins.
  `linc_get_io` reads coolant, lube, door, limits, homing and following error
  from the snapshot.
- Metrics now include the retained history and motion counts.

Deviations:

- `linc_ui_navigate`, `linc_ui_send_key`, `linc_ui_set_focus`,
  `linc_test_assert` and `linc_test_capture` from §9.10 are not exposed.
  Synthetic keyboard or focus input must not exist in a real-machine HMI, and
  the rest is covered by `linc_ui_action`, `linc_test_wait_for`,
  `linc_get_screen_model` and `linc_screenshot`.
- Real HAL pin/signal enumeration is not available (see above); the exit
  criterion "HAL pin values match halcmd" is met only for the `linc.*` outputs,
  which are the pins this process owns.
- `linc_get_nml` cannot show a live bridge in a build without LinuxCNC; it is
  exercised by the simulator answer and by unit tests.

### Phase 5 — optional guarded real writes

Only if explicitly approved.

Deliverables:

- Confirmation flow.
- `--allow-machine-write`.
- Audit and rate limit.
- Tests proving no panel-only actions and no bypass.

Exit criteria:

- Operator confirmation required for every write.
- Deny/timeout works.
- No write possible with stale snapshot.
- Code review by machine owner.

---

## 15. File map

| File | Change |
|---|---|
| `crates/linc-core/src/gates.rs` | New shared gate evaluator |
| `crates/linc-core/src/diag.rs` | New DTOs, rings, event/log records |
| `crates/linc-core/src/backend.rs` | Add `DiagTap`, command timing, event mirroring |
| `crates/linc-core/src/model.rs` | Add panel sequence/timestamps if missing |
| `crates/linc-core/src/command.rs` | Add diagnostic command record fields if needed |
| `crates/linc-core/src/config.rs` | Add diagnostic config keys, `Serialize` where needed |
| `crates/linc-core/src/ini.rs` | Add `Serialize` or INI DTO |
| `crates/linc-core/src/lib.rs` | Export new modules |
| `crates/linc-diag/Cargo.toml` | New crate |
| `crates/linc-diag/src/lib.rs` | Client/server, protocol, discovery |
| `crates/linc-diag/src/envelope.rs` | Envelope, errors, pagination |
| `crates/linc-diag/src/socket.rs` | Framing, peer credentials, limits |
| `crates/linc-mcp/Cargo.toml` | New crate/binary |
| `crates/linc-mcp/src/main.rs` | CLI, transport, server |
| `crates/linc-mcp/src/tools/*.rs` | Tool implementations by group |
| `crates/linc-mcp/src/resources.rs` | Resource registry |
| `crates/linc-mcp/src/prompts.rs` | Prompt registry |
| `crates/linc-mcp/src/audit.rs` | Audit log |
| `crates/linc-mcp/src/bundle.rs` | Diagnostic bundle |
| `crates/linc-mcp/src/policy.rs` | Read-only/sim/write policy |
| `crates/linc-app/src/main.rs` | CLI flags, log tap, diag server, UI publisher |
| `crates/linc-app/src/diag.rs` | Start/stop diagnostic services |
| `crates/linc-ui/src/shell.rs` | Publish UI snapshot, screenshot/action channels |
| `crates/linc-ui/src/state.rs` | UI snapshot helpers |
| `crates/linc-ui/src/screens/*.rs` | Page-local snapshot extraction where needed |
| `crates/linc-panel/src/main.rs` | Start panel diagnostic server |
| `crates/linc-panel/src/serial.rs` | Frame ring instrumentation |
| `crates/linc-panel/src/sim.rs` | Sim control/fault injection |
| `crates/linc-panel/src/diag.rs` | Panel diag server |
| `crates/linc-linuxcnc/src/lib.rs` | Bridge diagnostics, stderr tail, restart count |
| `crates/linc-hal/src/lib.rs` | Read-only HAL snapshot/pin enumeration |
| `Cargo.toml` | Workspace members |
| `README.md` | MCP usage |
| `CONFIGURATION.md` | `[diagnostics]` keys |
| `SPEC.md` | Diagnostics/MCP section |
| `AGENTS.md` | Crate map, commands, traps |

Do not add MCP dependencies to `linc-ui` or `linc-core`.

---

## 16. Testing and acceptance

### Unit tests

- Gate truth table for every action and every failure code.
- Backend rejection matches gate `machine_error`.
- Ring buffer ordering, capacity, drop counters.
- Pagination cursors.
- Snapshot diff ignores volatile fields.
- Event timeline ordering.
- Path canonicalization and symlink rejection.
- Redaction.
- Build info.
- DTO golden JSON.
- MCP tool input schema rejects unknown fields.
- Error mapping.

### Integration tests

- Fake backend with deterministic snapshots; call every MCP tool.
- Sim end-to-end:
  - start `linc-panel --sim --no-hal --window`
  - start `linc --sim --no-hal`
  - release E-stop
  - power on
  - set mode
  - load program
  - run
  - query why_blocked, motion, timeline
  - capture screenshot
  - export bundle
- Panel fault injection:
  - CRC error
  - timeout
  - disconnect/reconnect
  - invalid mode
  - protect unknown
- NML/snapshot loss:
  - bridge exits
  - status stale
  - snapshot lost
- Command failures:
  - revision conflict
  - unsafe path
  - wrong mode
  - not homed
- UI:
  - page navigation
  - modal open/close
  - dirty editor
  - screenshot
- Multi-client:
  - two `linc-mcp` instances
  - subscriptions
  - cancellation
- Security:
  - other uid rejected
  - path escape rejected
  - oversized frame rejected
  - arbitrary HAL/NML tool absent
  - write tools absent/read-only
  - prompt injection corpus does not execute writes
- Performance:
  - 8-hour sim soak
  - memory stable
  - no UI frame over 16 ms caused by diagnostics
  - event drops only under intentional overload
- Real machine:
  - read-only smoke
  - `linc_get_nml` matches `linuxcnc` status
  - `linc_get_hal_pins` matches `halcmd -s show pin`
  - no writes issued
  - `linc-mcp` never opens serial

### Acceptance script

`scripts/mcp-smoke.py` runs the two phases against the real binaries: phase 3
starts a headless `linc-panel --allow-sim-control` and a fake HMI socket, then
presses, patches, faults, compares and reads events through `linc-mcp`; phase 4
starts panel + HMI under Xvfb and checks the screen model, motion history,
NML/HAL/IO, crash records, a thirteen-file redacted bundle and a screenshot. It
exits non-zero on any failed check and is the first thing to run after touching
the socket or the UI publisher.

### Definition of done

- [ ] All read-only tools implemented and documented.
- [ ] `why_blocked` uses shared gates, not a copy.
- [ ] Snapshot, event, log, command, motion and panel rings working.
- [ ] UI state and screenshot working.
- [ ] Sim control and fault injection working.
- [ ] Diagnostic bundle reproducible.
- [ ] Audit log working.
- [ ] Local socket permissions and peer uid check tested.
- [ ] Real machine read-only smoke passed.
- [ ] `cargo fmt --check`
- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `linc --check-config --config configs/sim.toml`
- [ ] `linc --check-config --config configs/baiermill-pb.toml` (when siblings exist);
      `configs/baiermill.toml` is expected to fail until the machine tree carries the probe bundle
- [ ] README, CONFIGURATION, SPEC and AGENTS updated.

---

## 17. Known traps

1. **Do not call `BackendHandle::drain_events` from MCP.** It steals UI events.
   Add a tap.
2. **Do not serialize on the backend worker.** Clone and publish; serialize on
   the diagnostic thread.
3. **Do not hold the snapshot mutex while serializing.** Clone first.
4. **Do not open a second NML status channel.** Treat it as single-consumer
   unless proven otherwise. Use the backend snapshot.
5. **Do not open `/dev/ttyUSB0` from `linc-mcp`.** The panel service owns it.
6. **Do not create a second `linc` HAL component.** HAL ownership stays in the
   GUI/panel processes.
7. **Do not add arbitrary HAL writes.** Only `LincOutput` values, and only in
   sim if exposed.
8. **Do not put MCP on the UI thread.** `LincApp` is not `Send`; use a publisher
   and channels.
9. **Do not print logs to stdout in `linc-mcp`.** stdout is MCP transport.
10. **Do not trust `MachineSnapshot.meta.stale` clearing.** Preserve the
    backend's stale flag.
11. **Do not treat stale protect as off.** `protect_effective()` fails closed.
12. **Do not treat zero or multiple panel modes as valid.** Fail closed.
13. **Do not serialize full `live_trail` or `program.content` by default.**
14. **Sim panel starts E-stop latched and power off.** Tests must release and
    power on.
15. **`scripts/start-sim.sh` clears global LinuxCNC SysV IPC.** Coordinate
    before running it; do not run it while another session is active.
16. **HAL pin browser in the UI is synthetic.** Add real introspection; do not
    mislabel snapshot values as live HAL pins.
17. **The native NML client is linked at build time.** Rebuild after changing
    `crates/linc-linuxcnc` or the sibling `linuxcnc-nml` crate; a cross build
    needs target-side LinuxCNC libraries.
18. **`MachineIni` and `Config` are not fully `Serialize`.** Add derives or DTOs;
    do not expose `Debug` output.
19. **`linc-ui` must not depend on `linc-diag` or `linc-mcp`.**
20. **Real-machine writes are not a debugging requirement.** Do not add them
    without an explicit owner decision.

---

## 18. Open decisions

1. **Transport:** stdio only, or also loopback Streamable HTTP? Recommended:
   stdio first; HTTP only if multiple clients or remote support are required.
2. **HAL introspection:** public `liblinuxcnchal` API, a tiny C shim, or a fixed
   `halcmd -s show pin` adapter? Recommended: try the public API; fall back to a
   fixed-argv `halcmd` adapter with strict parsing.
3. **Program content default:** excluded by default, explicit include. Confirm
   with the owner if proprietary programs are a concern.
4. **Log persistence:** ring only by default; optional file sink for long
   sessions. Confirm disk budget.
5. **Screenshot storage:** on-demand only; no continuous capture. Confirm if
   screenshots on alarm are wanted.
6. **Real-machine writes:** disabled. Confirm if a guarded path is ever needed.
7. **Multi-instance:** support at least two sim instances for tests; real machine
   usually one. Confirm discovery UX.
8. **MCP client compatibility:** target tools/resources/prompts; make resource
   subscriptions optional.

---

## 19. References

- `SPEC.md` §3 (constraints), §4.3 (backend boundary), §6.5 (diagnostics),
  §8.2 (mode/command gating), §9 (data model)
- `CONFIGURATION.md` — profile keys, validation, hardware verification
- `AGENTS.md` — build/test commands, invariants, traps
- `crates/linc-core/src/model.rs` — `MachineSnapshot`, panel, tool, probe, ATC
- `crates/linc-core/src/backend.rs` — `BackendHandle`, worker, event queue
- `crates/linc-core/src/command.rs` — `MachineCommand`, `BackendEvent`
- `crates/linc-core/src/sim_ipc.rs` — atomic local JSON IPC pattern
- `crates/linc-linuxcnc/src/lib.rs` — Python bridge, NML ages, tool table
- `crates/linc-panel/src/serial.rs` — RS-485 frames, CRC, link stats
- `crates/linc-panel/src/sim.rs` — simulator panel state and buttons
- `crates/linc-hal/src/lib.rs` — typed HAL outputs, no arbitrary writes
- `crates/linc-ui/src/state.rs` — `LincApp`, UI state, page types
- `crates/linc-ui/src/screens/diagnostics.rs` — current diagnostics data
- `crates/linc-app/src/main.rs` — process startup, logging, backend selection
