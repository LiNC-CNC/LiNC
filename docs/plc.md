# Ladder PLC module

`linc-plc` is a userspace PLC for LiNC. It compiles a ladder project, scans it
in its own process, owns its own `linc_plc.*` HAL component, and serves the
LiNC **PLC** page over a same-user Unix socket. It is not part of the `linc`
process and never touches the closed `linc.*` HMI surface.

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
