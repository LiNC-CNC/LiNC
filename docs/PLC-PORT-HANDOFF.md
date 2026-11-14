# Handoff: the LiNC PLC, and the seam the next work goes through

Status: items 1–6 shipped, 198 tests green. This document has two jobs.

- **Part I** records the state of the code, so nobody re-derives it.
- **Part II** defines the architecture the next work must go through.
- **Part III** expresses the remaining work against that architecture.

Baseline: `LiNC-rebase @ 4468cad`. Read `docs/plc.md` for the operator-facing
description; this document is for whoever changes the code.

Audience: implementing agents. Parts II and III are decisions, not suggestions.
Where they disagree with the current code, the code is what changes.

---

# Part I — Where the code is

## I.1 What shipped

| item | commit | what it did |
| --- | --- | --- |
| 1 | `07dc1b7`, `c9a2cf0`, `c239f55` | word bank widened to 32 bits; `s32` and `float` HAL pins; `word`/`float` I/O points; page shows 32-bit values |
| 3 | `101349a` | presets sourced from a word (`PT:=MW3`); preset and value writes (`TON0.PT := MW10`) |
| 4 | `3e861cf` | jump and call coils in ladder, sharing IL's label machinery |
| 5 | `4e39cab` | free-text expressions in compare cells and compute coils |
| 2 | `02ec995` | ClassicLadder `.clp` importer, all-or-nothing, 22 refusal codes |
| 6 | `4468cad` | reload a running ladder without dropping its outputs |

Version markers moved exactly as briefed and no further:

```text
SCHEMA_VERSION   1   (unchanged — every model change is additive + serde(default))
PROTOCOL_VERSION 8   (WordValue and the word bank are 32-bit on the wire)
retain::FORMAT   2   (the blob layout changed with the word width)
```

## I.2 The type surface that now exists

These are the pieces the next work must build on rather than duplicate.

| type | where | why it exists |
| --- | --- | --- |
| `Bank { Bit, Word }` | `compile.rs` | a symbol's `index` is a slot in one bank or the other; word I/O cannot share the bit image |
| `Symbol { .., bank }` | `compile.rs` | bit symbols index `Vm.prev`/`next`; word symbols index `Vm.words` |
| `WordLayout { mw, iw, qw, fi, fo }` | `compile.rs` | the word bank's regions, in a fixed ABI order; `mw` is `WORD_COUNT` |
| `IoKind { Bit, Word, Float }` | `model.rs` | decides the HAL pin type and which bank region the value lives in |
| `PresetSource { Const(i32), Word(u16) }` | `compile.rs` | `Op::Fb` preset is now a source, resolved into `FbState.preset` before the block runs |
| `Cell::Expr { text }` | `grid.rs` | the grid form of `Logic::Expr`, for a compare over a free-text expression |
| `Coil { expr: Option<String> }` | `model.rs` | a compute coil carrying an assignment expression |
| `CoilKind::{Jump, Call}` | `model.rs` | target text lives in the existing `Coil.var`; serde shape unchanged |
| `Rung { label }` | `model.rs` | what a jump coil targets; also read by the `.clp` importer |
| `Request::Reload` / `Command::Reload` | `protocol.rs`, daemon | program swap at the scan boundary with `phase` preserved |
| `Vm::hold_outputs`, `Vm::outputs` | `vm.rs` | carry the field's last written values across a reload |
| `expr` | `expr.rs` | Pratt parser, `MAX_EXPR_DEPTH 32`, `MAX_EXPR_OPS 512`, offsets on every refusal |
| `clp` | `clp.rs` | `.clp` container + rung-grid importer, 22 codes, `import` / `import_path` |

Module weight, for where to look: `clp.rs` 1888, `compile.rs` 1824,
`validate.rs` 1610, `vm.rs` 997, `expr.rs` 835, `model.rs` 643, `grid.rs` 446.
Daemon side: `daemon.rs` 983, `hal.rs` 430, `server.rs` 400.

## I.3 Audit findings — three things to fix

These are drift left by items 1–6. Fix them before starting Part III.

1. **`docs/plc.md:137` contradicts itself.** It still reads *"Not yet: SFC,
   `.clp`, Modbus, true hot swap…"* while `docs/plc.md:180` documents
   `## .clp import (ClassicLadder)`. Delete `.clp` from the not-yet list. Leave
   SFC and Modbus.

2. **`crates/linc-plc-core/src/clp.rs` header points at a section that no longer
   exists.** It says "Read §2 of `docs/PLC-PORT-HANDOFF.md`". After this rewrite
   the importer's reference material is **Appendix A** here, and the
   operator-facing description is `docs/plc.md:180`. Repoint that comment.

3. **No dialect seam.** The 22 `clp` codes, the ClassicLadder counter rules, the
   nine-position grid geometry and the instruction names are all dialect-specific
   knowledge living in the shared core. That is not a bug yet — it is one
   dialect — but it is the reason Part II exists, and it gets expensive at the
   second dialect.

## I.4 Rules that still govern every change

These survived items 1–6 and apply to everything after.

1. **`SCHEMA_VERSION` stays 1.** Every model change is additive with
   `#[serde(default)]`. `validate.rs` rejects any other schema outright, so a
   bump orphans every project file on every machine.
2. **Refuse, never approximate.** A construct with no faithful mapping returns a
   `Diagnostic` with a specific code. No "close enough" branch. `clp.rs` and
   `plcopen.rs` are the models.
3. **`Vm::scan` allocates nothing, takes no locks, performs no I/O.** A change
   that breaks this is wrong regardless of what it enables.
4. **`Vm::scan` and `Vm::scan_with_dt` remain the test API.** 2,778 lines in
   `tests/compile_vm.rs` call them.
5. **Bump a version only with a reason you can state in one sentence**, and state
   it in the constant's doc comment, as 8 does.
6. **Do not commit.** Leave reviewable slices and report the diff.

---

# Part II — The architecture

## II.1 The problem

LiNC's PLC has one canonical model (`Project`) and one hard-coded semantics. Every
piece of dialect knowledge is spread across six files:

| concern | lives in today |
| --- | --- |
| instruction set | `compile.rs` — `Op`, `FbKind`, `MathOp`, `parse_block`, `parse_preset` |
| operand spelling | `compile.rs::word_operand_value`, `il.rs::word_operand`, `expr.rs` |
| memory model and layout | `compile.rs` — `Bank`, `WordLayout`, `WORD_COUNT` |
| instruction validity | `validate.rs` — every code |
| program languages | `model.rs::PouLanguage`, `il.rs`, `grid.rs` |
| import dialects | `clp.rs`, `plcopen.rs` |

The cost is visible in what is already shipped. `clp.rs` has to *translate
ClassicLadder's counter into* `CTU` plus a synthesised equality compare, because
LiNC has no counter whose `done` is equality. It has to map `%I3` to `i3` and
`@251/0@` to `CTU0.CV`, because there is one addressing scheme. Those are not
importer bugs — they are two dialects being forced through one semantics.

