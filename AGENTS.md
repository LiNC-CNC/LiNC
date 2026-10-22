# AGENTS.md — LiNC working briefing

LiNC is a Rust/`egui` HMI for LinuxCNC. The physical CNC panel and QMK keyboard
drive the machine; the screen reads state and issues only explicit, validated
commands (MDI, probe, tool change, program/offset writes). Three binaries:

- `linc` (`crates/linc-app`) — display/UI; loads the profile, selects the backend, creates the `linc.*` outputs.
- `linc-panel` (`crates/linc-panel`) — panel service; owns RS-485 or the sim window, creates `cnc_panel.*`, and runs separately so a UI crash cannot remove panel control.
- `linc-mcp` (`crates/linc-mcp`) — MCP server for diagnostics; speaks the `linc-diag` socket protocol and never links the UI, HAL, LinuxCNC or the panel driver.

## Build, test, run

```bash
cargo build --release                       # all three binaries
cargo test --workspace                      # 162 tests today
./target/release/linc --config configs/sim.toml --check-config
./target/release/linc --config configs/baiermill.toml --check-config
scripts/build-deb.sh                        # Debian packages, amd64 + arm64
```

- `scripts/build-deb.sh` builds one `.deb` per architecture by default. arm64
  needs `rustup target add aarch64-unknown-linux-gnu` and
  `gcc-aarch64-linux-gnu`; the script names them when they are missing.
  `--stage` lays out the tree without `dpkg-deb` (useful where dpkg is absent),
  and `debian/` still supports a native `dpkg-buildpackage`.

- `--check-config` merges the profile, reads the INI, resolves paths and checks safety without a window or HAL; it fails if the INI or a `../../` sibling checkout is missing.
- `lathe-sim.toml` is expected to fail `--check-config` until its tool table has `P`; that is the turret guard.
- `./scripts/start-sim.sh` (`--detach`; stop with `./scripts/stop-sim.sh`) runs `linuxcnc configs/sim.ini` from `configs/`, exports
  `LINC_CONFIG=configs/sim.toml`, builds only when a release binary is missing, and needs `DISPLAY`/`WAYLAND_DISPLAY`.
- Standalone, no LinuxCNC or HAL. Panel and HMI must share `LINC_SIM_IPC`:

```bash
./scripts/start-panel-sim.sh
LINC_SIM_IPC=/tmp/linc/panel-sim ./target/release/linc --config configs/sim.toml --sim --no-hal
```

- Raw panel: `./target/release/linc-panel --config configs/sim.toml --sim --no-hal --window` (`--sim` forces sim, `--no-hal` skips `cnc_panel.*`).
- `--demo-mode <edit|auto|mdi|jog|inc|mpg|home|proghome>` (or `LINC_DEMO_MODE`) sets the starting mode; it does not replace a connected panel.
- `--backend linuxcnc` attaches to running LinuxCNC; `auto` uses sim for sim profiles. `scripts/linc-display.sh` is the `[DISPLAY]` entry.

## Crate map

- `linc-core` — profile/INI loader, model, commands, backend trait, G-code lexer/preview, editor, probe, tool table, preferences, sim backend. Leaf: third-party deps only.
- `linc-view` — toolpath projection/painting, sparklines. `egui` + `linc-core`.
- `linc-hal` — `dlopen`ed HAL FFI and typed `linc.*` component. `linc-core`.
- `linc-linuxcnc` — `MachineBackend` over a supervised `python3` helper (`python/linc_bridge.py`, embedded with `include_str!`). `linc-core`.
- `linc-panel` — `PanelDriver`, RS-485 protocol, sim panel, `cnc_panel.*` pins. `linc-core` + `linc-hal`.
- `linc-ui` — shell, keymap, theme, pages, files/USB. `linc-core` + `linc-view`; never HAL, LinuxCNC, serial or Python.
- `linc-diag` — Unix-socket diagnostic protocol: discovery, envelope, `BusSource` over `linc_core::diag`. Depends only on `linc-core`.
- `linc-mcp` — MCP adapter over `linc-diag`: tools, resources, prompts, audit, read-only policy. Depends only on `linc-core` and `linc-diag`.
- `linc-app` — `main`, CLI, backend selection, window. The only crate that wires `linc-ui`, `linc-hal` and `linc-linuxcnc` together, and the one that starts the diagnostic socket.

## Sources of truth

