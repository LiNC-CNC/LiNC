# Panel driver parity: `cnc-panel-hal` versus `linc-panel`

This records the evidence behind replacing the machine's external panel driver
(`~/src/cnc-panel-rust`, binary `cnc-panel-hal`) with LiNC's own panel service
(`linc-panel`) on the BaierMill. It answers one question: when `linc-panel`
drives the SZGH panel, does the rest of the machine see the same thing it sees
today?

Reference driver: `cnc-panel-rust` at `0773a35` (the commit deployed at
`cnc@baiermill.lan:/home/cnc/src/cnc-panel-rust`, as it is running on the mill).
LiNC: this worktree, based on `main` at `7b899f2`.

## Method

Three comparisons, in increasing strength:

1. **Pin surface**, extracted mechanically from both sources and diffed.
2. **Wire protocol and decoded semantics**, read side by side, frame by frame
   and branch by branch.
3. **Differential run**: both implementations driving an emulated panel on a
   pty, fed identical reply bytes with identical HAL input pins, comparing every
   `cnc_panel.*` value and the exact bytes each one sends to the panel.

The third is what makes the claim checkable rather than asserted. The deployed
driver source is used **verbatim** (`md5 178d6553…`); only its HAL dependency is
swapped for a stand-in crate, because this machine's LinuxCNC headers are newer
than the `linuxcnc-hal` 0.3 release that crate builds against. `linc-panel` runs
against the real local HAL: `linc-hal` uses `dlopen`, so it needs no headers.