The same shape recurs: "can the LiNC GUI drive ClassicLadder?" is a hard question
only because a dialect cannot be a value. With a seam it becomes a table entry.

## II.2 The decomposition

Four parts, and only four.

```text
  editor  ──▶  canonical Project  ──▶  compiler  ──▶  Vec<Op>  ──▶  Vm
                       │                                            │
                  Notation (display only)                    Device (I/O only)
                       │                                            │
                  Profile (semantics) ──────────────────────────────┘
```

- **Canonical model** — `Project` / `Rung` / `Logic` / `Cell`. Dialect-neutral
  *shape*. Already exists.
- **Profile** — everything that differs between PLC families. The new part.
- **Notation** — how a profile's names and mnemonics are *displayed and typed*.
  Presentation only; never stored.
- **Device** — how the bit and word images reach the field. Today `hal.rs` is
  welded to HAL; a device seam makes the test and simulation paths possible.

The engine keeps: modes, the clock, the scan loop, faults, the snapshot, the
trace. Nothing dialect-specific crosses that line.

## II.3 What goes behind the profile seam

Mapped to the types that exist now. The left column is the interface; the right
column is the code that must move behind it.

| profile responsibility | moves out of | notes |
| --- | --- | --- |
| instruction table: mnemonic → operands, arity, is-output, draw shape | `compile.rs` (`FbKind`, `MathOp`), `grid.rs`, `ui` | one table drives compile, validate and render |
| operand resolution: text → slot | `compile.rs::word_operand_value`, `il.rs::word_operand`, `expr.rs::SymbolLookup` | one resolver, called by all three |
| memory layout: `WordLayout`, `WORD_COUNT`, region order | `compile.rs` | `mw` is dialect data, not a constant |
| symbol interning: name + `IoKind` → `Symbol { bank, index }` | `compile.rs::intern` | decides which bank and which region |
| instruction validity | `validate.rs` | structural rules stay in `validate.rs`; per-instruction rules move |
| languages | `model.rs::PouLanguage`, `il.rs` | which POU languages the dialect admits |
| runtime hooks | `daemon.rs`, `vm.rs` | `on_load`, `begin_scan`, `end_scan`, `reset_data` |
| I/O image naming and mapping | `hal.rs` | becomes `Device` + profile-named points |

Keep in `validate.rs` (structural, dialect-neutral): `duplicate_coil`,
`cal_cycle`, `no_sections`, `too_large`, `too_deep`, `empty_series`,
`empty_parallel`, `bad_name`, `reserved_name`.

## II.4 The three interfaces

Small on purpose. Grow them only when a second dialect needs a cell.

```rust
/// Everything that differs between PLC families.
pub trait Profile {
    fn name(&self) -> &'static str;

    // instruction set
    fn specs(&self) -> &'static [Spec];
    fn spec(&self, mnemonic: &str) -> Option<&'static Spec>;

    // memory and addressing
    fn layout(&self, project: &Project) -> WordLayout;
    fn intern(&self, name: &str, kind: IoKind, retain: bool) -> Symbol;
    fn resolve(&self, text: &str, symbols: &SymbolLookup) -> Result<Operand, OperandError>;

    // languages
    fn languages(&self) -> &'static [PouLanguage];

    // instruction validity; structural rules are validate.rs's job
    fn check_instruction(&self, instr: &Instr, ctx: &Ctx) -> Vec<Diagnostic>;
}

/// One instruction. Carries presentation, so the renderer is dialect-agnostic —
/// the same entry draws a contact in one notation and a box in another.
pub struct Spec {
    pub mnemonic: &'static str,
    pub family: Family,
    pub operands: &'static [OperandKind],
    pub optional: u8,
    pub output: bool,        // drawn against the right rail
    pub shape: Shape,        // Contact | ContactNc | Coil | Small | Block
    pub letter: &'static str,// glyph inside a coil or small box: L, U, S, R, P
}

/// How the images reach the field. Three methods, because that is all the scan
/// loop needs, and it is what makes a deterministic test device possible.
pub trait Device {
    fn read(&mut self, image: &mut IoImage) -> Result<(), DeviceError>;
    fn write(&mut self, image: &IoImage) -> Result<(), DeviceError>;
    fn step(&mut self, dt: Duration);
}
```

`Vm` stays untouched by this. It takes an `IoFrame` and produces images; the
`Device` is the daemon's, not the VM's.

## II.5 Migration — strangler, five moves

198 tests pass today. Do not rewrite. Each move is independently shippable, and
each has an acceptance test that is a *property of the code*, not a feature.

**M1 — instruction table.** Create `profile/spec.rs` with `Spec`. Express the
existing instruction set (`Op` mnemonics, `FbKind`, `MathOp`) as data.
*Acceptance:* `compile.rs` contains no match on a mnemonic string; the ladder,
the validator and the compiler all read the same table.

**M2 — operand resolution.** Move `word_operand_value`, `il::word_operand` and
`expr`'s lookup behind `Profile::resolve`.
*Acceptance:* exactly one function in the tree turns text into a slot; `grep -c
"MW" crates/linc-plc-core/src` drops to the profile module.

**M3 — memory layout.** Move `WordLayout`, `WORD_COUNT` and the region order into
the profile. `WordLayout` becomes `Profile::layout`.
*Acceptance:* no `WORD_COUNT` reference outside the profile module;
`retain::layout_hash` still notices a changed layout.

**M4 — validity split.** Keep structural rules in `validate.rs`; delegate
instruction rules to `Profile::check_instruction`.
*Acceptance:* `validate.rs` has no dialect mnemonic or operand-format literals.

**M5 — device seam.** Extract `hal.rs` behind `Device`. Add `TestDevice`
(deterministic: scripted inputs, recorded outputs, no HAL) and prove the whole
daemon path runs on it.
*Acceptance:* `cargo test -p linc-plc` runs the daemon against `TestDevice` with
no `hal_init`, and the existing HAL path is one impl of the trait.

Order matters only in that M1 must precede M4, and M2 must precede M3.

## II.6 Acceptance model — unchanged, and it is the transferable part

This was written for the `.clp` importer and it governs everything after: **the
definition of a construct is the function that implements it upstream, and the
test is a differential against a naive reference model of that function.**

For every dialect add a `#[cfg(test)]` reference model — short, deliberately
naive, no reuse of the compiler or the VM — and drive both with the same input
sequence, comparing observable coil and output values per scan.

| construct | reference | matrix |
| --- | --- | --- |
| contact / coil | `CalcTypeOutput`, `CalcTypeOutputSetReset` | NO/NC/NEG/SET/RESET × power × two scans |
| edge | `CalcTypeInput(OnlyFronts)` | 000, 011, 010, 001 over four scans |
| IEC timer | `CalcTypeTimerIEC` | TON/TOF/TP × pulse/hold × presets |
| counter | `CalcTypeCounter` | 16 input combinations, simultaneous edges, wrap at both ends |
| compare | `EvalCompare` / `MakeCalc` | one per operator, functions, a two-group `,` AND |