- `SPEC.md` — behaviour, layout, safety, acceptance (sections 3, 5–8, 10–13).
- `CONFIGURATION.md` — every profile key, merge rules, enums, validation.
- `configs/base.toml` — live defaults for keys, pages, allowed outputs, safety.
- The machine INI — authoritative for axes, travel, units, velocities, tool table, ATC and `PROGRAM_PREFIX`. Never duplicate those in TOML.
- Code and tests decide current behaviour when prose drifts. `LincOutput::ALLOWED` and `configs/base.toml` list five outputs (`hmi-alive`,
  `tool-change-confirm`, `status-display`, `jog-increment`, `jog-speed`), while SPEC §11, CONFIGURATION §8 and README still say three. Follow the code; fix the docs.

## Hard invariants

- `hal.allowed_outputs` must equal `LincOutput::ALLOWED` exactly, in order (`config.rs` compares the vectors). Adding an output means updating the enum,
  `ALLOWED`, `LincHal::new`, `configs/base.toml`, the post-GUI net and tests.
- Never add a generic HAL write. `SetHalOutput`/`HalWriter` take the closed `LincOutput` enum; arbitrary pin names are not representable.
- `linc.*` pins exist only after the GUI starts. Put every `linc.*` net in `hal/linc-postgui.hal` (`hal/linc-sim-postgui.hal` for sim), never
  `hal/linc.hal`. `linc-display.sh` waits for `linc.status-display`, then loads it.
- HAL pins take exactly one signal. `halui.mode.manual` and `halui.estop.reset` are single pins, so requests are ORed through `or2.manual_req` /
  `or2.estop_reset`; never attach a second `net`.
- `halui.estop.activate` belongs to the machine HAL (`cnc_panel.estop -> rio.sim-estop` in simulation). Do not link it in `hal/linc.hal`.
- `or2` is loaded once in `hal/linc.hal`; a second `loadrt or2` fails with "already exists". Add instances there.
- The panel service owns `/dev/ttyUSB0`. The HMI and `linc_bridge.py` must never open it (a test asserts the bridge has no `serialport`/`/dev/tty`).
- No on-screen mode, cycle, jog, spindle, coolant, override or software E-stop. The explicit exceptions are MDI, `RUN PROBE`, `CHANGE TOOL` and `RUN FROM LINE`.
- Program protect fails closed on unknown/stale and is rechecked on every write. Never discard a dirty buffer when protect turns on.
- Panel modes are steady one-hot; zero or several active modes decode to `Unknown` and the UI fails closed. Do not add pulse latching.
- `Ctrl+Q` is guarded: never quit while a program runs or the machine moves.
- Minimum window 1280×720; interactive targets at least 44 px.

## Verify UI work

- Run `cargo test --workspace` plus the affected `--check-config` profiles, then exercise the page in the standalone sim.
- LinuxCNC does not run in this sandbox (no realtime; `insmod` fails on a read-only `/tmp`). HAL changes are reviewed statically: read `net`/`loadrt`,
  confirm each input pin appears in exactly one signal, confirm no pre-GUI `linc.*` net (`grep -n '^net .*linc\.' hal/linc.hal` prints nothing), and
  confirm every output is wired post-GUI. The repo has no HAL linter; `halcmd` only works inside a live HAL.
- Banner-free means panel `connected=true`, E-stop released, valid mode, protect off. `PANEL OFFLINE`, `EMERGENCY STOP ACTIVE` and `PROTECT ON` shift every page.
- Capture recipe: build release; start `linc-panel --sim --no-hal --window` on a 1700×1060 Xvfb; release the E-stop (sim boots latched, power off); set
  the panel mode; start `linc --sim --no-hal` on a 1280×720 Xvfb with the same `LINC_SIM_IPC`; wait; `xdotool windowfocus`; keep the pointer over the
  window (no WM); `import -window "$WID"`.
- Give each capture an isolated `[preferences] file` and delete it first, or the remembered page/geometry decides the shot. Fixtures:
  `/home/jack/CNC/tmp/profiles/` with `/home/jack/CNC/tmp/prefs/{profile}.preferences.toml`; drivers `/home/jack/CNC/tmp/cap*.sh`; output `/home/jack/CNC/tmp/shots/`.

## UI conventions

- No page titles; the navigation names the page. Start each page with `common::focus_anchor(ui, "<page>-page-focus")` so arrow navigation has a
  starting point. Banners stay because they carry live state.
- Use `common::shell_card` + `common::card_title` for toolpath-style cards: exact rect, 4 px frame, `interact_size.y = 14`, `item_spacing.y = 0`,
  clip `rect.expand(1.5)`. `theme::card` (12 px margin) + `section_header` is a different, fatter shell; mixing them makes one widget look like two.
