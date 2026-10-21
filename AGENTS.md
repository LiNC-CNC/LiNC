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
- **Bound cards with an exact rect, and force them to that size.** A
  `theme::card` inside a shared column grows to fit its content, so header +
  body + footer drift past the slot. `allocate_exact_size` + `new_child` +
  `set_clip_rect` bounds it, and the card must also be told to fill the rect
  (`set_min_size`) — otherwise it ends at its content height, a few pixels above
  the rect, and the column beside it ends on a different line.
- **`available_height()` inside a frame is not bounded by that frame.** Sizing a
  child from it lets the child ask for more than the card has; size from the
  card's rect instead.
- **`ui.min_rect()` is not a measurement of what you just drew.** Once
  `set_min_size` has run, `min_rect` holds the whole card, so using it as a
  header height hands the rest of the card a negative number. Measure from the
  cursor before and after the header.
- **Never feed a measured height back into the next frame's layout.** Measuring
  the chrome and passing it back next frame makes the card oscillate between
  two sizes. Lay out from rects and measure within the same frame.
- **Measure, do not guess, chrome.** `graph_card`'s chrome is ~86 px and a
  MODAL card ~135 px; the cards report their own height back and the next frame
  uses it.
- **Toolpath cards share one shell.** `theme::card` (12 px margin) plus
  `section_header` gives a visibly fatter title bar than the Machine page's
  toolpath card, which uses a 4 px `egui::Frame`, a plain label, and
  `interact_size.y = 14` / `item_spacing.y = 0`. Copy the shell, not just the
  contents, or the rows stop looking like the same widget.
- `[ui] pages` selects the navigation entries and their order; it must match
  the keyboard (`MAIN` F5, `FILE` F6, `TOOL` F7, `OFFSET` F8, `WIN` F10).
- `hal.allowed_outputs` is validated for **exact** equality, so adding a
  `linc.*` output also means updating `configs/base.toml`.