A test that asserts `Op::FbReset` was emitted proves nothing about a machine.
Assert what settles on a pin.

---

## II.7 Non-goals — do not take these from the reference material

- **A per-instruction interpreter.** The reference walks a callable per node every
  scan. LiNC compiles to `Vec<Op>` and runs a stack VM. Keep the VM.
- **Name rewriting at run time** to distinguish locals from globals. Resolve at
  compile time and the problem does not exist.
- **A browser/WASM host, or any scripting runtime on the controller.**
- **Copying the reference implementation's code.** See Appendix B.

---

# Part III — Work the seam defines

Each of these used to look like a port. Under the seam each is a profile or a
device, which is a much smaller thing.

## III.1 Structured text

`PouLanguage` has `Il`, `Ld`, `Fbd`. ST is the largest missing language and the
one an operator is most likely to arrive with.

- Add `PouLanguage::St` plus `Pou.source`; the profile declares whether it admits
  ST via `languages()`.
- ST **lowers to the same `Op` stream**. Do not add an interpreter, and do not
  evaluate the source per scan — that is the one design decision in the reference
  material worth rejecting (see §II.7).
- Reuse `expr.rs` for expressions; an ST statement is a lowering over the same
  operator set.
- Scope: assignments, `IF`/`ELSIF`/`ELSE`/`END_IF`, `CASE` with ranges,
  `FOR`/`WHILE`/`REPEAT`, `EXIT`/`RETURN`, FB calls with named arguments,
  `JSR`. Arrays and structs wait for §III.6.
- Refusal posture identical to `clp.rs` and `plcopen.rs`: all-or-nothing, named
  diagnostics, byte offsets from `expr.rs`.

Acceptance: the §II.6 differential, plus a round trip through `validate()` and
`compile()` on a project that mixes LD and ST POUs.

## III.2 A second semantics — this is what retires the ClassicLadder question

The `.clp` importer translates ClassicLadder into the native dialect and loses
what does not fit. With the seam, the honest shape is two profiles:

```text
profile "iec"          current: %MW words, nine-position grid, CTU/CTD with Q = CV >= PV
profile "classicladder" counters with done = equality, 0..9999 wrap,
                        empty/full outputs, %B/%I/%Q/%W addresses,
                        expression text preserved verbatim
```

Then `clp::import` targets the `classicladder` profile and stops translating:
`done` is equality because *that profile's counter is like that*, not because the
importer synthesised a compare. The three `clp_counter_bidirectional` /
`_reset` / `_overflow` refusals disappear because the construct becomes
representable.

This is the concrete payoff of Part II, and it should be the first proof that the
seam pays for itself. Do not start it until M1–M4 are in.

## III.3 Notation

Display and input only; never stored. A `Notation` maps mnemonic and operand
spelling both ways:

| notation | contacts | coil | timer | I/O |
| --- | --- | --- | --- | --- |
| `iec` | NO / NC | S / R | PT / ET / Q | `%I0.0`, `%IW64` |
| `ansi` | XIC / XIO | OTL / OTU | PRE / ACC / DN | `Local:1:I.Data.0` |

`Spec.shape` and `Spec.letter` make this free: the table already knows a contact
from a coil. A notation is a rename table plus an address formatter, and
`Project` does not change. Store one canonical form; convert on load and on
display, as `plcopen.rs` already does by lowercasing identifiers.

Acceptance: switching notation changes no byte of the saved project.

## III.4 Symbol geometry

The renderer lives in `crates/linc-ui/src/screens/plc_grid.rs`
(`CellSymbol`, `draw_cell_symbol`). Shape selection must come from
`profile/spec.rs`'s `Shape` enum, not from matching `Cell` variants.

**Source of truth for the forms:** IEC 61131-3:2003 clause 4.2 — Table 59 (power
rails), Table 60 (links), Table 61 (contacts), Table 62 (coils) — with IEC
60617-12/-13 as the symbol standards it references. The copy in this tree
(`~/Documents/PLC/61131-3.pdf`) is IHS-licensed: **read it to know the form, do
not trace its figures.** Draw your own geometry from the form; never measure
another renderer.

### Decisions taken

Agreed with the operator; do not re-litigate these while implementing.

| # | decision |
| --- | --- |
| D1 | Block bodies follow IEC 61131-3 cl. 4.1.1 — name and pins inside — with rounded corners (38 wide, 23/32/41 tall by row count, rx 4; `Small` 28 x 18 rx 3). |
| D2 | A coil's leads terminate on its arcs, and `S`/`R`/`P`/`N` sit inside the coil. |
| D3 | Shape comes from `profile::spec::Shape`, never from matching a `Cell` variant; `CellSymbol` is retired. |
| D4 | Named module `const`s in cell units with derived expressions; **no** `Geometry` struct. |
| D5 | One scale factor computed from the cell rect, passed down; no pixel literal below it. |
| D6 | Arithmetic invariant tests are the primary guard; a golden image is secondary. |
| D7 | The renderer draws rung furniture and the symbol, never the operand label. |
| D8 | Three visual states (`Unknown`/`Off`/`On`) carried by `Style`; weight **and** colour, never shape. |
| D9 | Pin *names* are `Spec` data, not renderer knowledge. |
| D10 | A block spans two positions. Pin values are drawn **outside** the body, padded 6 units, on their own row — no stubs. Only the power row carries conductor. Boolean pins light their name when TRUE. |

### Rules that are correctness, not taste

1. **A coil's leads end exactly on its arcs.** A lead must terminate where the
   arc is, with no overshoot past it into the arc's interior. Draw the arc first
   from its radius, then terminate the lead at that same radius — never at a
   contact's half-gap, which is a different number and leaves the wire visibly
   crossing the circle.
2. **`S` and `R` sit inside the coil**, between the arcs, as Table 62 draws
   `--(S)--` and `--(R)--`. The same applies to `P`/`N` on a transition coil and
   to the negation slash of `--(/)--`. A letter outside the coil, or above it,
   reads as a separate annotation and breaks the family resemblance with every
   other renderer.
3. **A transition letter on a contact sits inside the gap** — `--|P|--`,
   `--|N|--` (Table 61, items 5 and 7). Above the bars is neither the table's
   form nor a recognised convention.
4. **The NC slash crosses between the bars**, not past them (Table 61, item 2).
   Its span is the bar separation, at 45 degrees.
5. **A block body carries its name inside the box**, with its pins labelled on
   the inside edges, as clause 4.1.1 draws a function box. A bare rectangle is
   not a block symbol; the operator cannot tell a timer from a compare. The
   corners are rounded — see **Block body** below for the numbers.
6. **The conductor is centred in the cell.** Every other renderer centres it;
   `CONDUCTOR_Y = 39` of a 50-unit cell puts the rung low and makes the row gap
   read as an empty band above each symbol.

