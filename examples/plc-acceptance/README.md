# Ladder acceptance corpus

Twelve real ladder programs, their stimulus, and the output trace each one must
produce. A port that runs these and matches the traces has not regressed; one
that deviates has, and the trace says at which scan.

## Licence

These files are **GPL-3.0-or-later** and are not part of LiNC. They are aggregated
with it, not linked into it:

- `LICENSE` here is the GPLv3 text that governs everything in this directory;
- LiNC itself stays `GPL-2.0-or-later` (`Cargo.toml`, `debian/copyright`);
- nothing in `crates/` may reference this directory at build time — no
  `include_str!`, no build script, no embedded fixture. A test may read these
  files at run time.

## Provenance

The programs in `sources/` are redistributed **verbatim** from a third-party PLC
sample set released under GPL-3.0-or-later; the samples are the corresponding
source of the derived files here, which is why they are kept. `manifest.json`
records the origin.

The `.xml` files are conversions of those sources to PLCopen TC6, produced by a
converter that lives outside this repository. `stimuli/` and `traces/` were
produced by running the sources in the reference engine, also outside this tree.

## Layout

```text
sources/*.plcy    the program, verbatim: the authoritative definition
*.xml             PLCopen TC6, for the three that convert today
stimuli/*.json    the inputs each program is driven with, and for how long
traces/*.json     the outputs the reference engine produced, per scan
manifest.json     all 41 sampled files: 12 ladder, 29 excluded, per-file status
```

## Status, and what the manifest means

Two flags per file, and they are not the same question:

| flag | means |
| --- | --- |
| `converted` | the converter produced `<name>.xml` |
| `runs` | LiNC imports it, compiles it, and matches its trace after every scan |

`runs` is the "N of 12 convertible" count that `tests/acceptance.rs` prints. A file
can convert and still not run: conversion is the converter's job, running is
LiNC's, and the manifest names whichever of the two is short. `blocker` is the
cause, in words, for every file where `runs` is false — including the ones that
converted, so a conversion is never mistaken for a pass.

A file listed as blocked but converted is executed by the test anyway and must be
shown *not* to pass. That is what stops the count from going green by accident.

Today: six convert, six run. `silo` matches all 200 scans, including rung 4's two
coils on parallel branches — the importer turns such a rung into one rung per coil
and the scan evaluates them in order, so a `Vec<Coil>` model turned out not to be
needed. Four converted files each hold a timer on a rung with no coil, which the
importer emits instead of dropping. `traffic-lights` is the largest: six timers
chained through `.DN` contacts. `timer-counter` is the one that exercises `.TT`.

The six that remain are named in the manifest: typed tags and user data types
(five), and `INT[1]` bit-of-word operands (`start-stop`).

### One trace, questioned and settled

`timer-counter` first came out one scan ahead of its trace: `%Q0.0` on at scan 502
where the trace has 503. Both engines were dumped per scan around the point.

| scan | reference `TON0` EN/TT/DN/ACC | reference `TON1` DN/ACC | LiNC `TON0` ET/Q | LiNC `TON1` ET/Q |
| --- | --- | --- | --- | --- |
| 501 | 1/0/1/3000 | 1/2000 | 3000/1 | 2000/1 |
| 502 | 0/0/0/0 | 0/0 | 0/0 | 0/0 |
| 503 | 1/1/0/0 | 0/0 | 0/0 | 0/0 |

The engines agree on every timer value, so the scan order and the timer arithmetic
are not in question: rung 1 reads `TON1.DN` as the previous scan left it, resets
`TON0` at 502, and re-enables it at 503. What differed was the converter, which had
rendered `.TT` as a `LT` compare on `ET < PT`. That compare is true of a timer that
has just been reset — `ET = 0`, idle — where the timing bit is false, and it is one
scan wide, exactly the offset. `.TT` means *enabled and not done*, which the
reference computes as `not DN` while the block is enabled; LiNC now has the member
(`TON0.TT`) and the converter emits it. The trace was never touched, and no
tolerance was introduced.

The converter itself was wrong four times when this corpus first ran, and all four
are fixed in the out-of-tree tool: it emitted elements without closing tags, so no
output was well-formed XML at all; it wired a parallel block's branches from the
left rail instead of from the power node the rung had reached, so a branch's first
contact dropped the series before it; it mapped `.TT` (timing) onto `.Q` (done),
inverting any rung that used it, and now refuses it; and it wrote a bare number as
a timer preset where TC6 wants a time literal, so `1000` became `T#1000ms`.

## Running a comparison

1. Import `sources/<name>.plcy` is **not** the LiNC path — LiNC reads the `.xml`.
   If `<name>.xml` exists, import that; otherwise the program is not yet
   expressible and the test asserts the diagnostic in `manifest.json` instead.
2. Compile it, then scan it once per stimulus step with the stimulus' `scan_ms`,
   applying each input change **before** the tick for that scan index.
3. Read the `outputs` listed in the trace after every scan.
4. Compare against the trace.

### Replaying a trace

`traces/<name>.json` holds change points, not one row per scan:

```json
{"scan_ms": 10, "scans": 320, "outputs": ["%Q0.0"],
 "changes": [{"scan": 1, "v": {"%Q0.0": "1"}}]}
```

Every output starts at `"0"`. At scan *k*, its expected value is the value from
the last change whose `scan` is at most *k*. A one-scan difference is a real
difference: these traces come from a 10 ms scan, and timers are checked to the
scan.

`fault` is `null` when the reference engine completed the run. A non-null fault
means the trace stops there, so a test must expect the same fault rather than a
longer trace.

## Status

Twelve of the 41 sampled files contain ladder and no FBD or ST. Three convert
today; the other nine are blocked, each for a named reason:

| program | blocked by |
| --- | --- |
| silo | two coils on parallel branches, one rung |
| traffic-lights | six timers; LiNC allows four per type |
| start-stop | `ONS` one-shot, `Bits[0].0` bit operand |
| timer-counter | `RES` coil, and `.TT` is not `.Q` |
| recipes-data-type-ld | `Recipe[3]` struct array, struct-field presets |
| traffic-lights-routines | `DINT[6]`, JSR routines, DINT phase counter |
| two-tanks-cascade | `PID`, `SCP`, `MOV`, `REAL` tags |
| motor-starters-function-block | user FB called with formal parameters |
| silo-function-block | FB instance data (`FillStation` struct) |

The 29 excluded: 19 structured-text only, 7 function-block-diagram only, 3 mixing
FBD with LD. They are counted in `manifest.json` and left out of `sources/`.

## Adding a program

Put the `.plcy` in `sources/`, write `stimuli/<name>.json` describing the drive,
regenerate the trace with the reference engine's trace tool (outside this tree),
then update `manifest.json`. Stimulus format:

```json
{"scan_ms": 10, "scans": 320,
 "held": {"%I0.1": true},
 "inputs": {"%I0.0": [[0, true], [2, false]]}}
```

`held` applies from scan 0; `inputs` are `[scan, value]` pairs applied before that
scan's tick. Comment each stimulus with what it is meant to exercise — a trace
only proves something if the drive actually reaches the interesting state.
