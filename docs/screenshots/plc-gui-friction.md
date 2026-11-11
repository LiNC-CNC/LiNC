# Ladder GUI friction, from driving the running app

Environment: Xvfb `:99` 1400x900, `DISPLAY=:99`, `start-sim.sh --detach --plc
--release-estop --power-on`, input with `xdotool`, screenshots with ImageMagick
`import -window root`. The session's own Wayland compositor is unusable for this:
`ydotool` absolute coordinates are in half the layout space and the active window
is covered by a fullscreen browser.

## Reproduced actions

| # | action | result |
|---|---|---|
| 1 | start the sim, screenshot | PLC page with launcher, "OPEN EDITOR", POU list, socket/file fields |
| 2 | click OPEN EDITOR | overlay opens on the POU list, menu bar File/POU/Edit/View/Run/Help |
| 3 | click the MAIN chip | EDIT page: the tree canvas of rung 1, then two grid rows with comment rows |
| 4 | press Right then `c` | nothing placed: the key went into the property panel's VAR field ("sealc") |

## Blockers found, in order

1. **A button kept keyboard focus, so every ladder key was ignored.**
   `editor_keys`, `il_keys` and `fbd_keys` all bailed out on "some widget is
   focused". After any click that is true forever, so the grid was dead with no
   indication. Fixed: only a focused *text field* takes the keys
   (`TextEditState::load` on the focused id).
2. **The property panel's VAR/COIL text field keeps focus and eats the element
   letters.** Pressing `C` on a selected rung typed into the field instead of
   placing a contact, and there is no way back to the ladder except closing and
   reopening the overlay. Not fixed yet: the panel needs an explicit
   "editing text" mode, with the ladder taking keys back on click or Escape.
3. **The GUI cannot author inputs.** The Symbols page's add row creates outputs
   only, and the daemon freezes its I/O map at startup, so a project needing new
   inputs cannot be built and loaded from the GUI. DEMO_LD needs I0.4-I0.7.
   Not fixed yet: needs "+ in" on the Symbols page and a service restart when
   the map changes.
4. The PLC service card draws its status line over the "PLC SERVICE" title, and
   the SOCKET/FILE fields truncate. Cosmetic, not blocking.

## Still to do

Build DEMO_LD's four rungs through the grid once 2 and 3 are fixed, then the
compare/compute POU, then run it and capture the simulation.

## Resolved in the ZX geometry pass

- **Sim page would not start.** Two causes, both fixed and logged: the validator
  counted a compute rung's unused coil as a write, so `Load` was refused with
  "`run` has more than one write coil" (`70074dc`), and the daemon serves a single
  client, so a second client saw `WouldBlock` — which is what several "no reply"
  runs actually were.
- **Sim page layout.** The ladder monitor has its own bounded scroll pane, so the
  I/O tiles are always on screen (`6987a64`).
- **Ladder looked like a table.** Now bare strips: no cell borders or backgrounds,
  zero spacing, one conductor, names above symbols, coil as two arcs, comments in
  the strip band, exact link pitch (`69c7ff2`, `d0b719c`).
- **Expression labels clipped.** Labels shrink to fit the cell instead of being cut,
  and the comment and operand baselines are separated inside a 40 px strip.
- **Expression values truncated.** `ExprEdit.text` holds the per-stage buffer, so a
  value typed across frames arrives whole (`7c27f9d`).
- **Chords applied twice / modifier mismatch.** `ctrl_key` consumes `CTRL` and
  `CTRL|COMMAND` once each (`677a6cf`, `e034edf`).

## Verified behaviours (screenshots in this directory)

Pump seal-in (`plc-sim-pump`), seal holding after release (`plc-sim-seal-in`),
estop dropping the run (`plc-sim-estop`), the 5 s fan run-on (`plc-sim-fan-runon`),
alert set and reset (`plc-sim-alert-set`, `plc-sim-alert-reset`), blinkers
alternating (`plc-sim-blink-a`, `plc-sim-blink-b`), and live symbol values with
`lamp` false at `MW1 = 0` (`plc-sim-live-values`).

## Genuine known limitations

- **One client at a time.** The daemon accepts a single connection; a second client
  blocks. Worth a queue or a clear refusal message.
- **Words are not in the Symbols table.** Only boolean symbols show live values; the
  `MW` bank and block members are visible in the snapshot and the watch list, not in
  that table.
- **Coil glyph** is a pair of arcs drawn as polylines; it reads correctly at this
  size but is not the exact IEC curve.
- **Menu clicks** still do not open dropdowns in the workbench; the keyboard paths
  (View/Run with Down+Enter, Ctrl+1..4) are the supported route.