### Geometry to implement

One unit, one stroke. Named `const`s at module scope in `plc_grid.rs`, in the
file's existing style — it already asserts layout facts in tests around
`CELL_WIDTH`, `CONDUCTOR_Y` and `ROW_STRIDE`.

**Do not introduce a `Geometry` struct.** There is one renderer, one notation and
one cell size; a struct of twenty fields each read once is indirection with no
second caller, and it invites a per-notation variant that does not exist. Express
derived values as expressions of the primaries, so they cannot drift apart:

```rust
const CONDUCTOR_Y: f32 = CELL_HEIGHT / 2.0;   // not 39.0
const SLASH: f32 = BAR_HALF;                  // spans the bars
const LEAD: f32 = (CELL_WIDTH - BLOCK_W) / 2.0;
```

Then **one** scale factor at the top of `draw_cell_symbol`, and every dimension
below it multiplied by that factor:

```rust
let s = rect.height() / CELL_HEIGHT;
```

The cell is 48 x 50 today, so `s == 1.0` and this changes nothing. It is the line
that makes a future zoom, or a denser view, safe; without it every literal is a
latent bug the moment the cell resizes. No bare numeric literal below that line.

| quantity | value | note |
| --- | --- | --- |
| cell | 48 x 50 | keep; the conductor moves to the vertical centre |
| conductor | cell centre | was `y = 39` |
| contact bar separation | 16 | was 13 (`+-6.5`); matches Table 61's proportions |
| contact bar height | 16 | already correct |
| NC slash | bar to bar, 45 deg | was `+-9` against `+-6.5` bars |
| coil radius | 8 | was 9 |
| coil arc span | 180 deg, symmetric about the conductor | was 143 deg (1.9 to 4.4 rad) |
| coil lead end | exactly at the arc radius | see rule 1 |
| coil letter | centred inside, on the conductor | was outside-left at `cx - radius - 5` |
| negation slash | inside, across the arc gap | was outside-right |
| small box (compare, arithmetic) | 28 x 18, rx 3 | was 32 x 18, square |
| block box | 38 wide, 23/32/41 tall by row count, rx 4 | was 32 x 18, square, no name |
| block span | 2 positions (96) | side = 6 pad + 21 value + 2 margin |
| stroke | 1.6 active, 1.2 inactive | state by weight and colour, never by shape |

Arc spans under 180 degrees are what make a lead appear to pierce the coil: the
arc's endpoints sit above and below the conductor, so the lead meets it mid-arc
and the two strokes cross. A half circle whose ends lie on the conductor line
removes the crossing by construction.

#### Block body

IEC 61131-3 clause 4.1.1 supplies the layout; the corners are rounded, in the
PLCYard manner. Every figure is in **cell units**, so it scales with zoom and with
the notation rather than being a pixel figure.

The body sits in a **two-position span**, because the pin values are drawn
outside it and there is no room for them in one 48-unit cell:

```text
            +----------+
     rung---|IN      Q |---rung
      4s ---|PT     ET |--- 2.4s
            +----------+
```

| element | value |
| --- | --- |
| span | 96 (two positions) |
| body | 38 wide; height 23 / 32 / 41 for 1 / 2 / 3 rows |
| corner radius | 4 |
| side | 29 = 6 pad + 21 value + 2 margin |
| row pitch | 9 |
| title baseline | 10 above the power row |
| pin names | 6.5 px, muted, inside the body at pad 6 |
| pin values | 6.5 px, monospace, outside, padded 6 from the body |

Rules:

- **The power row is row 0** and it is the only row with conductor: `IN` / `Q`,
  `CU` / `Q`, `S1` / `Q1`. The wire runs the full 32 units of each side, so the
  rung is continuous and the block is in series, per clause 4.2.5's requirement
  that every block show a Boolean in and out for power flow.
- **Every other row is a value row, and draws no line at all.** No stub, no
  conductor. A pin's value sits against the body with 6 units of padding:
  right-aligned for an input, left-aligned for an output. A pin with no value
  draws nothing.
- **Only TIME and INT pins carry text.** `PT`, `ET`, `PV`, `CV` show values.
  Boolean pins — `IN`, `Q`, `CU`, `R`, `S1`, `CLK` — show no text at all.
- **A TRUE boolean pin lights its name.** The name is drawn in the accent colour
  and weight 700 while the pin is TRUE, and in muted ink otherwise. The name is
  the indicator, so there is no separate lamp and no `1`/`0` text. This applies on
  the power row too: `IN` lights while the rung feeds the block, `Q` while the
  block passes power on.
- **A side with no pin draws nothing.** A counter's `R` row has an input and no
  output, so its right side gets no name and no value — only the body edge.
  A stub there would invent a connection that does not exist.
- **The type text is the block's own name** — `TON`, `CTU`, `SR` — not its
  instance. The instance stays in the label above the span.
- **Body height follows the row count**, so no block is padded with an empty row
  and none is clipped. `height = 18 + 9 * (rows - 1) + 5`, which gives 23 for one
  row, 32 for two and 41 for three.
- `Shape::Small` keeps its own body, 28 x 18, rx 3, one position, operator
  centred: `>`, `>=`, `+`, `:=`, `P`, `N`. It has no value rows.
- The body is 38 wide so the two pin columns keep clear space between them:
  at 32 a pair of two-character names such as `S1` and `Q1` left only 6 units
  between. Do not narrow it to buy value-column width.
- The radius is 4 of 38, about a tenth of the width. Under 3 the box reads as a
  rectangle; over 6 it reads as a button.
- A value may not overlap the body edge: its padding is measured from the
  rounded corner's tangent line, not from the un-rounded outline.

#### Pin table

Fill `Spec.pins` from this. Row 0 is the power row in every case.

| block | rows | left, top to bottom | right, top to bottom |
| --- | --- | --- | --- |
| `TON`, `TOF`, `TP` | 2 | `IN`, `PT` | `Q`, `ET` |
| `CTU` | 3 | `CU`, `R`, `PV` | `Q`, —, `CV` |
| `CTD` | 3 | `CD`, `LD`, `PV` | `Q`, —, `CV` |
| `SR` | 2 | `S1`, `R` | `Q1`, — |
| `RS` | 2 | `S`, `R1` | `Q1`, — |
| `R_TRIG`, `F_TRIG` | 1 | `CLK` | `Q` |

Read off IEC 61131-3 Tables 34, 35, 36 and 37. Two consequences worth noting
because they are easy to get wrong from memory:

- A bistable (`SR`, `RS`) has **one** output, `Q1`. There is no second `Q` pin on
  the box face, even though the body keeps an internal `Q1` feedback.
- The edge detectors are **one row**: `CLK` and `Q` share row 0, so their body is
  23 tall, not 32.

`CTUD` is not in the table: LiNC has no up/down counter, and the `.clp` importer
refuses one rather than faking it with two blocks.

#### Value formatting

The UI formats; the renderer positions. One place decides what a value means.

