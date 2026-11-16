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

The `.xml` files are conversions of those sources to PLCopen TC6. The converter
that produced them was a session artifact and is gone; **the committed `.xml` files
are the originals** and are the record of that conversion.

`tools/plcy2plcopen.py` is a **reconstruction, not verified**: it regenerates the
twelve files, but `garage-door`'s program diverges from its trace (`%Q0.1` is true
at scan 61 where the trace says false), so it must not be used to rebuild the
corpus until that is fixed. It is committed as a starting point, not as the tool
that made these files.

`stimuli/` and `traces/` were produced by running the sources in the reference
engine, which is outside this tree.

`tools/plcy2json/` converts the XML to the project JSON LiNC opens, with the same
canonicalisation the PLC page's Save uses — that one is verified by the corpus:
each project compiles to the same ops, word layout and output count as its XML.
Both tools are GPL-3.0-or-later with everything else here: they exist to process
this corpus, and their output is a derivative work of it.

Two provenance notes, because a tool's licence is not obvious from its job:

- `tools/plcy2plcopen.py` is a LiNC contributor's own work, written against this
  corpus. It reads PLCYard's `.plcy` format, which is what makes its output — and
  therefore the tool in this directory — part of the same GPL-3 aggregation. It is
  not a copy of PLCYard code; nothing is imported from it.
- `tools/plcy2json/` links `linc-plc-core` (GPL-2.0-or-later) to read the XML and
  write the JSON. That is the ordinary direction of use for a GPL library, and its
  own sources stay under this directory's licence.

## Layout

```text
sources/*.plcy    the program, verbatim: the authoritative definition
*.xml             PLCopen TC6, the conversion of each source
projects/*.json   the same programs as project JSON, the format the UI opens
stimuli/*.json    the inputs each program is driven with, and for how long
traces/*.json     the outputs the reference engine produced, per scan
manifest.json     all 41 sampled files: 12 ladder, 29 excluded, per-file status
tools/            the converters, so the conversions are reproducible
```

`projects/*.json` are read by the PLC page's OPEN, so the same twelve programs the
harness runs as XML also open in the simulator. The harness compares each project
to its XML — same ops, same word layout — so the JSON cannot drift from the
program the traces were recorded against.

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

Today: twelve convert, twelve run. `silo` matches all 200 scans, including rung 4's two
coils on parallel branches — the importer turns such a rung into one rung per coil
and the scan evaluates them in order, so a `Vec<Coil>` model turned out not to be
needed. Four converted files each hold a timer on a rung with no coil, which the
importer emits instead of dropping. `traffic-lights` is the largest: six timers
chained through `.DN` contacts. `timer-counter` is the one that exercises `.TT`,
`start-stop` is the one that writes a bit of a word, `traffic-lights-routines` is
three routines in one file, called by name, with a timer whose preset is an array
element, and the two `-function-block` files are user function blocks: two instances
of one body, called with their pins bound.

Nothing is blocked. The last file, `two-tanks-cascade`, needed the analogue blocks and a
second trace before it could be counted — see below.

### A struct array, and a field of one element

`Recipe { FillTime, MixTime, Lamp }` is a declared type, `Recipes: Recipe[3]` is an
array of it, and `TON,Fill,Recipes[Sel].FillTime` is a timer whose preset moves with
the selected recipe. The fields share one word type, which is what makes the run of
slots contiguous in that type's region: element `n`'s field `f` is at
`base + n*stride + f`, so one operand form finds any of them and no second bank is
needed. A struct whose fields differ, or hold bits, is refused by name — there is no
layout for it yet.

Every way the operand can be wrong says which struct, field and bound it broke: an
unknown field, an element past the array, an index that is not a declared word, an
array that is not declared. The corpus file's two timings only come out right if the
index is applied, so the trace checks the arithmetic and not just the count.

### The analogue blocks, and a trace that was a compile test

`two-tanks-cascade` has `REAL` tags, an `SCP` scaling block and two `PID` controllers, one
cascaded into the other. Its first trace held only `%Q0.2`, an alarm bit from a digital
input that the loops do not touch — so a `PID` that returned zero would have passed. The
hole was closed in the corpus, not worked around:

* the controller is the reference engine's positional form: `CV = KP·E + KI·∫E dt + KD·dE/dt`,
  gains on the error in percent of span, integral with anti-reset-windup, derivative on the
  process variable, and the loop updates on its declared interval (`UPD`, 100 ms here).
  The members are baked at compile time, so the scan reads no tag for them.
* the stimulus format grew **word inputs**, so `%IW68` (the setpoint at 50 %), `%IW66` and
  `%IW64` are driven, and the loops actually move.
* the trace now records `Setpoint`, `OuterOut`, `%QW64` and `%QW66` as numbers, and
  `manifest.json` declares their tolerance — `0.5` for the controller output, `0.01` for the
  setpoint, exact for the rest. The alarm bit stays exact. The rule the test gained: a
  numeric output compares within its declared tolerance, a missing tolerance means exact.

The reference computes in double precision and LiNC in single, which is why the tolerance is
declared rather than assumed: the two agree scan by scan inside it, and the values are not
the same bits.

### Instances, and why two callers do not share one

