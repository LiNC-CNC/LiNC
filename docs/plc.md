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
`Load` with error code `io_map_locked`. Logic-only changes load at a scan
boundary, but **Load sets the daemon to Loaded and drives every output false**;
press START afterwards. There is no hot swap in this slice.

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

Supported in this slice: NO/NC contacts, normal/set/reset coils,
series/parallel branches, multiple sections, constants, validation, local
offline editing, download, start/stop, live monitoring and forcing.

Not yet: timers, counters, compare/math, SFC, `.clp` or IEC 61131-3 import,
Modbus, retained values across daemon restarts, true hot swap, and a
servo-thread realtime component. The scan is userspace (1–5 ms target, soft
realtime); do not use it for encoder phasing or step timing.

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

## Protocol

4-byte big-endian length + JSON, adjacent tagging:

```
{"request":"hello","data":{"protocol":1}}
{"request":"load","data":{"project":{...}}}
{"request":"start"}
{"response":"state","data":{ ... StateSnapshot ... }}
{"response":"error","data":{"code":"...","message":"..."}}
```

Dashboard pins: `linc_plc.<binding>` for each I/O point,
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