| pin type | shown | example | when unset |
| --- | --- | --- | --- |
| BOOL | nothing — the name lights instead | — | name unlit |
| TIME | seconds, one decimal, `T#` dropped | `4s`, `2.4s` | `—` dim |
| INT | digits, padded to the preset's width | `10`, `007` | `—` dim |

`—` rather than `0`, so an operand the snapshot cannot answer for never reads as
a value. The preset is padded to a fixed width so a counting `CV` does not make
the text jitter.

#### The two-position span is occupancy, not a new cell

Correcting an earlier note in this document: **this does not need a model
change.** A block stays one `Cell::Block`; its span is a rule about the cell to
its right.

- **`validate.rs`** — a `Cell::Block` may not sit at `POSITIONS - 1`, and the cell
  immediately to its right must be `Cell::Wire` (or the row must end there).
  Code: `block_span`. That makes 8 positions usable and a block consumes two of
  them.
- **`plc_grid.rs`** — the render loop draws a block across the boundary of its
  cell and the next one, and **skips drawing the next cell**. The box straddles,
  centred on the boundary, which is what puts 29 units of value column on each
  side.
- **cursor movement** — stepping right from a block advances two positions.
- **importers** — `clp.rs` and `plcopen.rs` must emit a `Cell::Wire` after every
  block they emit, including the last-in-row case, which they must refuse rather
  than overrun.

Nothing in `compile.rs` changes: the compiler is told nothing about the span,
because the neighbour is a wire and a wire lowers to nothing.

#### Renderer API

Three functions replace the single `draw_cell_symbol`. The split is between rung
furniture, the symbol, and the operand label: the renderer draws the first two
and never the third.

```rust
/// How an element is painted. Weight and colour both come from `State`, so a
/// call site cannot forget one of them.
pub(crate) enum State { Unknown, Off, On }

pub(crate) struct Style { ink: Color32, accent: Color32, state: State }

impl Style {
    pub fn new(ink: Color32, accent: Color32, state: State) -> Self;
    /// Outline, bars, arcs. 2.2 px when On, 1.6 px otherwise.
    fn body(&self, s: f32) -> Stroke;
    /// The conductor. 1.6 px when On, 1.2 px otherwise; ink dimmed when Unknown.
    fn lead(&self, s: f32) -> Stroke;
}

pub(crate) fn draw_wire(painter: &Painter, rect: Rect, style: Style);
pub(crate) fn draw_link(painter: &Painter, rect: Rect, style: Style);
pub(crate) fn draw_face(painter: &Painter, rect: Rect, face: &Face<'_>, style: Style);
```

`Face` is what the call site derives from `Spec` plus the cell:

```rust
/// One row of a block body. Row 0 is the power row.
pub(crate) struct Row<'a> {
    ins: Option<&'a str>,      // name, inside, left edge   — from Spec.pins
    out: Option<&'a str>,      // name, inside, right edge
    value_in: Option<&'a str>, // already formatted, drawn outside after the stub
    value_out: Option<&'a str>,
}

pub(crate) struct Face<'a> {
    shape: Shape,          // profile::spec::Shape
    glyph: &'a str,        // Spec.letter, or Spec.mnemonic for a block face
    rows: &'a [Row<'a>],   // empty for contact, coil and small
}
```

Rules that follow from this shape:

- **`Shape` comes from the table, never from the cell.** `draw_face` matches on
  `Shape`. The mapping `Cell -> Face` stays at the call site, where it is the
  editor's job, and `CellSymbol` disappears so the two cannot drift.
- **Pin names are `Spec` data.** Add
  `pins: (&'static [&'static str], &'static [&'static str])` to `Spec`, filled
  for `Family::Fb` and empty otherwise. `draw_face` must not contain the string
  `TON`.
- **Pin values arrive pre-formatted.** The UI formats them from the snapshot
  (one decimal for TIME, padded digits for INT, `—` when unset) and hands over
  strings. The renderer positions text and never formats a number, so the one
  place that knows what a value means is `slc` reporting, not the painter.
- **`draw_wire` and `draw_link` are not shapes.** A conductor is rung furniture,
  not an instruction; a `Shape::Wire` would put it in the instruction table where
  it does not belong.
- **The operand label is drawn elsewhere.** `grid_cell_label` already owns it for
  contacts and keeps owning it. `Face.glyph` is symbol furniture only: `S`, `R`,
  `P`, `N`, `:=`, a block's type name.
- **`State::Unknown` is a real state.** `cell_active` returns `Option<bool>`; a
  block whose `Q` is not published is `Unknown` and must not be painted `Off`.
  Dim the ink and draw no claim.
- **Scale is passed down, never re-derived.** Each `draw_*` computes `s` from the
  rect as its first line and passes it to private helpers; no helper reads a
  module const for a pixel dimension.

### Acceptance

Arithmetic tests on the constants, one per rule above. These are what stop the
defects returning, and they cost microseconds each:

- `CONDUCTOR_Y == CELL_HEIGHT / 2.0` — the conductor is centred (rule 6).
- `SLASH == BAR_HALF` — the NC slash spans the bars, not past them (rule 4).
- `COIL_SPAN == PI` — the coil is a half circle, so a lead meets its end rather
  than crossing it mid-arc (rule 1).
- `BLOCK_W + 2.0 * LEAD <= CELL_WIDTH`, and the same for the height — the body
  fits its cell and the leads have somewhere to go (rule 5).
- `BLOCK_R >= 3.0 && BLOCK_R * 6.0 <= BLOCK_W` — soft, not a button.
- `TITLE_PX + INNER_PAD <= BLOCK_W / 2.0` — the type text fits inside the body.
- `2.0 * (PAD_GAP + VALUE_W + MARGIN) + BLOCK_W == SPAN` — the two-position span
  is exactly consumed, so no value is clipped and no cell is left over.
- **No line is drawn off the power row.** Assert that the drawing of a 3-row
  block emits exactly one horizontal line per side, both on row 0.
- **`height(rows) == 18 + 9 * (rows - 1) + 5`** for rows 1, 2 and 3 — 23, 32, 41.
- **A block at the last position is refused** with `block_span`, and a block whose
  neighbour is not `Wire` is refused too. One test per case.

Then four tests for the API itself, none of which needs a painter:

- **Every `Family::Fb` row declares pins, and no other row does.** A table-level
  assertion, so a new block cannot ship without its pin names (D9).
- **`Face::from_cell` yields the table's `Shape`.** Build a `Cell::Block`, a
  `Cell::Contact { negated: true }` and a `Cell::Compute`, assert the three come
  back as `Shape::Block`, `Shape::ContactNc`, `Shape::Small`. This is the
  regression that keeps D3 true after `CellSymbol` is deleted.
- **`State::Unknown` is visibly not `Off`.** `Style::new(a, b, Unknown).body(s)`
  differs from `Style::new(a, b, Off).body(s)` in colour, and matches it in
  width; `On` differs in both (D8).
