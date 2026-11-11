# PLC interop handoff — what to take from openplc-editor

Status: decision recorded, one implementation item proposed, none started
Date: 2026-11-11
Target: LiNC `main` @ `16b4c4b` reviewed, HEAD `aeb1eed` at write time
License: GPL-2.0-or-later
Author: session that evaluated <https://github.com/Autonomy-Logic/openplc-editor>
against the PLC stack in this repo

---

## 0. Decision

**Do not adopt openplc-editor.** It is not a drop-in and not a code donor for the
PLC stack here. Continue `crates/linc-plc-core`, `crates/linc-plc`,
`crates/linc-ui/src/screens/plc*.rs`.

One durable asset is worth taking: the **PLCopen TC6 XML (IEC 61131-10) exchange
format** — a standard, not their code. Section 3 is the proposed work item.

Do not re-open the adoption question unless one of the conditions in §7 appears.

## 1. Upstream fact sheet (verified from the repo, not the README)

| | |
|---|---|
| Licence | `GPL-3.0-only` (`package.json`); baremetal runtime dual-licensed with a paid commercial option |
| Version | 4.3.4 |
| Stack | Electron ^35, React ^18.3, TypeScript ^5.4, Monaco ^0.54, `@xyflow/react` ^12 (React Flow), zustand, Tailwind 3 |
| Toolchain | Node ^22, npm, webpack, Playwright, Jest; `arduino-cli` binaries vendored per platform in `resources/bin/` |
| Repo size | ~1.06 GB (API-only access; not cloned here) |
| Runtime targets | OpenPLC runtime v3/v4 (Linux/Windows) and baremetal Arduino firmware; Modbus and EtherCAT I/O; cloud project sync over an HTTP API |
| Languages | LD, FBD, SFC, ST, IL |
| Compiler path | graphical → ST (`src/backend/shared/transpilers/st-transpiler/`), then a C++/Arduino toolchain; a newer `strucpp` compiler is in migration (`docs/strucpp-migration/`) |
| Project format | **`.ld` files are React-Flow JSON**, not XML (`e2e/fixtures/project-search/motor_ladder.ld`). PLCopen XML appears only in `configs/mocks/plcMock.xml`, namespace `http://www.plcopen.org/xml/tc6_0201` |

That last row corrects an easy assumption: their on-disk ladder is a
React-Flow node/edge graph with handle coordinates (`numericId`, `executionOrder`,
`glbPosition`), specific to their editor. It is not an interchange format and
nothing should be imported from it.

## 2. Why adoption is rejected

1. **Runtime target differs.** They compile to ST and then to C++ for the OpenPLC
   runtime or an Arduino. `linc-plc-core` lowers to its own `Op` bytecode and runs
   a deterministic scan loop in `vm.rs` with no allocation, locks or I/O. The
   compiler, the validator and the scan engine would all be discarded; only the
   editor surface is shared, and that surface is React.
2. **Process model differs.** Their app is Chromium + Node + a runtime + an I/O
   bridge. This repo's stated design (`README.md`, `docs/plc.md`) is in-process,
   "no helper process, no Python runtime and no web service between the screen and
   LinuxCNC", 2 ms scan, `linc_plc.*` HAL pins owned by `linc-plc`. Shipping
   Chromium on the controller is the opposite direction.
3. **Dialect differs.** This PLC is ZX-PLC-flavoured: nine element positions plus
   a coil position (`grid::POSITIONS`), a fixed FB set (TON/TOF/TP/CTU/CTD/SR/RS/
   R_TRIG/F_TRIG, four instances each, numbered as the ZX manual numbers them),
   symbolic I/O instead of `%IX/%QW`. Their feature list (SFC, EtherCAT
   configurator, Modbus groups, debugger over sockets, PDF export, vendor
   configurators) is mostly out of scope here.
4. **Licence.** `GPL-3.0-only` combined with this repo's `GPL-2.0-or-later` makes
   the combined work GPLv3. Legal, but it changes the distribution terms of the
   whole binary (patent grant, anti-tivoisation). Importing a file *format* is
   not affected — see §5.

Scale sanity check: the PLC stack here is ~13,750 lines (`linc-plc-core` 3,900,
`linc-ui` PLC screens 5,955, daemon/client/tests the rest). Switching is a
rewrite in another language plus a HAL bridge, not a port.