A `<pou pouType="functionBlock">` is a type: its interface is its own, and a project
variable whose type names it — `Pump1: MotorStarter` — is an *instance*. The compiler
runs the body once per call, with the instance's variables and its own copy of every
block the body declares, so `Pump1`'s ready lamp can be on while `Pump2`'s is off. That
is the test that matters, and it is the one that failed the first time: the contact
on `TON0.Q` read the declared timer instead of the site's copy, so both pumps shared
one delay.

Nothing is renamed at run time. A body's `#Start` is resolved when the body is
compiled — against the instance the call names — and the two runs of that body produce
two different slots from the same tree. A call binds its pins around the body: an input
is written into the instance's variable before the body runs, an output is read out of
it afterwards. An instance's field is an operand in a program that declares it, so
`Station.Filling` is a contact like any other.

A function block's variables are BOOLs for now; a word pin would need a slot per
instance in the word bank, and the corpus's blocks are all bits.

### Routines, calls, and writes that are not coils

`traffic-lights-routines` needed four things LiNC did not have, and none of them is a
new block:

* **several POUs.** Every ladder or subroutine body in the source becomes a `<pou>`, and
  the importer gives each body's section its own name so two bodies can coexist. The
  entry routine is MAIN and a call is a `Call` coil — LiNC inlines the callee under the
  rung's power, so a subroutine has no locals of its own, which is what a scan-based PLC
  means by a call.
* **`JSR` by name.** The call coil carries the POU's name, and validation checks it
  against the project's POUs rather than a number.
* **`MOV` and `ADD`.** A word write is a compute coil: `MOV,0,Phase` becomes
  `phase = 0` and `ADD,Phase,1,Phase` becomes `phase = phase + 1`, both through the
  expression engine that already existed. A `.ACC` member is the timer's `ET`, so
  `MOV,0,StepTimer.ACC` zeroes the elapsed time.
* **a preset read from an array.** `TON,StepTimer,Durations[Phase]` sets `PT` from one
  element of a declared array, read every scan. A literal index is folded at compile
  time and refused if the array has no such element; a computed index is read and
  clamped into the array by the scan, because a value is worth more than a stopped PLC
  (the same choice `POW` makes for a negative exponent).

The declaration's initial values came with it: `Durations` carries
`<initialValue><arrayValue>`, the model keeps them additively and skips them when
empty, and a reset returns the scan image to them rather than to zero.

One timer rule came out of this file and is now the model: **a done timer holds its
elapsed time.** The program zeroes `ET` on the done scan, and the timer still reports
done on the next one, so it does not add time that scan — which is why each phase lasts
`PT` plus one scan, and why the trace's transitions are at 601, 802, 903. The reference
engine holds it the same way; this is the model, not a tolerance.

### A bit of a word

`Bits[0].0` is bit 0 of the first element of `Bits`. It is an operand, not a value: a
contact reads the bit and a coil writes it, and the word around it keeps its other
bits. The form is parsed in one place (`model::WordBit::parse`), resolved in one place
(`profile::layout::word_bit`), and the declaration decides both bounds — the element
count and the type's width — so bit 16 of an `INT` is refused with the variable and
the width in the sentence, not read out of the next slot. A marker word's bit is the
same form, and a marker word is sixteen bits wide whatever the slot is.

`start-stop` needed two more things before its bit-of-word tag could run, and both were
converter faults rather than model limits:

* two coils in series on one branch. A rung ends in one coil, so the converter now
  emits one rung per coil, each with the series that reaches it — which is exactly the
  power the coil sees in ladder. The file's two coils are compared at scans 21 and 31,
  so the split is checked by the trace and not only by the count.
* the one-shot's memory. `ONS,tag` is a one-shot whose memory is `tag`, so the
  converter now writes the tag with the pulse instead of dropping it. That write is
  what puts `Bits[0].0` in a running program. The importer splits the two consumers of
  the block into two rungs and gives each its own `R_TRIG`; the two see the same input
  on the same scan, so their outputs are the same pulse, which is why the split is
  faithful.

A known limit: an edge contact on a bit of a word is refused by name. It would need a
previous-value slot of its own, and nothing in the corpus asks for one yet.

### Declared types, and the gate they were hiding

LiNC's importer refused every declaration that was not `BOOL`, and it refused it first,
so five of the twelve files could not report what actually stopped them. The importer
now reads `BOOL`, `INT`, `DINT`, `REAL`, `TIME`, an array of any of them, and a type
name the project declares itself; a name it keeps rather than resolves, so a refusal
says `MotorStarter` and not "unknown type". A type with no width for this build is
still refused, by name, and the project's own types are refused by validation because
there is no storage for them yet.

The word bank grew named typed regions for the declarations — `int`, `dint`, `real`,
`time` — appended to the existing one bank, so a project that declares types gains
slots at the end and every word and float point keeps its number. A load sign-extends
a 16-bit variable and a store keeps its low half, so two reads of an `INT` cannot
disagree; a `DINT` and a `TIME` are 32 bits; a `REAL` is its own 32 bits, unchanged.
The retain layout hash now covers the region table, not just the bank's size: a bank
that is the same size in another shape would put a retained word at the wrong slot.

The count did not move — six of twelve — and that is the point of the step: the gate
moved from the types to the constructs.

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
