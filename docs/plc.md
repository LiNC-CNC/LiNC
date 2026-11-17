# Ladder PLC module

`linc-plc` is a userspace PLC for LiNC. It compiles a ladder project, scans it
in its own process, owns its own `linc_plc.*` HAL component, and serves the
LiNC **PLC** page over a same-user Unix socket. It is not part of the `linc`
process and never touches the closed `linc.*` HMI surface.

## Enabling

The PLC is **off by default**. With it off the HMI starts no daemon, no
`linc_plc.*` pins exist, and `linc-display.sh` does not load the PLC HAL hook.
The PLC page stays reachable and works offline: it edits, validates and saves a
project with no daemon, and its service card says the PLC is disabled.

Three switches, highest first:

| switch | scope | effect |
| --- | --- | --- |
| `LINC_PLC=1` / `LINC_PLC=0` | one process | forces on / off, ignoring the profile |
| `start-sim.sh --plc` / `--no-plc` | one simulation run | exports `LINC_PLC=1` / `LINC_PLC=0` |
| `[plc] enabled = true` | the profile | on for every launch of that profile |

`LINC_PLC` accepts `1|true|on` and `0|false|off` in any case; anything else
leaves the profile in charge. `--plc` implies no other start-up step, and
`--no-plc` wins over a profile that enables the PLC.

```toml
[plc]
enabled = false        # off unless this or LINC_PLC says otherwise
project = "ladder.json" # ladder project, relative to the profile directory
socket = ""            # empty: $XDG_RUNTIME_DIR/linc/plc-<uid>.sock
scan_us = 2000         # scan period, 100..=1000000 microseconds
allow_force = false    # let the PLC page force I/O
```

`configs/base.toml` carries exactly that block; `configs/sim.toml` adds
`project = "plc-sim.json"` and leaves `enabled` false, so the simulation shows
the page offline until `start-sim.sh --plc` asks for the daemon.

Validation: `enabled = true` with an empty `project` is an error naming
`plc.project`, and `scan_us` outside 100..=1000000 is an error naming
`plc.scan_us`. Both are reported by `linc --check-config`.

When enabled, `linc` spawns `linc-plc` (from `LINC_PLC_BIN`, else the sibling
of its own executable) with `--project`, `--socket`, `--scan-us`,
`--allow-force` when set, and `--no-hal` when there is no LinuxCNC back end. It
writes the pid to `LINC_PLC_PID_FILE`, else `$XDG_RUNTIME_DIR/linc/plc.pid`,
else `/tmp/linc-plc.pid`. The HMI never waits for the daemon and never kills it:
it is a separate service, and `scripts/stop-sim.sh` stops the simulation's copy.
`scripts/linc-display.sh` loads `hal/linc-plc-postgui.hal` only after
`linc_plc.alive` appears.

## Safety boundary

This is not a safety PLC and not a safety function. E-stop, limits, guards,
door interlocks and STO belong in the machine's hardwired circuit and in a
certified safety device. `linc-plc` drives ordinary HAL outputs with no
safety rating, no redundancy and no independent watchdog. Wire
`linc_plc.alive` and `linc_plc.fault` into the machine's existing watchdog
or charge-pump chain, and treat a dead daemon as a machine fault.

## Build and run

```sh
cargo build --release -p linc-plc

# standalone, no LinuxCNC and no HAL pins
./target/release/linc-plc --project ladder.json --no-hal --allow-force

# inside the machine HAL
# loadusr -W linc-plc --project /home/operator/linc/ladder.json
```

The component and pin map are created before `hal_ready()`. Changing the
project's I/O bindings needs a daemon restart; the socket answers a mismatched
`Load` or `Reload` with error code `io_map_locked`.

There are two ways to hand the daemon a program:

- **Load** stops the ladder: it leaves the daemon `Loaded` and drives every
  output false, so a start press follows.