## 3. Item to take: a PLCopen TC6 XML importer

### Why

The project schema is a private JSON (`Project`, `SCHEMA_VERSION = 1`,
`serde` on `crates/linc-plc-core/src/model.rs`). Every ladder that exists on the
mill today, and every ladder an operator brings from another IDE (Beremiz,
CODESYS, OpenPLC v3, most PLCopen-compliant tools), is PLCopen XML. A read-only
importer turns "retype the rung" into "open the file". That is the one thing this
extern ecosystem genuinely offers.

### Shape

New module `crates/linc-plc-core/src/plcopen.rs`, one dependency (`quick-xml`;
the workspace has no XML parser today — `grep xml` over the manifests is empty).

Import into `Logic`, **not** into `grid::Cell`. `model.rs` documents the path:
`Rung::cells` empty means "the rung is still the version-2 tree in `logic`; a
non-empty grid is lowered by the compiler". So the importer produces the tree and
the existing lowerer handles the rest. No cell quantisation, no geometry.

Walk from each `<rightPowerRail>` backwards through `<connectionPointIn>`: one
`<connection>` child is series, several are `Logic::Parallel`, recursion bottoms
out at `<leftPowerRail>` → `Logic::Const { value: true }`.

| PLCopen TC6 element / attribute | this repo |
|---|---|
| `<project>` `contentHeader/@name` | `Project::name` |
| `<pou pouType="program">@name` | `Pou::name`; first program becomes `MAIN` |
| `<documentation>` (xhtml `p`) | `Pou::comment` / `Rung::comment` (plain text, markup stripped) |
| `<pou><body><LD>` | `PouLanguage::Ld`, `Pou::sections` |
| `<contact negated="false|true"><variable>` | `Cell`-less `Logic::Contact { var, negated }` |
| `<coil negated storage="none\|set\|reset"><variable>` | `Coil { var, kind }` → `Normal`/`Negated`/`Set`/`Reset` |
| `<block typeName="TON\|TOF\|TP\|CTU\|CTD\|SR\|RS\|R_TRIG\|F_TRIG">` | `FbKind`; instance from `@localId`, preset from the `PT`/`PV` input |
| `<leftPowerRail>` / `<rightPowerRail>` | rung boundary, `Logic::Const { value: true }` |
| `<localVars><variable name><type><BOOL/>` | `VarDecl` (BOOL → `VarKind::Bit`) |
| `%IX`/`%QX` `@address` | `IoPoint` + `Binding`, opt-in — see §4 |

### Refuse, never approximate

Any element or attribute that has no mapping must fail with a diagnostic, not be
dropped. Use the existing `Diagnostic`/`Severity` (`validate.rs`), new codes
prefixed `plcopen_`:

- `plcopen_unsupported_body` — `<ST>`, `<SFC>`: `PouLanguage` has IL, LD, FBD only.
- `plcopen_unsupported_element` — `jump`, `return`, `label`, `connector`,
  `continuation`, `compareContact`, `inOutVariables` on a block: refuse by name.
- `plcopen_multi_coil` — a rung whose right rail is fed by more than one coil.
  `Rung` holds exactly one `Coil`.
- `plcopen_unreachable` — a `<coil>` or `<contact>` not connected to a rail.
- `plcopen_cycle` — a connection loop; `connectionPointIn` is user data.
- Anything refused also means the whole import fails (all-or-nothing) so a
  partial ladder is never loaded into a live PLC.

Then run `validate()` on the result and surface its diagnostics too. Two coils on
one target in a section already fails with `duplicate_coil` (`validate.rs:706`),
which is exactly the rule that makes the upstream coil-grouping bug
unrepresentable here — the importer gets that check for free.

### Acceptance tests

In `crates/linc-plc-core/tests/plcopen.rs`, fixtures authored by hand (three
small `.xml` files, no copied upstream material):

1. `motor.ld.xml` — left rail → contact `Motor` → coil `Lamp` → right rail.
   Import succeeds, `validate()` returns no errors, `compile()` produces the
   one-rung program, and a two-scan `Vm` run drives `Lamp` from `Motor`.
2. `parallel.ld.xml` — two contacts in parallel, one block `TON` in series.
   Tree shape asserted as `Logic::Series([Logic::Parallel([..]), Block])`.
