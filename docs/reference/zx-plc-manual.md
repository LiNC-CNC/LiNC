# ZX-PLC User Manual

ZX-PLC is a PLC programming environment for the **ZX Spectrum 128K** (also +2 / +2A / +3). It edits
programs in three IEC 61131-3 languages: Instruction List (IL), Ladder Diagram (LD) and Function Block
Diagram (FBD). It runs them in a built-in simulation with live monitoring, a watch / force table and
virtual inputs and outputs. Projects are saved to and loaded from tape.

The screenshots in this manual come from the program itself; `make docs` regenerates them.

## Contents

1. [Getting started](#1-getting-started)
2. [Screen and keys](#2-screen-and-keys)
3. [Menus](#3-menus)
4. [Project and POUs](#4-project-and-pous)
5. [Operands and symbols](#5-operands-and-symbols)
6. [IL editor](#6-il-editor)
7. [LD editor](#7-ld-editor)
8. [FBD editor](#8-fbd-editor)
9. [Symbol editor](#9-symbol-editor)
10. [Simulation](#10-simulation)
11. [Watch and force table](#11-watch-and-force-table)
12. [Tools: undo, copy and paste, cross references, add to watch](#12-tools-undo-copy-and-paste-cross-references-add-to-watch)
13. [The demo project](#13-the-demo-project)
14. [Reference](#14-reference)

---

## 1. Getting started

Build the snapshot with `make` (needs `sjasmplus`) and load `zxplc.sna` into Fuse (`make run`) or into
any emulator of the ZX Spectrum 128K. The program starts with the **demo project** in the POU list.

![POU list with the demo project](img/pou_list.png)

On a real ZX Spectrum 128K / +2 / +2A / +3, load `zxplc.tap` from the
[releases](https://github.com/Pomaser/zx_plc/releases) with *Tape Loader* in the 128 menu (or `LOAD ""`). The
tape takes under 4 minutes, because its blocks are packed with ZX0. The loading screen shows the PLC.
When everything is loaded, its display changes from STOP to RUN, the LEDs start blinking and "Press any
key to start" appears.

Without a Spectrum, [the browser version](https://pomaser.github.io/zx_plc/) loads the same tape in the
JSSpeccy emulator (keyboard only: Esc or Tab opens the menu, Ctrl is SYMBOL SHIFT; saving to tape does not
work there).

To try it at once:

1. Move to `DEMO_IL` with the cursor keys and press ENTER to see its program.
2. Open the menu with BREAK (CAPS SHIFT + SPACE) or EDIT (CAPS SHIFT + 1) and choose **Run > Run**.
3. Press `0` to switch on the input `%IX0.0`: the traffic light starts.

## 2. Screen and keys

* **Menu bar** (top): `File  POU  Edit  View  Run  Help`. The name and language of the current POU are
  shown on the right.
* **Status bar** (bottom):
  * In the editors, the left part shows the symbol (inverted) and the comment of the operand at the
    cursor; the right part shows the free project memory (`Free 9906B`).
  * In the simulation, it shows the keys on the left and RUN / STOP on the right.
* **Popups** show messages, questions and input fields. Any key closes a message.

The cursor keys are CAPS SHIFT + 5 / 6 / 7 / 8, DELETE is CAPS SHIFT + 0. Combinations with SYMBOL SHIFT
(SS) jump or start tools:

| Key | Function |
|---|---|
| BREAK (CS+SPACE), EDIT (CS+1) | open / close the menu |
| Arrows | move; in a menu: other title / item |
| SS + LEFT / RIGHT | start / end of the line (LD / FBD: first position / output column) |
| SS + UP / DOWN | one screen up / down (POU list: first / last POU) |
| SS + ENTER | IL: new line below; LD / FBD: start of the next row |
| ENTER | confirm, open, edit |
| DELETE | delete |
| letters | hotkeys of menu titles, items and elements |
| SS + U | undo the last change |
| SS + Y | redo the undone change |
| SS + X | cross references of the operand at the cursor |
| SS + W | add the operand at the cursor to the watch list |
| SS + I | LD: insert an element in front of the cursor |
| SS + A | mark the start of a block (again: remove the mark) |
| SS + D | copy the block (or the current line / rung / network) |
| SS + F | paste in front of the cursor |

**Help > Controls** shows a short list of the keys:

![Help > Controls](img/help_controls.png)

The AY chip plays a soft click for every key and a short warning tone on errors (Options > Sound switches
them off).

**Kempston mouse.** Options > Mouse switches a Kempston mouse on (it is off after the start). It is only
switched on when a mouse answers, otherwise "No Kempston mouse" is shown. A small arrow follows the mouse:

* a click on a menu title opens the menu, a click on an item runs it, a click elsewhere closes it;
* in the element pickers, the operator list and the language choice a click picks an item; outside or
  the right button cancels;
* in a yes / no question a click on "Y = yes" or "N = no" answers; in an input line the left button is
  ENTER and the right one cancels; a message closes on a click;
* in the editors a click puts the cursor on the line, field or element under it, and a click on the
  cursor itself (or the right button) is ENTER;
* in the POU list a click selects a POU and a second click opens it;
* in the simulation a click on an input LED toggles that input.


## 3. Menus

Each menu title opens a drop-down list. The first letter of a title or item is its hotkey.

### File

![File menu](img/menu_file.png)

| Item | Function |
|---|---|
| New project | empty project with an empty MAIN (asks first) |
| Open demo | loads the demo project (asks first) |
| Load tape... | loads a project from tape |
| Save tape... | saves the project to tape |
| Load SD... | loads a project from the SD card (divMMC) |
| Save SD... | saves the project to the SD card (divMMC) |

### POU

![POU menu](img/menu_pou.png)

| Item | Function |
|---|---|
| Manage | the POU list |
| New... | new POU: name, language, comment |
| Rename/Comment... | rename the selected POU (not MAIN) and edit its comment |
| Language... | change the language of an empty POU |
| Duplicate... | a copy of the selected POU under a new name (also `U` in the POU list) |
| Delete | delete the selected POU (not MAIN) |

### Edit

![Edit menu](img/menu_edit.png)

| Item | Function |
|---|---|
| Edit POU | open the current POU in its editor |
| Undo | undo the last change (also SS+U) |
| Redo | redo the undone change (also SS+Y) |
| Add to watch | add the operand at the cursor to the watch list (also SS+W) |
| Symbols | the symbol editor |
| Insert line | insert an empty line / row |
| Delete line | delete the current line / row |
| Mark block | mark the start of a block (also SS+A) |
| Copy | copy the block (also SS+D) |
| Cut | copy the block and delete it |
| Paste | paste in front of the cursor (also SS+F) |

### View

![View menu](img/menu_view.png)

| Item | Function |
|---|---|
| Symbols | show operands by their symbol names |
| Addresses | show operands by their addresses (default) |
| Cross references | uses of the operand at the cursor (also SS+X) |

### Run

![Run menu](img/menu_run.png)

| Item | Function |
|---|---|
| Run | compile all POUs and run (the simulation screen) |
| Stop | stop the scan cycle |
| Step | one scan |
| Reset | clear all PLC data (inputs, outputs, memory, timers, counters, steps) |
| Watch | the simulation on the watch / force page |
| Trend | the simulation on the trend page (time charts) |

### Options

![Options menu](img/menu_options.png)

| Item | Function |
|---|---|
| Sound | key click and error tone on / off |
| Mouse | Kempston mouse on / off (only when one is found) |
| UR-4 I/O | real inputs and outputs through a UR-4 interface on / off (only when one is found, see [10](#10-simulation)) |

### Help

![Help menu](img/menu_help.png)

**About** shows the version:

![Help > About](img/help_about.png)

## 4. Project and POUs

A project has up to 8 POUs (Program Organisation Units) and up to 12 KB of memory. **MAIN** always exists
and is the only POU the scan cycle runs; it calls the other POUs (`CAL DEMO_IL`, up to 8 levels deep).

**The POU list** (POU > Manage) shows the name, language, size in bytes and comment of every POU.
UP / DOWN select a POU and ENTER opens it. `N` (new), `R` (rename / comment), `L` (language) and `D`
(delete) work directly.

**A new POU** asks for:
* its name: up to 8 characters A-Z, 0-9, `_`; it must not be an address, keyword, FB instance or symbol;
* its language;
* a comment of up to 20 characters.

![New POU: name](img/pou_new.png)

![New POU: language](img/pou_lang.png)

### Tape

**File > Save tape...** asks for a tape name (up to 10 characters) and starts on a key press. The project is
written as a standard CODE file (start 0x9000), so any tape tool can copy or check it. SPACE breaks off.

![File > Save](img/file_save.png)

**File > Load tape...** waits for a project on the tape and skips other files. If loading fails or is broken
off, the old project comes back. Projects saved by older versions (format 1, without the watch list) are
converted when loaded.

### SD card (divMMC)

**File > Save SD...** and **File > Load SD...** work with a **divMMC** interface running **esxDOS**
(without it they report "No esxDOS (divMMC) found"). In an emulator the divMMC needs the esxDOS ROM
in its EEPROM, write protection on (otherwise there is no automapping) and an SD card image with the
esxDOS `SYS` folder.

* **Save** asks for a file name of up to 8 characters (A-Z, 0-9, `_`, `-`) and writes `NAME.PLC` to the
  current folder of the card; an existing file is only overwritten after a question.
* **Load** lists the `.PLC` files of the current folder (up to 16); choose one with UP / DOWN and ENTER
  (or a click). A file that is not a ZX-PLC project is refused and the old project stays.
* The file holds the same bytes as the data block on tape, so a project can be moved between tape and
  card.

## 5. Operands and symbols

| Area | IEC form | Short form | Range |
|---|---|---|---|
| Inputs | `%IX0.0` | `I0.0` | 0.0 - 1.7 (16) |
| Outputs | `%QX0.0` | `Q0.0` | 0.0 - 1.7 (16) |
| Bit memory | `%MX0.5` | `M0.5` | 0.0 - 7.7 (64) |
| Step flags | `%SX0.2` | `S0.2` | 0.0 - 7.7 (8 bytes) |
| INT words | `%MW3` | `MW3` | 0 - 15 |
| System flags (read only) | `%SM6` | `SM6` | 0 - 7 |
| System words (read only, TIME) | `%SW0` | `SW0` | 0 - 2 |
| Constants | `TRUE`, `FALSE`, `-123`, `T#1s500ms`, `T#2m` | | |
| FB instances | `TON0`-`TON3`, `TOF0`-`TOF3`, `TP0`-`TP3`, `CTU0`-`CTU3`, `CTD0`-`CTD3`, `RT0`-`RT3`, `RF0`-`RF3`, `SR0`-`SR3`, `RS0`-`RS3` | | |
| FB members | `TON0.Q`, `TON0.ET`, `CTU1.CV`, `RT2.CLK` ... | | |

* Operands can be typed in lower case and in the short form. They are stored and shown in the IEC form,
  or by the symbol name with View > Symbols.
* **System flags** are written before every scan:

  | Flag | Meaning |
  |---|---|
  | `SM0` | first scan after Run / Reset |
  | `SM1` | always TRUE |
  | `SM2` | always FALSE |
  | `SM3` | 10 Hz clock |
  | `SM4` | 5 Hz clock |
  | `SM5` | 2 Hz clock |
  | `SM6` | 1 Hz clock |
  | `SM7` | 0.5 Hz clock |

* **System words** hold the scan cycle time, as TIME values (20 ms steps):

  | Word | Meaning |
  |---|---|
  | `SW0` | the last scan cycle (the time since the scan before) |
  | `SW1` | the longest cycle since Reset |
  | `SW2` | the shortest cycle since Reset |

  Add them to the watch list to see how fast the simulation runs (the screen updates take part of the
  time), or compare them, e.g. `LD %SW0` / `GT T#100ms`.

* **Step flags**: in each step byte `S0`-`S7` only one bit can be TRUE. Setting a step clears the other
  steps of its byte, which makes step sequences easy (see DEMO_IL).
* **Symbols** (Edit > Symbols) give operands names. A symbol can be typed instead of an address
  anywhere.
* The time base is 20 ms (one scan per frame).

## 6. IL editor

![IL editor (DEMO_IL)](img/il_editor.png)

Each line holds one instruction, a label (`LOOP:`) or a comment (`// text`, dark green). The line number
is on the left, the instruction in dark blue and the operand in black. The status bar shows the symbol
and comment of the operand.

* Type the instruction and press SPACE: a known instruction is written in upper case and the cursor moves
  to the operand.
* ENTER stores the line and checks it; an error is shown in a popup and the cursor stays on the line.
  At the very start of a line, ENTER inserts an empty line above it.
* SS+ENTER opens a new line below. UP / DOWN store the line and move. DELETE on an empty line removes it.
* Instructions:

  ```
  LD LDN ST STN S R AND ANDN OR ORN XOR XORN NOT ADD SUB MUL DIV MOD
  GT GE EQ NE LE LT JMP JMPC JMPCN CAL CALC CALCN RET RETC RETCN )
  ```

  The `(` modifier is allowed where IEC allows it (`AND(`, `ADD(`).
* FB call: `CAL TON0(IN:=S_Red, PT:=T#4s)`; POU call: `CAL DEMO_LD`.

## 7. LD editor

![LD editor (DEMO_LD)](img/ld_editor.png)

A rung runs from the left rail through 9 positions for contacts, wires and boxes to the coil position
and the right rail. The operand is written above each element. The status bar shows the symbol and
comment of the element (for boxes: the instance and its PT / PV).

**ENTER** opens the element picker. The current element is preselected, so ENTER also changes an element:

![LD element picker](img/ld_picker.png)

| Key | Positions 0-8 | Coil position |
|---|---|---|
| `C` | normally open contact | coil |
| `/` (SS+V) | normally closed contact | negated coil |
| `P` / `N` | rising / falling edge contact | |
| `S` / `R` | | set / reset coil |
| `-` (SS+J) | wire | |
| `V` | vertical link at the right edge (parallel branch) | |
| `T` / `F` / `U` | TON / TOF / TP timer box | |
| `K` / `D` | CTU / CTD counter box | |
| `G` | compare contact `MW1>5` | |
| `M` | | compute coil `MW2:=MW1+5` |

* **Compare and compute** work with words (`MW`, numbers, `T#` times, `ET` / `CV` / `PT` / `PV` of FB
  instances). After `G` / `M` a list offers the operator, then the values are asked one by one:
  * compare: GT, GE, EQ, NE, LE, LT (`A > B` ... `A < B`), then A and B. The contact passes the power when
    it is true; in the simulation it is green while true.
  * compute: ADD, SUB, MUL, DIV (`D := A + B` ...) or MOVE (`D := A`), then A, B and the result D (an
    `MW`). It runs in every scan in which the coil gets power.
  * The expression (`CTU0.CV>=3`, `MW2:=MW2+1`) is the label, the operator is shown in the symbol (`-[>]-`,
    `-[+]-`), the status bar shows it in full. ENTER and the same key edit it, starting from its values.
* **Placing elements:**
  * After a new element the cursor moves on.
  * Empty positions on the left are filled with wires.
  * SS+RIGHT jumps to the coil position and SS+ENTER to the next row.
* **Timer and counter boxes:**
  * The power from the left is the input (IN, CU / CD) and Q powers the rest of the rung.
  * The box asks for the instance and the preset: PT for timers (`T#4s`), PV for counters (a number).
  * `T` on a box edits only its preset.
  * Reset or load a counter with a coil on `CTU0.R` / `CTD0.LD`.
* **Branches:**
  * `V` toggles a vertical link that joins the row below.
  * A new branch inserts an empty row below if the next row belongs to another rung or is a comment.
* **Insert:** SS+I picks an element and inserts it in front of the cursor. The whole rung moves one
  position to the right.
* **Delete:** DELETE removes the element and the row closes up to the left. Where links would lose their
  place, a wire takes the element's place instead.
* **Comments:** `//` at the start of an empty row writes a network comment. Typing on a comment row edits
  it in place.

## 8. FBD editor

![FBD editor (DEMO_FBD)](img/fbd_editor.png)

Blocks are placed on the same grid. Inputs and outputs are written on the signal line (`I1.0--`,
`--Q1.0`). A block with *n* inputs covers *n* rows: input *k* comes in on row *k* and the output leaves
on the top row, so blocks nest by feeding the lower inputs of the next block.

ENTER opens the picker. On a block input, outputs are not offered:

![FBD element picker](img/fbd_picker.png)

| Key | Element |
|---|---|
| `I` / `N` | input / negated input (the `o` is drawn at the block input) |
| `Q` / `W` | output / negated output (right behind a block, or in the output column) |
| `-` | wire |
| `A` / `O` / `X` | AND / OR / XOR with 2-4 inputs |
| `!` (SS+1) | NOT |
| `S` / `R` | SR / RS flip-flop |
| `P` / `E` | RT rising edge / RF falling edge |
| `T` / `F` / `U` | TON / TOF / TP timer (2 rows: IN, PT) |
| `K` / `D` | CTU / CTD counter (3 rows: CU, R or CD, LD, then PV) |
| `G` | compare block GT / GE / EQ / NE / LE / LT (2 rows: the value, the value to compare with) |
| `M` | compute block ADD / SUB / MUL / DIV / MOVE (2 rows: the values A and B, the result D on the right) |

* **Presets:** a timer has its preset PT on its second row and a counter its PV on its third row. The
  value is written on the left of the row (`T#2s--PT`, `3--PV`). `T` on the block or on that cell edits
  it:

  ![Editing the PV of a counter](img/fbd_pv.png)

* **Compare:** the block compares two words, written on the left of its rows (`MW3--` and `50--`), and its
  output is TRUE when the comparison holds (green in the simulation). Placing it only asks for the
  operator (from a list; the block is named after it, `LT`). The values are entered like inputs: ENTER on
  the cell left of a row asks for that value. ENTER on the block chooses the operator again. Run reports
  a missing value at the block.
* **Compute:** the block runs `D:=A op B` (`+` `-` `*` `/`, or `D:=A` for MOVE) in every scan. A and B
  are written on the left of its rows, the result D on the right of it (`MW3--ADD--MW3`). As with the
  compare block, placing it only asks for the operator; A and B are entered on the cells on its left and
  D with ENTER on the cell on its right (`-?` until then). The cell of D stays free.
* **Placing them:** the values need the cell on the left. At position 0, or when that cell is used, the
  block goes one position to the right; inside a network the network moves right, so a compare put on
  an input replaces it and feeds the block input. A compute block also needs its result cell. When
  nothing fits, "No room for the block" is shown and nothing changes.
* **Chaining:** a compare or compute block placed right behind the result of a compute block takes that
  result as its A (`MW3--ADD--MW3--GE`); A is then not asked.
* **The cursor** stands only on block inputs, blocks and outputs. It skips the cells inside a block.
* **Outputs and wires** need a signal from the left. Inputs placed left of a block input are wired to
  it automatically.
* **Networks:** FBD has no vertical links, so rows belong together only through blocks: a network is the
  rows that its blocks span (an AND with 2 inputs joins 2 rows, a counter 3). Rows that are only next to
  each other are separate networks, and moving one does not move the other.
* **Making room:**
  * A block placed on an input inserts itself behind it: the input feeds its first input and the network
    moves right.
  * Extra block inputs get room automatically. The network moves right or empty rows are inserted.
  * When nothing fits, "No room for the block" is shown and nothing changes.
* **Deleting a block:**
  * The signal of its first input goes on through a wire.
  * Whatever fed its other inputs is removed.
  * The network then closes up and empty rows below it disappear.

## 9. Symbol editor

![Symbol editor](img/symbols.png)

**Edit > Symbols** lists all symbols sorted by address. Each row holds the name (up to 8 characters,
dark blue), the address (black) and a comment (up to 30 characters, dark green).

* SPACE moves between the fields; ENTER, UP and DOWN store the row.
* The empty last row adds a symbol.
* Clearing the name and the comment removes a symbol, as does Edit > Delete line.
* Symbols can name inputs, outputs, memory bits, step flags, system flags and FB instances.

## 10. Simulation

**Run > Run** compiles all POUs and runs MAIN once per frame. A compile error opens the editor of that POU at
the faulty line or element; the message is shown in the status bar.

* The upper part monitors the current POU (or MAIN):
  * IL lines show the value of their operand on the right. `TRUE` is black on green. A `CAL` of a timer
    shows its running ET.

    ![Simulation of DEMO_IL](img/sim_il.png)

  * In LD, active elements are green:

    ![Simulation of DEMO_LD](img/sim_ld.png)

  * In FBD, the names of TRUE operands and the insides of TRUE blocks are green:

    ![Simulation of DEMO_FBD](img/sim_fbd.png)

* The lower panel shows the 16 inputs (green) and the 16 outputs (red) with their bit numbers. The half
  of the selected byte has a bright paper.

| Key | Function |
|---|---|
| `0`-`7` | toggle input bit 0-7 of the selected byte ("In") |
| SS + `0`-`7` | push button: the input is on while the key is held ("Set") |
| `B` | select byte 0 / 1 |
| `R` | run / stop |
| `S` | one scan (step) |
| `E` | reset all data |
| `W` | program / watch page |
| `G` | program / trend page |
| UP / DOWN | scroll the listing |

### Real inputs and outputs: the UR-4 interface

The simulation can read and drive real hardware through a **UR-4** interface (TESLA Kolín, an 8255
chip with three 8-bit ports). **Options > UR-4 I/O** switches it on; it is only switched on when the
interface answers, otherwise "No UR-4 interface" is shown. It is off after the start.

| UR-4 port | Address | PLC | Direction |
|---|---|---|---|
| PA | 31 | `%IX0.0`-`%IX0.7` | input |
| PB | 63 | `%IX1.0`-`%IX1.7` | input |
| PC | 95 | `%QX0.0`-`%QX0.7` | output |
| control word | 127 | 146 (PA, PB in, PC out) | |

* The inputs are read every frame and ORed with the inputs set by keys or mouse. A high pin (H) is TRUE.
* The outputs are written only while the simulation runs (RUN). In STOP, after a step, when the
  simulation screen is left and when the UR-4 is switched off they are all 0. `%QX1` stays simulated.
* The status bar shows `UR-4 RUN` / `UR-4 STOP` while the interface is on.
* When switched off, all ports are set back to inputs, as after a reset.
* A Kempston joystick interface also answers on port 31; do not connect both.
* The interface itself (ports, control words, connectors) is described in Czech in
  [manual_ur-4.md](manual_ur-4.md).

## 11. Watch and force table

`W` in the simulation (or **Run > Watch**) shows the watch list of the project. It holds up to 16 operands
with their symbols and live values and is saved with the project.

![Watch page with a forced output](img/sim_watch.png)

* UP / DOWN select a row.
* ENTER edits the operand of a row. The last row adds one; an empty text removes the row.
* DELETE removes a row.
* `T` switches to the trend page, `W` back to the program.
* `F` forces the selected operand:
  * a BOOL goes forced TRUE, then forced FALSE, then not forced;
  * an INT (`%MW`) asks for a value, and `F` again releases it.
* Forced values are marked `F`. They hold for the program and for the outputs until the simulation screen
  is left.
* Time values (ET) and counter values (CV) can be watched, but not forced.

### Trend

`G` in the simulation (or **Run > Trend**) shows the watch list as time charts, like a logic analyzer:
up to 8 traces (the first 8 suitable rows of the watch list), each with its symbol (or operand) on the
left, highlighted while it is at the top.

* A BOOL operand: TRUE is the upper line, FALSE the lower, dotted one.
* The elapsed time `ET` of a timer and the value `CV` of a counter: drawn between the two lines as a
  part of the preset (`ET` / `PT`, `CV` / `PV`); the upper line means the preset is reached.

![Trend page: the traffic light of the demo after TL_Start (`0`)](img/sim_trend.png)

The labels follow **View > Symbols / Addresses**: with symbols, an FB member is labelled with the symbol
of its instance (`T_Red` for `TON0.ET`); an operand without a symbol keeps its address.

* The samples are taken from the start of the simulation, also while another page is shown. The newest
  is at the right edge; when the chart is full it moves to the left.
* `W` switches to the watch page, `G` back to the program.
* `Z` changes the time window of the chart: 4 s, 20 s (the default) or 100 s. The history starts again.
* The inputs keep working (`0`-`7`, `R`), so a program can be tried out and its signals compared in time.
* Other words (`%MW`, constants) are not drawn. Operands are added on the watch page (`W`).

## 12. Tools: undo, copy and paste, cross references, add to watch

**Undo (SS+U, Edit > Undo)** returns the project to its state before the last key that changed it, for
example a deleted line, element or POU. Pressed again, it goes further back, step by step.
**Redo (SS+Y, Edit > Redo)** brings an undone step back. A new change ends the steps that can be
redone. The history holds hundreds of small steps (fewer in a big project; the oldest go first) and
is cleared when a project is loaded from tape.

![Nothing to undo](img/undo_none.png)

**Copy and paste.** SS+A (Edit > Mark block) marks the start of a block at the cursor line / row; moving
the cursor selects the block, shown by a bar at the left screen edge. SS+A again removes the mark.
SS+D (Edit > Copy) copies the block to the clipboard, Edit > Cut copies it and deletes it. Without a mark
the current IL line, LD rung or FBD network is taken. SS+F (Edit > Paste) inserts the clipboard in front
of the cursor line (IL) or in front of the rung / network of the cursor (LD / FBD), also in another POU of
the same language. The status bar shows what was copied or pasted.

![A marked block (lines 3-5)](img/clip_mark.png)

* LD blocks always take whole rungs (rows joined by vertical links), FBD blocks whole networks, so that
  no element is cut; the comment rows just above a rung / network go with it.
* FB instances stay the same: a pasted `TON0` box uses the same timer. Choose another instance if the
  copy needs its own.
* Paste is one step for Undo.

**Duplicate a POU (POU > Duplicate..., `U` in the POU list)** makes a copy of the selected POU with its
language and comment under a new name.

**Cross references (SS+X, View > Cross references)** list every use of the operand at the cursor. It
works on an IL line, an LD / FBD element or a symbol; in the POU list it finds the calls of the POU. Each
entry shows the POU, the line (or the row and position) and whether the operand is read, written or
called. ENTER opens the editor at that place.

![Cross references of S_Red](img/xref.png)

**Add to watch (SS+W, Edit > Add to watch)** puts the operand at the cursor on the watch list. For an
FB instance it adds the ET of a timer, the CV of a counter, or the Q of the other blocks.

![Added to watch](img/add_watch.png)

## 13. The demo project

The demo starts with the program and can be loaded again with File > Open demo. Each part uses its own
inputs and outputs:

| POU | Function | Inputs | Outputs |
|---|---|---|---|
| MAIN | calls the three demos | | |
| DEMO_IL | traffic light as a step sequence (S0.0-S0.4), one TON per step | `0` start, `1` stop | Q0.0-Q0.2 |
| DEMO_LD | pump with self-holding, fan run-on (TOF), alert memory (P / N contacts, set / reset), blinking lamp (2 x TP) | `4`-`7` | Q0.4-Q0.6 |
| DEMO_FBD | inlet valve with nested gates and a 2 s run-on, overflow horn (RT, SR), service counter (CTU, after 3 fills) | byte 1: `B`, then `0`-`5` | Q1.0-Q1.2 |

A symbol table names all of them. The watch list contains the traffic light (the ET of each of its four timers followed by its light,
shown first on the trend page), HighLvl, Valve, Horn, TValve.ET and Fills.CV.

## 14. Reference

### Standard function blocks

| FB | Inputs | Outputs | Instances |
|---|---|---|---|
| TON, TOF, TP | IN, PT (TIME) | Q, ET | 4 each |
| CTU | CU, R, PV (INT) | Q, CV | 4 |
| CTD | CD, LD, PV (INT) | Q, CV | 4 |
| R_TRIG, F_TRIG (RT, RF) | CLK | Q | 4 each |
| SR, RS | S1 / S, R / R1 | Q1 | 4 each |

### Limits

| Item | Limit |
|---|---|
| POUs | 8 (plus the symbol table and the watch list) |
| Project memory | 12 KB |
| POU name / comment | 8 / 20 characters |
| Symbol name / comment | 8 / 30 characters |
| Watch list | 16 operands |
| Call depth | 8 |
| Watchdog | 30000 instructions per scan |
| Edge contacts (P / N) | 32 per project |

For the program structure, memory map and build details see `README.md` and `CLAUDE.md`.