- **Weight scales with `s`.** `Style::body(2.0).width == 2.0 *
  Style::body(1.0).width`, and the same for `lead` (D5).

A golden-image test is the **second** layer, for the rendered row only. It
answers "did the picture change", not "is the geometry correct"; reviewers
rubber-stamp golden diffs, so it must never be the primary guard.

## III.5 Device, and the test harness it enables

`Device` replaces the welded `hal.rs` path. Three impls, in this order:

1. `HalDevice` — today's `hal.rs`, behind the trait. No behaviour change.
2. `TestDevice` — scripted inputs, recorded outputs, injected clock. This is what
   makes §II.6 cheap and what lets `cargo test` cover the daemon's I/O path.
3. `PlantDevice` — a process model that drives the inputs (motor, tank, silo,
   sorter). Orders of magnitude smaller than `linc-sim3d` and the thing that makes
   commissioning a ladder possible without the machine.

A `PlantDevice` plus the §II.6 harness is a *grader*: run a program against a
plant, assert the outcomes, report per-requirement pass/fail. Worth having for
regression alone; it does not need a UI.

## III.6 Typed data

`WordLayout` is one flat `i32` bank. Real ladders want `REAL` and wider integers,
and the `.clp` importer already meets `%IF`/`%QF` floats as integer views.

Scope reference for the type system, in order of need: `BOOL`, `INT`,
`DINT`, `REAL`, `TIME`, then `TIMER`/`COUNTER` struct instances with named
members. Do this as a widening of `WordLayout` into named typed regions, not as a
second bank — the ABI order lesson from item 1 applies again: inserting a region
renumbers every slot after it, and `retain::layout_hash` must notice.

Arrays and indexing sit at the edge of this, so state the boundary here rather
than leaving it to whoever gets there. §III.1 and §III.8 both point at this
section for indexed fields, and it did not previously name them.

- **Declared arrays with a checked index are in scope.** `Durations[Phase]`, where
  `Durations` is a declared `DINT[6]` and `Phase` is a `DINT`, is a computed
  offset inside a known extent: the validator knows the element type and its
  bounds, and the compiler can emit a checked load. Two corpus programs need it
  (`recipes-data-type-ld`, `traffic-lights-routines`).
- **ClassicLadder's indirect addressing is not.** There an operand names a cell or
  variable by a number computed at run time, with no declared type and no extent —
  arbitrary aliasing into the memory image, which defeats static validation.
  `clp.rs` already refuses it as `clp_indexed`, and that refusal is a decision, not
  a gap.

The test that separates them: can the validator name the element's type and
bounds before the program runs? Then it is an array. If not, it is indirection.

## III.7 ZX attribution in code comments

The documentation no longer names the ZX Spectrum program this dialect started
from. The code comments still do, in **42 places across 11 files**. Same rewrite
as the documentation pass: **state the rule, drop the attribution.**

| file | refs |
| --- | --- |
| `linc-ui/src/screens/plc.rs` | 13 |
| `linc-ui/src/screens/plc_grid.rs` | 7 |
| `linc-core/src/gcode.rs` | 4 — probably unrelated, see below |
| `linc-plc-core/src/vm.rs` | 3 |
| `linc-plc-core/src/model.rs` | 3 |
| `linc-plc-core/src/compile.rs` | 3 |
| `linc-plc-core/src/profile/layout.rs` | 3 |
| `linc-plc-core/src/grid.rs` | 2 |
| `linc-plc-core/src/profile/mod.rs` | 1 |
| `linc-ui/src/screens/common.rs` | 1 |
| `linc-ui/src/shell/dialogs.rs` | 1 |

How to rewrite, by kind:

- **"as ZX-PLC draws a coil"** → describe the shape: "a coil is two arcs". A
  comment should say what the code does, not who did it first.
- **"ZX writes the letter between the bars"** → "the letter sits between the
  bars". If the reason matters, give the reason: at this size an arrow reads as a
  stray plus sign.
- **"ZX geometry: one continuous conductor per rung"** → "one continuous conductor
  per rung". The design is LiNC's now and stands on its own.
- **A comment that exists only to cite the reference** should be deleted outright.
  Keep the ones that record a trap; drop the attribution half.

`linc-core/src/gcode.rs` is the exception: "zx plane" there is most likely the
X-Z plane of the toolpath, not this. Read it before touching it, and leave it
alone if it is geometry.

**Acceptance:** `rg -i zx crates/` returns only `gcode.rs`, or nothing.

**Constraint:** comments only — no behaviour change in the same commit. The tree
is owned by other agents while the seam work lands, so this is a single
comment-only commit made when those files are quiet.

## III.8 The acceptance harness

`examples/plc-acceptance/` now holds twelve ladder programs, twelve stimuli and
twelve traces, and a manifest that says which of them convert (Appendix E).
Nothing reads them yet. That is the work, in the order the evidence supports.

### The harness itself

1. **Iterate the corpus.** For each `sources/<name>.plcy`: if `<name>.xml` exists,
   import, compile, scan once per stimulus step, and compare the outputs after
   every scan against `traces/<name>.json`. If it does not exist, assert the
   blocker named in `manifest.json`.
2. **Replay traces, do not search them.** Every output starts at `"0"` and takes
   the last change at or before the scan under test. A one-scan offset in a timer
   is a failure, not a rounding difference.
3. **Expect the fault.** A trace with a non-null `fault` ends there; the run must
   end the same way.
4. **Print the count.** "3 of 12 convertible, 9 blocked" should appear in the test
   output. It is the number that has to fall, and it should be visible without
   reading code.
5. **Read at run time.** No `include_str!`, no fixture embedded in the binary, no
   build-script reference. §Appendix E makes that a licence requirement, not a
   preference.

### Then the model changes it will start failing on

| # | change | why |
| --- | --- | --- |
| 1 | `coils: Vec<Coil>` in `Rung` | unblocks `silo` (parallel coils) and stops a block-only rung being dropped, which is why tier 1 fails silently today |
| 2 | instance numbering in `plcopen.rs` | `garage-door` has two timers and three blocks; today every block binds to instance 0 |
| 3 | `FB_PER_TYPE` above four | `traffic-lights` needs six; the limit is arbitrary and instances are already numbered |
| 4 | typed data — structs, arrays, indexed fields (III.6) | 4 of the 12 ladder programs |
| 5 | a timer whose `.DN` can restart the next rung | `traffic-lights` chains six of them |

Items 1 and 2 are worth doing together: both are in the importer's rung path, and
separately each leaves the corpus half-broken.

### Then the corpus side

- **Extend the converter** for what TC6 expresses faithfully: `RES` onto a
  counter's `R` input, coils on parallel branches, typed tags and arrays. The
  convertible count rises with no LiNC change beyond items 1 and 3 above.
- **Fix the converter's `.TT` → `.Q` mapping**, which inverts a condition.
- **Add the `debian/copyright` stanza** when the corpus is committed: its own
  `Files:` line, derivation stated, `GPL-3.0-or-later`.
