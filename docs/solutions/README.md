# Solutions

Documented solutions to problems already solved in this repository: bugs that
took real investigation, and practices that emerged from them. `ce-plan`,
`ce-code-review`, and `ce-debug` search this directory, so an entry written
here is what a future session sees before it repeats the work.

Each entry carries YAML frontmatter (`module`, `date`, `problem_type`,
`component`, `severity`, plus the track-specific fields) and then a body:
Problem, Symptoms, What Didn't Work, Solution, Why This Works, Prevention.

## Categories

| Directory | Holds |
|---|---|
| `integration-issues/` | Faults in the boundary to LinuxCNC, HAL, the panel, or the diagnostic socket |
| `ui-bugs/` | Failures in the egui shell, pages, keyboard flow, or rendering |
| `build-errors/` | Rust, packaging, and cross-build failures |
| `test-failures/` | Tests that failed for a reason the failure message did not show |
| `best-practices/` | Conventions that keep recurring mistakes away |

## Entries

- [Saving a program must reopen it, or AUTO runs the pre-edit text](integration-issues/linuxcnc-save-must-reopen-program.md)
