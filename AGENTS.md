# LiNC — working notes for agents

Rust/egui HMI for LinuxCNC: operator panel service (`linc-panel`) plus the
display (`linc`). Design authority is `SPEC.md`; configuration keys are in
`CONFIGURATION.md`.

## Build and verify

```bash
cargo build --release          # start-sim.sh runs target/release, not debug
cargo test --workspace         # 107 tests
./target/release/linc --config configs/sim.toml --check-config
./target/release/linc --config configs/baiermill.toml --check-config
```

`scripts/start-sim.sh` uses the **release** binary and only rebuilds when it is
missing, so `cargo build` alone leaves it running stale code.

LinuxCNC cannot run in a sandbox without realtime privileges (`insmod` fails on
a read-only `/tmp`). HAL changes are therefore verified statically: every pin
must be linked exactly once, and `linc.*` nets belong in the post-GUI file
because the component only exists once the GUI has started.

## Screenshots

**Take screenshots with no banners.** Banners shift every page vertically, so a
capture taken with one is not a valid layout reference.

- Run `linc-panel` and let the HMI connect: an offline panel raises
  `PANEL OFFLINE` and hides machine state.
- **Release the E-stop on the panel first.** The simulation boots with the
  mushroom latched by design, so a capture straight after start has
  `EMERGENCY STOP ACTIVE` across the top.
- Use an isolated `[preferences] file` per capture, or the remembered page and
  window geometry from the last run decide what is on screen.
- Capture at 1280×720, the design baseline, with the window focused for key
  input. Without a window manager the pointer must be over the window.

## Layout rules learned the hard way

- **No page titles.** The navigation names the page; banners stay because they
  carry live state. Pages keep an invisible focus anchor so arrow-key navigation
  has a starting point.
- **Bound cards with an exact rect.** A `theme::card` inside a shared column
  grows to fit its content, so header + body + footer drift past the slot and
  the bottom is clipped. `allocate_exact_size` + `new_child` prevents it.
- **Measure, do not guess, chrome.** `graph_card`'s chrome is ~86 px and a
  MODAL card ~135 px; the cards report their own height back and the next frame
  uses it.
- `[ui] pages` selects the navigation entries and their order; it must match
  the keyboard (`MAIN` F5, `FILE` F6, `TOOL` F7, `OFFSET` F8, `WIN` F10).
- `hal.allowed_outputs` is validated for **exact** equality, so adding a
  `linc.*` output also means updating `configs/base.toml`.