- **Keep the reference engine honest.** A trace is only as good as the run that
  produced it; where it disagrees with IEC 61131-3, IEC wins and the trace gets
  corrected, not the semantics.

## III.9 Symbol geometry, as a change list

§III.4 states the geometry and the tests that guard it, but it never names a
file, a symbol or a first step. The model work in §III.8 was concrete and got
done; this was prose and did not. This section is the missing translation. Read
§III.4 for the numbers and the reasoning; execute this.

### The code as it stands

All of it is `crates/linc-ui/src/screens/plc_grid.rs`, `draw_cell_symbol`:

| what | now | §III.4 wants |
| --- | --- | --- |
| block body | `Rect::from_min_max(cx - 16, y - 9, cx + 16, y + 9)`, `rect_stroke(.., 0.0, ..)` — 32 x 18, square, **no name inside** | 38 wide, `height(rows)` = 23 / 32 / 41, `BLOCK_R = 4`, name and pins inside |
| NC slash | `(cx - 9, y + 8)` to `(cx + 9, y - 8)`; bars at `cx +- 6.5` | `SLASH == BAR_HALF`: spans the bars, does not run past them |
| edge letter | at `y - 11`, above the bars | between the bars, in the gap |
| coil | two arcs, `1.9..4.4` and `-1.25..1.25` rad | `COIL_SPAN == PI`, leads ending on the arc |
| values | not drawn for blocks | outside the body, `PAD_GAP = 6`, no stub off the power row |

Already right and worth keeping: one continuous conductor per rung, leads
running to the cell edges so neighbours show no joint, `CONDUCTOR_Y` named.

### Steps, in order

1. **Constants first.** Add the ones §III.4 names — `BAR_HALF`, `SLASH`,
   `COIL_SPAN`, `BLOCK_W`, `BLOCK_R`, `PAD_GAP`, `VALUE_W`, `SPAN`,
   `height(rows)` — each derived from `CELL_HEIGHT` or from a neighbour, no
   `Geometry` struct.
2. **Land the acceptance tests from §III.4, failing.** They are arithmetic and
   cost microseconds. Doing this second means every later step is a measured
   change, not a redraw someone eyeballs.
3. **Contacts.** `SLASH = BAR_HALF`; move the `P`/`N` letter into the gap between
   the bars.
4. **Coils.** A half circle (`COIL_SPAN = PI`) with the leads terminating on the
   arc ends; the `S`/`R`/`J`/`C` letter inside the coil; the negation slash at
   the coil's edge.
5. **Blocks.** `BLOCK_W = 38`, `height(rows)`, `BLOCK_R = 4`; the type name inside;
   pins inside per the §III.4 table; the value outside with no horizontal line
   through it; two positions occupied, neighbour must be `Cell::Wire`, refused as
   `block_span`. This is the only step that touches `validate.rs`.
6. **Replace `CellSymbol` with `Face`.** `Face::from_cell` becomes the single
   mapping; `Row` carries `ins/out/value_in/value_out`; `Style` takes the `State`.
   The tests in §III.4 for `Face::from_cell` and `Style` are the guard that this
   deletion did not change what is drawn.
7. **Golden image last**, for the rendered row only. It answers "did the picture
   change", not "is the geometry right".

### Done when

- Every arithmetic test in §III.4 passes.
- A `TON` renders as a named two-row body, 32 tall, `IN`/`PT` on the left, `Q`/`ET`
  on the right, with the elapsed value outside the body and no line drawn across
  it.
- A `CTU` renders three rows, 41 tall, with a blank right side on its `R` row.
- An NC contact's slash spans exactly its bars, and a `P` or `N` letter sits
  between them.
- `State::Unknown` is visibly not `Off`, so a value the snapshot cannot answer for
  is not shown as false.
- A block in the last position, or beside something other than a wire, is refused
  with `block_span`.

UI only, apart from the `block_span` rule. Doing this before III.6 is right: typed
data grows the block body, and it is cheaper to grow a body that already has rows.

# Part IV — Appendices

## Appendix A — `.clp` refusal codes, as shipped

All-or-nothing: any refusal returns diagnostics and no `Project`.

```text
clp_container              malformed container, or missing _FILES_CLASSICLADDER
clp_member                 a required member is missing
clp_version                an unreadable #VER
clp_rung_syntax            a cell that does not parse
clp_variable               a variable reference with no faithful mapping
clp_indexed                indirect addressing
clp_counter_bidirectional  a counter with both up and down wired
clp_counter_instances      more than 4 counters of one direction
clp_counter_reset          down-counter reset to zero with a non-zero preset
clp_counter_overflow       empty/full outputs are read somewhere
clp_counter_preset_load    preset-load wired on a counter mapped to CTU
clp_counter_edge           edge semantics LiNC cannot reproduce
clp_legacy_timer           legacy timer whose extra contacts are wired
clp_timer_contact          an IEC-timer contact beyond Q
clp_timer_instances        more than 4 timers of one kind
clp_branch                 a branch topology LiNC cannot express
clp_rung_chain             the NextRung chain is broken, cyclic, or skips a rung
clp_sfc                    a sequential section
clp_modbus                 Modbus distributed I/O configured
clp_expression             an expression the parser cannot read
clp_counter_wrap           warning only: preset above 9,999, where ClassicLadder
                           wraps and LiNC saturates
```

The four facts that are easy to get wrong, all encoded in `clp.rs` and worth
keeping true: rung order is the `NextRung` chain and never the file numbering; a
counter's `done` is equality while `CTU.Q` is `>=`; expressions store operands as
`@type/number@`; a jump targets a rung number and a call a subroutine number.

## Appendix B — licence rules

- LiNC is `GPL-2.0-or-later` (`Cargo.toml`, `debian/copyright`).
- LinuxCNC is `GPL-2.0-or-later` tree-wide except two scripts
  (`emc/ini/linuxcnc_check_ini.py`, `hal/drivers/mesa-hostmot2/mesambccc.py`),
  neither used here. ClassicLadder is `LGPL-2.1-or-later`. No compatibility
  problem either way.
- `debian/copyright` declares `subroutines/probe_basic/*` as `GPL-3.0` while the
  workspace is `GPL-2.0-or-later`. That inconsistency predates this work; resolve
  it (it is metadata, one stanza) before any redistribution.
- **Take ideas, never code, from GPL-3.0-only implementations.** Read the design,
  write the Rust. This is the rule already recorded for OpenPLC, and it applies
  unchanged to any other GPLv3 PLC codebase.

## Appendix C — verification

```sh
cargo test -p linc-plc-core -p linc-plc-client -p linc-plc
cargo test -p linc-ui
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p linc-plc
```

Current state: 198 tests pass across `linc-plc-core`, `linc-plc` and
`linc-plc-client`.

Manual smoke path (`docs/plc.md`): start `linc-plc --no-hal`, then `Hello`,
`Load`, `Start`, `Snapshot`, `Reload` while running.