3. `refuse.ld.xml` — an `<SFC>` body. Import returns exactly one
   `plcopen_unsupported_body` diagnostic and no `Project`.
4. `set-coil.ld.xml` — `<coil storage="set">` maps to `CoilKind::Set`;
   a second coil on the same variable yields `duplicate_coil`.

### Effort

One day, bounded: ~350-450 lines of parser plus fixtures. Optional second day for
an exporter, and the exporter should emit only what round-trips (LD rungs, BOOL
symbols, the nine FB kinds), refusing the rest by the same rule.

## 4. Optional, deferred: `%` addressing

They address I/O as `%IX0.0` / `%QW1` (`docs/iec-address-registry.md`). This PLC
uses symbolic names and a ZX numbering (`Binding`, `Symbol { index, kind }`).
Do not add a second addressing scheme to the runtime. If imported files carry
addresses, translate them in the importer: an `@address` becomes an `IoPoint`
named after the address with the numeric index recovered from it, and the
operator renames them in the Symbols page. Nothing in `vm.rs` or `compile.rs`
changes.

## 5. Licence rules for this work

- Copy **no** source from openplc-editor. It is `GPL-3.0-only`; this repo is
  `GPL-2.0-or-later` and the combined binary would have to become GPLv3.
- Implement the importer from the PLCopen TC6 schema and the IEC 61131-10 text,
  which are published specifications. Element and attribute names are facts about
  a format, not copyrightable expression.
- Do not vendor their fixtures. Write the four test files above by hand.
- Reading their documents for background is fine and does not taint the work.

## 6. Reading list — background, no code to port

- `docs/transpiler/coil-reevaluation-836.md` — parallel SET/RESET coils sharing
  one energisation. **Not applicable here:** one `Coil` per `Rung` plus the
  `duplicate_coil` rule makes the topology unrepresentable. Skip unless the rung
  model ever grows a second coil.
- `docs/transpiler/block-output-ordering-830.md` — evaluation order of block
  outputs. Worth a read only if FB ordering in `compile.rs` is ever questioned.
- `docs/iec-address-registry.md` — the `%` address layout, for §4.
- `docs/strucpp-migration/*.md` — how they moved to a new compiler. Process
  reading, not engineering input.

Their 193-project LD corpus (`docs/transpiler/coil-reevaluation-836.md` cites
`~/src/xml2st/fixtures/golden/real_projects_json`) is **not in the repository**;
its availability is unverified. Treat it as unavailable. If it ever appears, it
is test *data* for the importer — do not check it in.

## 7. What would change the decision

Re-open only if one of these becomes true:

1. The machine needs full IEC 61131-3 authoring — SFC and ST with a code
   generation target — not ZX-PLC ladder.
2. Running Chromium and Node on the controller becomes acceptable, i.e. the
   in-process constraint in `README.md` and `docs/plc.md` is dropped.
3. The PLC must exchange programs with third-party PLCs over Modbus/EtherCAT
   rather than drive HAL pins.
4. The Rust scan VM is abandoned for a C++ runtime.

Otherwise: keep the Rust stack, add the importer if interop is wanted.

## 8. Notes for whoever picks this up

- Working tree is active: another agent is committing in this checkout (HEAD moved
  from `16b4c4b` to `aeb1eed` during this review) and `git status` shows
  uncommitted work in `crates/linc-plc-core/src/{compile,model,validate}.rs` and
  `tests/compile_vm.rs`, which reads as FBD-grid work in progress. This document
  is standalone; commit it on its own and do not branch over that work.
- Doc drift found during this review: `model.rs:18` and `:24` describe "version-2
  projects" carrying `pous`, but `SCHEMA_VERSION` is `1` (`lib.rs:32`) and
  `validate.rs:85` rejects `schema: 2` outright while still decoding `pous` at
  schema 1. The comments describe a shape, not a schema revision. An importer
  must write `schema: 1`.
- `grid.rs` documents that `Compare` and `Compute` cells parse but are refused by
  validation until their semantics exist, and `Edge` needs a previous-scan value
  the VM does not carry. Importing a PLCopen file that uses edge contacts or
  compare contacts therefore hits a known-refused path — refuse them explicitly
  with `plcopen_unsupported_element` rather than emitting a cell the compiler
  rejects later.
