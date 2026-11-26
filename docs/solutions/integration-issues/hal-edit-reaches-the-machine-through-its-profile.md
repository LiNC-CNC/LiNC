---
title: A HAL edit reaches the machine only through the file its profile loads
date: 2026-11-26
category: integration-issues
module: HAL deployment and machine profiles
problem_type: integration_issue
component: api_layer
symptoms:
  - "A HAL chain removed from every file in the repository still drives the machine after a restart"
  - "The live HAL shows the net and the function instance the edited file no longer creates"
  - "The rebuild and the redeploy both succeed and nothing reports an error"
  - "The same edit behaves as intended in the simulated profile"
  - "A key press still reaches halui.program.run twice with one owner left in the HMI"
root_cause: missing_workflow_step
resolution_type: code_fix
severity: high
tags: hal, postgui, machine-profile, deployment, cycle-start
---

# A HAL edit reaches the machine only through the file its profile loads

## Problem

The BaierMill kept a second owner of CYCLE START after the repository's HAL
chains were removed and the HMI was rebuilt and redeployed: one press of the
panel key reached `halui.program.run` through the HMI and once more through a
chain the file edit was supposed to have deleted. The machine's HAL is not the
repository's HAL. It is whatever its machine profile includes, and for this
machine that file lives in a second repository.

## Symptoms

- `hal/linc.hal` and the repository's `*-postgui.hal` files no longer created the
  chain, and the live HAL still had `and2.cycle-start-run` and its
  `or2.pendant-run` consuming `szgh_panel.cycle_start` on the way to
  `halui.program.run`.
- The rebuild and the redeploy both succeeded, the session restarted cleanly, and
  no log line named the disagreement.
- The simulated profile, which loads the repository's own files, showed exactly
  the intended topology: the chain gone, one press, one command.
- Reading `halui.program.run`'s producing net on the machine named a file the
  repository had already changed.

## What Didn't Work

- **Editing the repository's HAL files and redeploying the binary.** The binary
  carries no wiring. HAL is resolved at startup from files the profile's INI
  names, and no file the LiNC repository owns was in that set for this machine.
- **Trusting the simulation as proof of the deployment.** The sim profile loads
  `hal/linc.hal`; the mill's profile loads its own `hal/baiermill-pb-postgui.hal`
  from a separate machine-config repository. A profile that loads different files
  cannot show a fault that lives in the difference between them.
- **Comparing the two files by name.** Both repositories hold a file called
  `hal/baiermill-pb-postgui.hal`, and they differ by hundreds of lines. The name
  matching is what made the wrong copy look like the right one.

## Solution

Read the profile the machine starts with, edit the files it names, install them,
and confirm on the restarted machine's pins.

```ini
; baiermill-linc/baier-mill-linc.ini — the mill's own profile
HALFILE = hal/cnc-panel.hal
POSTGUI_HALFILE = hal/baiermill-pb-postgui.hal
```

That post-GUI file is `baiermill-linc/hal/baiermill-pb-postgui.hal`, installed to
`/home/cnc/linuxcnc/configs/baier-mill-linc/`. Removing the
`szgh_panel.cycle_start` chain there, and keeping the pendant's run request as
the only producer of the controller's run input, changed the machine on the next
restart. The repository's own copy of the same name was not part of the fix.

The file set is wider than the INI. The mill's launcher loads a further post-GUI
file after the HMI exists, and that file carries the net that lets the pendant's
request through at all (`linc.net-safe` into the pendant run and resume gates).
So there are two kinds of file to read: those the profile names, and those the
launcher loads once the HMI is up. A chain can live in either.

Verification is reading the running machine, not the source:

```sh
halcmd show net | grep -n "halui.program.run"
halcmd show pin | grep -n "cycle-start"
```

Absence of the instance and the net from the live HAL is the proof; a clean
`baiermill-linc/tools/check-hal-nets.py` run is the cheaper check before the
restart, and it lives in the machine-config repository, not in LiNC.

## Why This Works

HAL is a file set a profile resolves at startup. The binary publishes and
consumes pins; it does not contain wiring. So an edit to a repository's HAL file
is a claim about a machine, and the machine is whatever its installed profile and
launcher include. When the profile includes a file from another checkout, the
edit and the machine are two different artifacts, and only the second one runs.

## Prevention

- **Read the profile and its launcher before editing a HAL file.** Grep the
  profile's `HALFILE` and `POSTGUI_HALFILE` keys and the launcher's HAL loads,
  then edit exactly those files. A file nobody's profile or launcher names is not
  wiring; it is a document about wiring.
- **Check the nets before the restart, and the live pins after it.** A HAL
  checker run's output on the file set being installed is the fast check; the
  restarted machine's own nets are the proof.
- **Treat the simulation as evidence about the HMI, never about the
  deployment.** A profile whose HAL file set differs cannot reproduce a fault
  that lives in the difference.
- **A same-named file in two repositories is two files.** Diff them before
  reasoning from either, and name which one the profile resolves.
- **Nothing in the tree enforces this.** A repository cannot see a machine's
  installed profile, so no test or lint rule can decide it; the review condition
  in `CODING_STANDARDS.md` under HAL wiring is the guard.

The repository's `configs/baiermill-pb.ini` still names its own
`../hal/baiermill-pb-postgui.hal`, which still carries the old chain and does not
match the machine. Until that mirror is removed or regenerated, the repository
contradicts the deployed machine, and the profile is the only trustworthy
answer.

## Related Issues

- `hal-gate-needs-addf.md` — the same silence one layer down: a gate that exists
  in the loaded file and is never scheduled.
- `limit-switch-recovery-needs-override-first.md` — parses the same
  sim-versus-real HAL parity question from the pin side.
- `linuxcnc-save-must-reopen-program.md` — the general shape: the artifact edited
  is not the artifact the controller reads.
- `docs/plans/cycle-start-owner.md` — deferred this machine's own chain as out of
  scope; this record closes it from the deployment side.