- **Reload** swaps the program at the top of a scan while the ladder keeps
  running. The phase is preserved (`Running` stays `Running`, `Loaded` stays
  `Loaded`), and the field outputs keep their last values until the new
  program's first scan writes them, so a running machine does not drop its
  outputs. Forces are re-applied by name, so a renumbered slot never inherits
  one; a force whose symbol the new program does not declare is dropped, and the
  log says how many. Retained values are restored through the usual store. From
  `Idle` or `Fault` a reload answers `state`: call `Load` instead.

Both accept logic-only changes; neither may change the I/O bindings. The
program is swapped at the top of a scan, never inside one, so no input decodes
against two programs.

Options: `--project` (env `LINC_PLC_PROJECT`), `--socket` (env
`LINC_PLC_SOCKET`, default `$XDG_RUNTIME_DIR/linc/plc-<uid>.sock`),
`--no-hal`, `--scan-us` (default 2000), `--allow-force`.

## Project format

Schema version 1. A project declares I/O bindings, internal variables and
sections of rungs. A rung is a `Logic` tree plus a coil:

```json
{
  "schema": 1,
  "name": "smoke",
  "description": "",
  "io": [
    { "name": "start", "direction": "in",  "kind": "bit" },
    { "name": "stop",  "direction": "in",  "kind": "bit" },
    { "name": "run",   "direction": "out", "kind": "bit" }
  ],
  "vars": [{ "name": "seal", "kind": "bit", "retain": false }],
  "sections": [{
    "name": "main",
    "rungs": [{
      "comment": "seal-in",
      "logic": { "op": "series", "items": [
        { "op": "parallel", "items": [
          { "op": "contact", "var": "start", "negated": false },
          { "op": "contact", "var": "seal",  "negated": false }
        ]},
        { "op": "contact", "var": "stop", "negated": true }
      ]},
      "coil": { "var": "seal", "kind": "normal" }
    }]
  }]
}
```

**A coil has a name, or it is not a coil.** IEC 61131-3's coil is an assignment:
`--( )--` writes the bit above it. A coil with no operand assigns nowhere, so
`validate()` refuses it as `coil_operand` — "a coil must name the variable it
writes" — and names the rung. The rule is what keeps a converted ladder honest: a
row that is the continuation of a `Link` above it carries no coil at all (the row
above holds the one the branch feeds), rather than a blank slot that draws as a
nameless `( )`. The importers and the `.clp` reader emit no coil where there is
nothing to write, `normalise()` clears a blank slot in an older file, and the
editor cannot create one — a new rung's coil slot is a bare conductor until it is
named. `Compute` coils (target in the cell) and `Jump`/`Call` coils (label or POU
name) are judged by their own rules.

Supported in this slice: NO/NC contacts, normal/set/reset coils, jump and call
coils, series/parallel branches, multiple sections, constants, a 32-bit word bank
with `word` and `float` I/O points, validation, local offline editing, download,
start/stop, live monitoring and forcing.

Not yet: SFC, Modbus, true hot
swap, and a servo-thread realtime component. Timers, counters, compare/compute
and a PLCopen XML import exist (see **Import**). The scan is userspace (1–5 ms target, soft
realtime); do not use it for encoder phasing or step timing.

## Import

`linc_plc_core::plcopen::import(xml)` reads a PLCopen TC6 / IEC 61131-10 file
(via `quick-xml`, MIT) and returns a `Project` with `schema: 1`. It is
read-only and there is no exporter.

What it maps:

| PLCopen | model |
|---|---|
| `<contentHeader name>` | `Project::name` |
| first `<pou pouType="program">` | `MAIN`; other POUs keep their names |
| `<LD>` body | `PouLanguage::Ld`, one section, one `Rung` per `<coil>` |
| `<contact negated edge="rising|falling">` | `Logic::Contact` / `Logic::Edge` |
| `<coil negated storage="none|set|reset">` | `Coil` with `Normal`/`Negated`/`Set`/`Reset` |
| `<block typeName="TON|TOF|TP|CTU|CTD">` | `Logic::Block`, `PT`/`PV` from an `<expression>` |
| compare block (`GT`…`LT`) | `Logic::Compare` from two `<expression>` inputs |
| `<localVars>` / `<inputVars>` / `<outputVars>`, BOOL | `VarDecl` / `IoPoint` |
| `<documentation>` | POU and rung comments, markup stripped |