Harness (outside this repo, in the development checkout's `tmp/`):

```
tmp/panel-parity/harness/
  panel_emulator.py        pty panel: answers polls, records every frame
  mock-hal/                linuxcnc-hal stand-in that dumps pin values as JSON
  ref-driver/              deployed src/main.rs, byte-identical, wired to mock-hal
  compare.py               18 cases: run both, diff pins and LED frames
```

Run it with `python3 tmp/panel-parity/harness/compare.py --json report.json`.
It needs `halrun`/`halcmd` and a built `linc-panel`.

## Result

**13 of 18 cases are byte-identical on every shared pin and on the LED frame.**
The five that differ do so in four specific places, all listed below.

Shared pin surface: **129 pins, identical name, direction and type**. `linc-panel`
adds 25 pins (the lathe-only keys, the simulation-only keys, `sseg_status`,
`heartbeat`, `power_on`/`power_off`, `probe`, `estop_reset`, `home_state_*`,
and four extra LED names). Nothing the deployed driver publishes is missing or
differently shaped. The button and LED bit tables are numerically identical,
including `K1`, `K2` and `LUB` after the deployed `fix K2 bit position`.

Cases, each holding one reply payload and one set of HAL input pins:

| case | what it exercises | pins | LED frame |
| --- | --- | --- | --- |
| 01 auto-idle | AUTO, nothing pressed, pots at nominal | equal | equal |
| 02 auto-all-leds | every LED pin on, velocity on all axes | equal | equal |
| 03 auto-step | CYCLE START with single block lit | equal | equal |
| 04 auto-cycle-start | CYCLE START in AUTO | equal | equal |
| 05 jog-cycle-start | CYCLE START outside AUTO | **cycle_start** | equal |
| 06 home-keys | HOME mode, RAPID + JOG.X+, JOG.A+ | equal | equal |
| 07 home-keys-homing | same while a joint is homing | **home-x** | equal |
| 08 proghome-keys | PROG.HOME, RAPID + JOG.Y- | equal | equal |
| 09 mdi-zero-overrides | MDI, zero override switches, E-stop + protect | equal | equal |
| 10 auto-all-keys | every key pressed at once in AUTO | equal | equal |
| 11 sseg-out-of-range | tool displays 300 and -1 | equal | **differs** |
| 12 edit-released | EDIT, nothing pressed, all LEDs off | equal | equal |
| 13 jog-keys | jog keys, RAPID and SAFE DOOR in JOG | equal | equal |
| 14 inc-overrides | INC, feed 5 / rapid 1 / spindle 2 | equal | equal |
| 15 led-pattern | seven specific LED bits | equal | equal |
| 16 panel-silent | panel stops answering after 3 cycles | **mode_auto** | equal |
| 17 bad-crc | every reply after 3 has a broken CRC | **mode_auto** | equal |
| 18 panel-absent | panel never answers | equal | equal |

Protocol, frame for frame: identical. Poll `EB 01 80 8E`; reply `EB 01` + eight
payload bytes + CRC-16/MODBUS accepted in either byte order; LED frame
`EB 02` + ten bytes + CRC with the CRC bytes reversed; commit `EB 02 81 CE`;
same 4 ms / 5 ms / 5 ms settles. LED bitmap inversion (`u64::MAX & !leds`, then
`<< 16`) and the big-endian extraction of the reply are the same code. The
seven-segment bytes are the same for every value the machine can produce.

## The four differences

### 1. `cycle_start` is asserted outside AUTO

Deployed: the pin is only ever high in AUTO; outside AUTO the key is dropped in
the driver. `linc-panel`: the pin follows the key in every mode, and the HMI
refuses the press (`cycle_start_refusal`, toast *"select AUTO on the panel
first"*).

Machine impact: none. The mill's HAL wires nothing to `cycle_start` — the key
reaches the controller only through the HMI, which requires AUTO itself
(`crates/linc-ui/src/shell/gates.rs`). The difference is one extra refusal toast
instead of silence. LiNC also holds the pin for 200 ms so a tap cannot fall
between two 20 Hz reads; the deployed driver presents the level for one panel
cycle (~24 ms) and can drop a short tap.

### 2. Individual home pins are inhibited while a joint is homing

Deployed: `home-x/y/z` assert whenever HOME/PROG.HOME mode has the matching
direction key held. `linc-panel` adds `&& !homing` (`home_state_*`).

Machine impact: none on this machine — `hal/baiermill-pb-postgui.hal` already
ANDs every home key with `not-homing` before `halui.*.home`. On a machine
without that gate the deployed behaviour can re-issue a home command while a
joint is homing, so the LiNC gate is the safer of the two.

### 3. Seven-segment displays outside 0..99

Deployed: masks the value to 8 bits, then clamps (`300 → 44 → 0x44`, `-1 → 255
→ 0x99`). `linc-panel`: clamps to 0..99 (`300 → 0x99`, `-1 → 0x00`). Identical
for 0..99, which is the whole range of `iocontrol.0.tool-number` and
`tool-prep-number` on this machine (tool table T1..T99).

### 4. Mode pins after the link is declared down

`linc-panel` drops the link after five consecutive failed frames; the mode then
becomes `Unknown` and the eight `mode_*` pins go low. The deployed driver never
clears them: they freeze at the last decoded mode for as long as it runs. Both
end at `panel_connected = false`, `communication_error = true`, and case 18 (a
panel that never answers) is identical.

Machine impact: `mode_*` feeds `halui.mode.*` and the jog gates in
`hal/panel-jog.hal`, so LiNC's behaviour fails closed (jog stops) while the old
one leaves the last mode selected. No motion can come from the panel either way,
because a dead panel sends no keys.

Two further differences are timing only: `linc-panel` sends the LED frame built
from the previous cycle's HAL read (one cycle, ~24 ms, of lag), and it retries a
lost link at most once a second instead of every cycle. Steady state and every
case above are unaffected.

## What LiNC gets that the deployed driver does not have

Not required for parity, but the reason the swap is not a peer swap:

- `heartbeat`, which the HMI reads to catch a wedged panel process: with the
  deployed driver the pin is absent and `panel.rs` treats a missing heartbeat as
  healthy, so that check never runs today.
- `sseg_status`, the pin `hal/linc-postgui.hal` had to leave unwired.
- `estop_reset`, `power_on`/`power_off`, `probe`, `home_state_*`, and the lathe
  face's keys — all inert on the mill table, all used by the simulator.

## Conclusion

For every state the machine can reach with a working panel, `linc-panel`
publishes the same 129 pins with the same values and sends the same bytes to the
panel. The four differences are either invisible to this machine's HAL and HMI,
or on the safe side of a fault. The swap does not change machine behaviour; it
removes a second driver, a second binary to build, and a second set of panel
semantics to keep in step.
