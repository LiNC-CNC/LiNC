# Loobric tool-table sync

Status: implemented as an optional LiNC integration. Disabled by default; the
profile must opt in with `[loobric] enabled = true`.

## What Loobric is

Loobric synchronizes CNC tool data between CAM, a tool-management server and a
machine control. For LinuxCNC the unit of exchange is the standard `tool.tbl`:
tool numbers, pockets, offsets, diameter and comments.

## Repositories and bindings

Surveyed 2026-11-02 from `https://github.com/loobric/`:

| Repository | Language | What it provides |
|---|---|---|
| `loobric-server` | Python (FastAPI-style server) | Self-hosted server and the JSON REST API. No published client SDK. |
| `loobric-cli` | Python, stdlib only | `loobric.Client` library, the `loobric` CLI, importers (DIN 4000, STEP P21, GTC/ISO 13399, SolidCAM, hyperMILL) and an optional `loobric-mcp` server. Speaks the REST API over `urllib`. |
| `loobric-linuxcnc` | Python, archived | Moved to [`loobric/loobric-clients`](https://github.com/loobric/loobric-clients) under `clients/linuxcnc/`. Single-file, stdlib-only client for LinuxCNC. |
| `loobric-freecad` | Python | FreeCAD addon. |
| `iso_133399` | C++ | ISO 13399 helper code, not a CNC tool-table client. |

There is **no Rust binding**. The LinuxCNC client is the only component that
already implements the table-specific behavior LiNC needs:

- `sync`: three-way merge between the local table, the server and the last-sync
  state; pushes local changes, writes back only **bound** server changes,
  reports conflicts and never guesses identity.
- `push`: one-way local table to server.
- `doctor` / `init`: validate the client config and write a starter config.
- A timestamped backup before any write, and line-surgical writes that preserve
  the operator's comments.
- State and operator summary in `state-<machine>.json` under the client's
  `STATE_DIR` (default `~/.config/loobric/`).

Its configuration is a shell-style file, by default
`~/.config/loobric/linuxcnc.conf`, with `LOOBRIC_API_URL`, `LOOBRIC_API_KEY`,
`MACHINE_NAME`, `TOOL_TABLE` or `LINUXCNC_INI`, `UNITS`, `STATE_DIR` and
`LOG_DIR`. Environment variables with the same names override the file. The
server API is JSON over HTTP with a Bearer key; the client endpoints LiNC's
chosen path exercises include:

- `GET /api/health`
- `POST /api/v1/machine-records`
- `POST /api/v1/machine-records/<id>/assert`
- `GET /api/v1/tool-table-entry-records?machine_id=<id>`
- `POST /api/v1/tool-table-entry-records/sync` (`mode: snapshot` or `merge`)
- `GET /api/v1/machine-set-maps/status?machine_id=<id>`

The server also exposes `/openapi.json` when running, but no generated Rust
client or stable schema crate is published.

## Why LiNC runs the Python client

Reimplementing the REST calls in Rust would duplicate the client's merge rules,
state file, backup behavior and provenance semantics, and would need a new
TLS/HTTP dependency in `linc-core`. The Baiermill Probe Basic integration
(`../baiermill_pb/custom_probebasic.py`) already does the same thing: run the
official single-file client as a subprocess, on a worker thread, and reload the
table after a pull. LiNC follows that precedent.

The trade-off is one runtime requirement: Python 3 must be available on the
control box when the integration is enabled. A disabled profile never looks for
it.

## LiNC integration

`linc-core/src/loobric.rs` provides:

- `LoobricSettings::from_profile` — returns `None` unless `[loobric] enabled` is
  true.
- `LoobricRunner` — a worker thread that runs the client with
  `--config <path> sync`, sets `TOOL_TABLE` from the machine INI, captures
  stdout/stderr, enforces `timeout_secs`, and publishes `LoobricStatus`.
- `LoobricBackend` — a `MachineBackend` wrapper that:
  - runs one sync on startup when the interpreter is idle;
  - runs a sync after the tool-table revision changes (an operator write);
  - folds a completed result into the snapshot;
  - when the client changed the file, sends the internal
    `ReloadToolTable` command, but only when idle;
  - exposes `SyncToolTable` for the Tools page's `S` shortcut.

`linc-app` wraps the selected backend only when the profile enables Loobric.
Both backends implement `ReloadToolTable`: the simulator reloads its in-memory
table, and the NML backend reloads the file and issues an IO `LoadToolTable`.
`SyncToolTable` is refused by an unwrapped backend, so a profile without the
integration cannot trigger a process.

The Tools page shows the Loobric status in the TOOL TABLE header only when
`snapshot.loobric` is present: the last client message, or a warning while a
pulled table waits for idle. `S` starts a manual sync. The status shares the
existing header row, so it costs no extra vertical space. All other pages are
unchanged.

## Configuration example

```toml
[loobric]
enabled = true
# Single-file client; use `client = "loobric-linuxcnc"` with no interpreter
# when the package is installed on PATH instead.
client = "/home/cnc/.local/bin/loobric_linuxcnc.py"
interpreter = "python3"
config = "~/.config/loobric/linuxcnc.conf"
sync_on_startup = true
sync_after_tool_write = true
timeout_secs = 60
```

Prepare the client once:

```sh
python3 loobric_linuxcnc.py init
python3 loobric_linuxcnc.py doctor
```

Keep the server URL and API key in the client config, not in the machine
profile. The machine profile is normally readable by every operator.

## Safety and limits

- The client's writes happen before LiNC reloads the table. If the interpreter
  starts a program during the network round trip, LiNC defers the reload until
  the interpreter is idle and says so on the Tools page.
- A sync owed by a tool-table write also waits for idle. The write itself has
  already landed on disk; the client run is held back because it rewrites
  `tool.tbl`, and a table that changes under a running program changes the
  offsets the next tool change reads. The Tools page says
  `tool write waiting for idle`. A sync owed by the startup setting at the same
  time collapses into the same single client run.
- `S` is idle-only. The backend rechecks idle when the command arrives; it
  does not trust the page's shortcut gate.
- The integration does not add a server-side push/subscribe channel, does not
  read the client's `state-<machine>.json` health summary, and does not resolve
  conflicts in LiNC. Those remain the official client's and the server's job.
- A failed run is shown as the client's last output line. It never stops the UI,
  the backend worker or the machine.