- Bound a card with `allocate_exact_size` + `new_child(max_rect(rect))` + `set_clip_rect`, and `set_min_size` so it fills the rect. Otherwise it ends at
  content height and the neighbouring column ends on another line.
- Do not size children from `ui.available_height()` inside a frame; a frame does not bound its content. Size from the card's rect.
- `ui.min_rect()` is not a measurement after `set_min_size`. Measure from `ui.cursor()` before and after the header.
- Never feed a measured height into the next frame's layout; measure and lay out from rects within the same frame, or the card oscillates between sizes.
- Measure text with `theme::measure(ui, text, base)` instead of hardcoding column widths. The tool table is the reference.
- Page-local keys go before widgets draw (`screens/files.rs`, `editor_pane` in `program.rs`), so a focused row cannot eat Enter or the arrows first.
- Test the page before `consume_key`. It swallows the key even when the rest of the condition is false; `consume_key(mdi_issue) && page == Mdi` ate
  plain Enter everywhere and broke opening a file from NC_FILES.
- A focused editor keeps Home/End/PgUp/PgDn. Check `memory.has_focus(egui::Id::unique("linc-program-editor"))` first, and keep that id stable.
- Use `theme` constants and the `SPEC.md §7` palette, not literals.

## Keyboard reality

Source: `../qmk_firmware/cnckeyboard/keyboards/cnckeyboard/keymaps/doubletap/keymap.c`.

- Present: `MAIN`, `FILE`, `TOOL`, `OFFSET`, `WIN` (F5, F6, F7, F8, F10), `ESC`, arrows, `PGUP`, `PGDN`, `HOME`, `END`, `ENTER`, `BACKSPACE`,
  letters, digits, punctuation, `CTRL`, and `MO(1)` (symbols, mouse buttons, `Ctrl+Q`). Letters arrive lowercase.
- Missing: F1–F4, F9, F11, F12, `Tab`, `Shift`, `Delete`. Provide a Ctrl fallback (Files accepts `Ctrl+X` as Delete) or a focusable action.
- Global keys navigate: F5 Machine, F6 Program (again → Files), F7 Tools, F8 Offsets, `Ctrl+Esc` Diagnostics, `Ctrl+Q` guarded quit, Esc,
  arrows/Page/Home/End, Enter. `[keys]` drives the shell; `Keymap::from_config` is the only source.
- Everything else is page-local and consumed only on that page: `Ctrl+S`, `Ctrl+Z/Y`, editor keys, `Ctrl+R` run-from-line, `Ctrl+0` fit, `Ctrl+L`
  clear trail, Files copy/delete, MDI history. A page-local key consumed elsewhere shadows another page's command.
- The sidebar SHORTCUTS pane lists page-local commands only; navigation keys are never listed. The bottom bar carries live machine state only.
- `Ctrl+F` is a dead binding: `[keys].find` exists and Settings advertises it, but no find UI is implemented. Do not add one without changing SPEC §6.2.
- `[ui] pages` selects and orders nav entries but does not unbind keys. If you hide a page, decide what its `[keys]` binding does. The Diagnostics
  keycap reads `WIN` while its binding is `Ctrl+Esc`; `[keys]` is authoritative.

## Traps

- **Release vs debug.** `start-sim.sh` and the capture scripts run `target/release/…`; `cargo build` alone updates `target/debug` and leaves the
  running release binary stale. Use `cargo build --release`.
- **Stale fixture copy.** `/home/jack/CNC/tmp/profiles/base.toml` copies `configs/base.toml` for screenshot profiles. Edit `configs/base.toml`, then
  refresh the copy; editing only the fixture hides the change.
- **Embedded bridge.** `python/linc_bridge.py` is compiled in via `include_str!`; rebuild after editing it.
- **Standalone IPC.** With `--sim --no-hal`, panel and HMI only see each other through `LINC_SIM_IPC` JSON files; different paths give a
  `PANEL OFFLINE` banner, not an error dialog.
- **Profile selection.** `--config` or `LINC_CONFIG` is mandatory; there is no default. Under LinuxCNC they must match the `[DISPLAY]` line, and
  `-ini` overrides `machine.linuxcnc_ini`.
- **Sibling checkouts.** `sim.toml` is self-contained (`configs/` + `hal/`), but `baiermill.toml` and `lathe-sim.toml` point outside the repo
  (`../../baiermill_pb`, `../../probe_basic_dev`); `--check-config` fails without them.
- **egui 0.37.** The workspace pins `egui`/`eframe`/`egui_extras` 0.37. Copy API style from `theme.rs`/`shell.rs`; older egui examples may not
  compile, and a second egui version breaks type compatibility.