## Appendix D — review checklist

- [ ] No `SCHEMA_VERSION` bump; new fields are `#[serde(default)]`.
- [ ] A version bump, if any, is stated in the constant's doc comment.
- [ ] `Vm::scan` still allocates nothing, locks nothing, does no I/O.
- [ ] Every refusal has a code, a path, and a sentence an operator can act on.
- [ ] A test exists for the refusal, not only for the happy path.
- [ ] Dialect knowledge did not spread: it goes behind `Profile`, not into
      `compile.rs`, `validate.rs` or `vm.rs`.
- [ ] `docs/plc.md` matches what the code now does.

## Appendix E — the PLCYard acceptance corpus

A set of 41 real ladder programs, used as the acceptance target for the PLC work.
They are not a port: they are programs that must eventually **run and match a
recorded trace**, and today they fail for known and named reasons.

### Licence, and why the corpus is a separate directory

The source programs are **GPL-3.0-or-later**, and conversions of them are
derivatives, so they stay GPL-3.0-or-later. LiNC is GPL-2.0-or-later. The two
coexist only as **aggregation** — separate programs on one medium, neither
compiled into the other:

```text
examples/plc-acceptance/
  LICENSE          GPLv3, verbatim
  README.md        provenance, licence rules, how to run a comparison
  manifest.json    all 41 sampled: 12 ladder, 29 excluded, per-file status
  sources/*.plcy   the programs, verbatim: the authoritative definition
  *.xml            PLCopen TC6, for the three that convert today
  stimuli/*.json   the inputs each program is driven with, and for how long
  traces/*.json    expected outputs per scan, produced by the oracle below
```

The directory is named for what it is used for, not for where the programs came
from. Provenance still has to be recorded — in `README.md` and in `manifest.json`
— because that is what the licence asks for. A name that hides the source would
be worse than one that mentions it.

The rules that keep that boundary real, matching how `subroutines/probe_basic/`
already sits in this tree:

- its own `Files:` stanza in `debian/copyright`, stating the derivation;
- nothing in `crates/` may reference the directory — no `include_str!`, no build
  script, no test that embeds a fixture into the test binary. Tests may **read**
  it at run time;
- `Cargo.toml` stays `GPL-2.0-or-later`. That line does not change.

### What is in it, measured

| | count |
| --- | --- |
| sampled files | 41 |
| ladder only — LD, no FBD, no ST | 12 |
| excluded: ST only | 19 |
| excluded: FBD only | 7 |
| excluded: FBD + LD mixed | 3 |
| convert to TC6 today | 3 |
| blocked, with a named cause | 9 |

All twelve ladder programs ship as `sources/*.plcy`, verbatim, so the corpus holds
the whole ladder target set even where LiNC cannot express it yet. Three ship as
`.xml` as well. Every one of the twelve ships a stimulus and a trace, including
the nine that cannot run: the trace is the reference behaviour the program owes if
and when it becomes expressible.

### The nine, and what each needs

| program | blocked by | what it would take |
| --- | --- | --- |
| `silo` | two coils on parallel branches, one rung | `coils: Vec<Coil>` in `Rung` |
| `traffic-lights` | six chained timers | `FB_PER_TYPE` is 4; six instances needed |
| `start-stop` | `ONS` one-shot; `Bits[0].0` bit operand | rising-edge contact; bit-of-word operands |
| `timer-counter` | `RES` coil; `.TT` is not `.Q` | `RES` → the counter's `R` input; a timing member |
| `recipes-data-type-ld` | `Recipe[3]` struct array; field presets | typed data, III.6 |
| `traffic-lights-routines` | `DINT[6]`; JSR routines; DINT counter | typed data plus subroutines |
| `two-tanks-cascade` | `PID`, `SCP`, `MOV`, `REAL` | analogue blocks and real tags |
| `motor-starters-function-block` | user FB called with formal parameters | user POUs and parameter binding |
| `silo-function-block` | FB instance data (`FillStation`) | same |

### Wrong today, and why

| gap | consequence |
| --- | --- |
| `Rung` holds exactly one `Coil` | `silo` cannot be expressed; and a block feeding no coil is silently dropped, not refused |
| the same field | a rung with two coils cannot be expressed at all |
| `plcopen.rs` binds every `<block>` to instance 0 | `garage-door` has three blocks, two of them timers; they collide on `TON0` |
| no `PouLanguage::St` (III.1) | 19 of the 41 sampled files are out of reach |
| no typed data or arrays (III.6) | 4 of the 12 ladder programs |

The drop is the trap worth naming: `plcopen::import` walks backwards from each
`<coil>`, so a `<block>` that feeds no coil is dropped rather than refused. That is
data loss, not a diagnostic, and a test asserting only "imports cleanly" would
pass while the timer never ran. `start-stop-delay` is the smallest case: five
rungs, and the 2 s timer is the one that disappears.

`coils: Vec<Coil>` in `Rung` (or `Option`, with validation forbidding more than one
until the compiler and editor can hold several) is the first of these and the
cheapest; it unblocks the most programs per line changed.

### The corpus found a converter bug

`MEMBER = {..., "TT": "Q"}` maps a timer's `.TT` (timing) onto `.Q` (done) —
opposite conditions, so a rung using `.TT` would be inverted. `timer-counter`
refuses at `RES` before reaching it, which is why it has not surfaced. The
converter must refuse `.TT` until there is a distinct timing member to map it to.

### The oracle

Traces are produced by running the sources in the reference engine, headlessly,
out of tree, and are **data** — numbers a program settled to, not code. The
procedure, so it can be reproduced:

1. Unpack the reference package from its archive; it is kept outside the
   repository and rebuilt on demand.
2. `Processor(profile)`, `download(program)`, then `set_mode(RUN)`; its scan
   period is 10 ms, so one `tick(10)` per scan.
3. Apply each stimulus change before the tick for that scan index.
4. Read every `%Q` tag the program writes after each scan and keep only the
   scans where something changed.
5. Stop at the first fault and record it, so the trace ends where the run did.

The comparison is a trace, not a final state: a one-scan difference is a real
difference, and this corpus already found that class of bug — a ClassicLadder
counter's `done` is equality while `CTU.Q` is `>=`.

Known limitation: a trace is only as right as the engine that produced it. Where
the reference and IEC disagree, IEC wins and the trace is wrong; record the
disagreement rather than editing the trace to match.

### Harness contract

- Assert **values per scan** against the trace, not "imports without error".
- Replay: every output starts at `"0"` and takes the last change at or before the
  scan under test.
- Assert the diagnostic **code** for every blocked file, so a refusal is a
  contract and a change of code is visible.
- Assert the blocked set by name, so a file that starts converting is noticed
  rather than quietly passing.
- Mark blocked files with a reason, so the suite is green while being accurate,
  and remove the marker when the gap closes.
- Read the directory at run time. Nothing embedded, nothing `include_str!`.