Wiring is followed backwards from each coil: one `<connection>` is a series
step, several are `Logic::Parallel`, and the walk ends at the left power rail as
`Logic::Const { value: true }`. Imports become `Logic` trees; `Rung::cells` stays
empty and the existing compiler lowers the tree.

Names are lowercased to the model's `^[a-z][a-z0-9_-]{0,31}$` (PLCopen allows
capitalised identifiers), so `Motor` arrives as `motor`; a collision is a
`duplicate_symbol` error rather than a silent merge.

The import is **all-or-nothing**: any element with no faithful mapping fails the
whole file and no project is returned. Codes: `plcopen_xml` (malformed XML),
`plcopen_unsupported_body` (an `<SFC>`, `<ST>` or other non-LD body),
`plcopen_unsupported_element` (by name: jumps, labels, connectors, compute
blocks, latches fed by wires, non-BOOL variables), `plcopen_multi_coil` (a coil
feeding a coil), `plcopen_unreachable` (an element with no rail) and
`plcopen_cycle` (a connection loop). The result is then run through
`validate()`, so ordinary model rules such as `duplicate_coil` apply as well.

## .clp import (ClassicLadder)

`linc_plc_core::clp::import` reads a ClassicLadder project: a `.clp` container
(the plain-text concatenation of the project's parameter files) or an unpacked
project directory, which `LoadProjectFiles` accepts too. `clp::import_path`
picks the shape from the path. Import is read-only and all-or-nothing, like the
PLCopen one: a construct with no faithful LiNC equivalent returns diagnostics and
no project.

| ClassicLadder | model |
| --- | --- |
| `ELE_INPUT` / `ELE_INPUT_NOT` | `Cell::Contact`, NO / NC |
| `ELE_RISING_INPUT` / `ELE_FALLING_INPUT` | `Cell::Edge` |
| `ELE_CONNECTION` | `Cell::Wire` |
| `ELE_OUTPUT`, `_NOT`, `_SET`, `_RESET` | `Coil` normal / negated / set / reset |
| `ELE_OUTPUT_JUMP` | `CoilKind::Jump`, targeting the labelled rung its rung number names |
| `ELE_OUTPUT_CALL` | `CoilKind::Call`, naming the subroutine section's POU |
| `ELE_COMPAR` | `Cell::Expr` from `arithmetic_expressions.csv` |
| `ELE_OUTPUT_OPERATE` | `CoilKind::Compute` with `Coil.expr` |
| `ELE_TIMER_IEC` | `Cell::Block` TON / TOF / TP, preset from `timers_iec.csv` |
| `ELE_COUNTER` | `CTU` or `CTD`, per the wiring rules below |
| `%B`, `%I`, `%Q` | a variable, an input, an output |
| `%W`, `%IW`, `%QW`, `%IF`, `%QF` | `MWn`, a word or float point |
| `%TMn.Q`, `%TMn.P`, `%TMn.V` | `TONn.Q`, `TONn.PT`, `TONn.ET` |
| `%Cn.D`, `%Cn.E`, `%Cn.F`, `%Cn.P`, `%Cn.V` | `done` as a compare, `empty`/`full` refused, `CTUn.PV`, `CTUn.CV` |

`sections.csv` becomes one POU per section; a subroutine section is named after
its number, which a call coil names. Rung order is the `#NEXTRUNG` chain from
`FirstRung`, never the file numbering, and `#LABEL` becomes the label a jump
targets. `symbols.csv` renames variables through the same rule the PLCopen
importer uses, and a collision is refused rather than merged.

A counter is one element with four input rows and three output rows, so one rung
file can become several LiNC rungs: the counting block, the preset-load and
reset rungs in the order `CalcTypeCounter` applies them, then a rung for the
cells that read `done`. `done` is equality in ClassicLadder while `CTU.Q` is
`>=`, so it is synthesised as an equality compare against the counter's preset
and `Q` is never used. A counter rung has no coil, so it ends in a compute coil
that writes `CV` back to itself: the block's output is consumed and nothing
observable is written.

Refusals name the rung or member and the construct: `clp_container`,
`clp_member`, `clp_version`, `clp_rung_syntax`, `clp_indexed`,
`clp_counter_bidirectional`, `clp_counter_instances`, `clp_counter_reset`,
`clp_counter_overflow`, `clp_counter_preset_load`, `clp_legacy_timer`,
`clp_branch`, `clp_rung_chain`, `clp_sfc`, `clp_modbus`, `clp_expression`, plus
`clp_variable` (a variable the model cannot hold, such as `%W20` or an error
bit), `clp_timer_instances` (a fifth timer of one kind), `clp_timer_contact` (an
edge contact on a timer's done; plain and negated ones read `TONn.Q`) and
`clp_counter_edge` (an edge on a counter's done). `clp_counter_wrap` is the one
warning: a counter preset above 9,999, where ClassicLadder wraps and LiNC
saturates.

Not imported, by refusal: legacy timers and monostables, a branch other than one
link to the row below, a rung whose line is not drawn contiguously (a gap stops
ClassicLadder's power where LiNC's wire conducts), and a section named `MAIN`
(LiNC would treat every other POU as an uncalled routine).

## Scan semantics

The VM keeps two images. At scan start it copies `prev` to `next`, writes the
current inputs into both, and applies forces to both. Ops read `prev`; coils
write `next`. After the ops pass, forces are applied to `next` again, the
images swap, and outputs are read from the new `prev`. So:

- a contact sees the **current** input and the **previous** scan's coil value;
- set/reset coils hold across scans;
- a forced input affects logic on the same scan;
- a forced output is not overwritten by a coil;
- duplicate coil targets in one section are a compile error (no last-one-wins).

Word I/O follows the same rule. `scan_with_io` writes the word and float input
regions into the bank before the ops pass, so a rung reads this scan's field
value, and the daemon reads the output regions after it.

## Word bank and word I/O

The integer bank is 32 bits wide. `MW0..MW15` are the internal marker words;
after them come the project's word inputs, word outputs, float inputs and float
outputs, in that order. That order is the ABI: adding a point renumbers the
slots after it. A point declared `"kind": "word"` becomes an `s32` HAL pin, and
one declared `"kind": "float"` becomes a `float` pin. The project carries at
most 256 bank slots; a larger bank is refused with `too_large`.

Consequences worth stating:

- Timers are no longer capped at 32.7 s: `T#5m` is 300,000 ms and `TON0.PT`
  reports it. A preset the bank cannot hold is refused, never clamped.
- `MW` arithmetic wraps at 32 bits (`i32::MAX + 1` is `i32::MIN`); `/` and `%`
  by zero still yield 0.
- A float pin is an integer view of its slot. At the boundary, `f64 as i32`
  truncates toward zero and saturates, so `2.7` reads as `2` and an out-of-range
  pin clamps. The output boundary converts back, so writing `2` drives `2.0`.
  This is ClassicLadder's `%IF`/`%QF` behaviour.
- A point may only be used as its declared kind: a word point named as a
  contact, or a bit point named in a compare, fails validation with `io_kind`
  instead of addressing the other image.

### Presets from a word

A function block's preset can be a live word rather than a literal:
`CAL TON0(IN:=start, PT:=MW3)` (or `PT:=` a declared word point) reads the bank
on every scan, so `MW3 = 5000` is a five-second timer and changing `MW3` changes
the preset from the next scan. Anything that is neither a preset nor a declared
word — `BOGUS`, `MW99`, a block member — is refused with `cal_arg` naming the
operand.

A compute (MOVE/ADD/…) coil can write a block member: `TON0.PT := MW3`,
`CTU0.PV := MW7`, `TON0.ET := 0`, or a declared word point. A target that is
neither a bank slot nor a block member is refused with `compute_target`.

Ordering matters, and it follows from the block owning one preset field: the
block's own instruction stores its source into that field on every execution, so
a preset written by a compute coil holds only until the block runs again. When
the preset must stay under the ladder's control, source it from a word instead
of writing it once.

## Jump and call coils

A rung may carry a **label**, and two coil kinds use one:

- `-[J]-` (jump) names a label in its **own section**. The rung's power decides:
  powered jumps to the labelled rung, unpowered falls through. A forward jump
  skips the rows in between; a backward jump loops, and a loop that never ends is
  stopped by the scan's op budget (100,000 ops) instead of hanging the scan.
- `-[C]-` (call) names a POU. It is an **inline**, exactly like the IL `CAL`
  instruction: no locals, no stack frame, and the callee's coils and timers are
  the ones it owns when it runs. The rung's power gates the call, so an unpowered
  call coil runs nothing.

Labels are section-local and must be unique in their section. A jump with no
label is `rung_label_missing`, two rungs with one label is
`rung_label_duplicate`, a jump to a label the section does not declare is
`jump_target_missing`, and a call to a POU the project does not declare is
`call_target_missing`. A call cycle — ladder, IL, or a mix — is `cal_cycle`.

## Expressions

A compare cell (`Cell::Expr`) and a compute coil (`CoilKind::Compute` with
`Coil.expr`) hold free text instead of a structured operator, which is how
ClassicLadder's compare and operate blocks are written. A compare expression is
a condition: it passes power while it is true. A compute coil's text is an
assignment, `target=value`, and it writes only while its rung passes.

The grammar is ClassicLadder's. Precedence, low to high:

| level | operators |
| --- | --- |
| 0 | `,` — AND between comparison groups |
| 1 | `\|` |
| 2 | `&` |
| 3 | `=`, `<>`, `<`, `<=`, `>`, `>=` (`=` is comparison; never `==`) |
| 4 | `+`, `-` |
| 5 | `*`, `/`, `%` |
| 6 | unary `-`, `+`, `NOT` |
| 7 | `^`, right-associative |
| 8 | number, `MWn`, a declared word point, `TON0.ET`/`TON0.PT`, `CTU0.CV`/`CTU0.PV`, `( … )`, `ABS(x)`, `MINI(a,b)`, `MAXI(a,b)`, `MOY(a,b)` / `AVG(a,b)` |

Operands are the project's own names, not `%` addresses: a word or float point
appears by the name it was declared with, and an imported `%IW1` has already
become `iw1`. There is no implicit `IW0`-style region addressing, because a
second operand namespace used by one construct would be a trap.

Details worth stating:

- `TON0.ET` is the elapsed time and `TON0.PT` the preset; `CTU0.CV` and
  `CTU0.PV` are the count and its preset.
- `/` and `%` by zero yield 0, matching the IL operators. `^` returns 0 for a
  negative exponent, and the result wraps at 32 bits like all word arithmetic.
  `MOY`/`AVG` is `(a+b)/2`, truncated toward zero.
- `NOT` binds tighter than a comparison, so write `NOT (MW0>1)`. `NOT MW0>1`
  is refused: it would negate a number.
- An assignment writes a word. `q3=1` is refused with `expr_operand`, because a
  bit target is not representable; write a compare into the bit's coil instead.
- One expression may nest 32 levels and lower to 512 operations.

Refusals carry the byte offset of the offending token and one of four codes:
`expr_syntax`, `expr_operand`, `expr_function`, `expr_depth`. A coil whose
`expr` is missing, or present on a kind that does not read it, is `coil_expr`.

## Retained values

Variables declared `retain` and the marker words `MW0..MW15` can survive a daemon
restart. The word and float I/O regions are never stored: HAL owns those values,
and a restored `QW` would be a command nobody issued. Set `[plc] retain_file` to
a path (empty means off, logged at info) and `retain_flush_s` for how often the
store is published (default 5; Stop and exit always publish).

The store is a small blob: magic `LPRT`, format version **2**, the **program
hash** and a **layout hash** over the retained slots, the marker-word count, the
whole bank size, the values, and a CRC-32. A restore needs both hashes to match,
so editing the program, reordering its symbols or adding a word point refuses an
old store instead of lining values up with the wrong slots. Format 2 widened a
word value to four bytes; a format-1 store is refused with `UnsupportedFormat`
rather than misread, and a format-2 store is refused the same way by an older
build. A refusal
is logged with its reason — bad magic, unsupported format, truncated, CRC
mismatch, layout mismatch, another program, or a blob over 64 KiB, which turns
retention off rather than writing something unreadable. A restore writes values
into both scan images; it never creates a force.

## Timing

The scan schedule is **anchored**: the expected start of a scan is the previous
expected start plus the interval, set once when the daemon starts and never
re-anchored on a late wake. A late scan therefore carries its lateness into the
next one instead of quietly resetting the clock, so a run that is consistently
late keeps reporting it.

Three durations are measured, each with current, minimum, maximum and an
exponentially weighted average over about two seconds:

| metric | from → to |
|---|---|
| scan time | wake-up → end of the body (inputs, ops, outputs) |
| cycle time | wake-up → previous wake-up |
| latency | wake-up → its expected start |

An **overrun** is a wake-up that landed past its expected start by more than
100 µs, not a body that ran longer than the period. A long body shows in scan
time, and makes the *next* wake late, which is where the overrun appears.

All timestamps are `std::time::Instant` (`CLOCK_MONOTONIC`). Do not switch to
`CLOCK_MONOTONIC_RAW`: it is not slewed by NTP, so a corrected clock would show
up as phantom lateness and false overruns.

The snapshot carries `timing.scan`, `timing.cycle`, `timing.latency` and
`timing.overruns` alongside `scan_time_us` and `overruns`; the PLC status bar
shows scan, cycle and lateness.

## Protocol

4-byte big-endian length + JSON, adjacent tagging:

```
{"request":"hello","data":{"protocol":8}}
{"request":"load","data":{"project":{...}}}
{"request":"reload","data":{"project":{...}}}
{"request":"start"}
{"response":"state","data":{ ... StateSnapshot ... }}
{"response":"error","data":{"code":"...","message":"..."}}
```

Protocol 8 widens the word bank and the `WordValue` wire type to 32 bits. The
version is checked on `Hello`, so a 7 client attached to an 8 daemon (or the
reverse) is told the numbers differ instead of decoding a truncated word. The
snapshot's `output_image` is the ladder's output values in compiled order; after
a reload it holds the values the field is still driving until the first scan
writes the new program's own.

Dashboard pins: `linc_plc.<binding>` for each I/O point — `HAL_BIT` for a bit
point, `HAL_S32` for a word point, `HAL_FLOAT` for a float point — plus
`linc_plc.alive`, `linc_plc.scan-count`, `linc_plc.scan-time-us`,
`linc_plc.fault`, `linc_plc.overruns`. See `hal/linc-plc.hal` for example
nets; it is documentation only and is not loaded by any machine profile.

## Tests

```sh
cargo test -p linc-plc-core -p linc-plc-client
cargo test -p linc-plc            # in-process daemon + socket end to end
cargo test -p linc-ui             # PLC page helpers + fake-daemon link
```

The daemon also has a manual smoke path: start it with `--no-hal`, then send
framed requests to the socket (the page uses `linc-plc-client`).
