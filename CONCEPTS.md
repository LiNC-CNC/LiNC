# Concepts

Shared domain vocabulary for this project — entities, named processes, and status concepts with project-specific meaning. Seeded with core domain vocabulary, then accretes as ce-compound captures and refreshes learnings; direct edits are fine. Glossary only, not a spec or catch-all.

## Machine profile

The set of files a machine starts from: the INI that describes it, the HAL files that INI names, and any file its launcher loads afterwards. A profile may extend another profile, and its file set may live outside the repository, so the profile and its launcher — not the checkout — decide which wiring a machine runs. Two files with the same name in different checkouts are two different files.

## Post-GUI HAL file

Wiring loaded after the GUI starts, as opposed to the pre-GUI files loaded before it. Only a post-GUI file may net pins the HMI publishes, because those pins do not exist when the pre-GUI files load. A missing net here fails at load time and names the pin, not the file.

## Program interlock

The level the HMI publishes to say the loaded program may be started, published as `linc.program-ok`. It is shut unless the program is the one the controller holds, the buffer has no unsaved change, no block-level fault is known, the interpreter has not refused or crashed on it, and no interpreter check is still outstanding. A check in flight is not an answer, so the interlock stays shut until the verdict is applied. It is distinct from the network interlock, which concerns the machine's link while a program is armed.

## Override limits

The mask that lets a bounded move ignore a tripped hard limit. It must be armed before motion is enabled, because the fault check runs every servo cycle and re-disables motion while the joint still sits on the switch. The controller clears the mask when the move it allowed ends, so each bounded step needs its own arm, enable, and short jog. It is per joint, and it is not the same as the soft-limit or home state.

## Cycle start ownership

Which component decides that a CYCLE START press starts or resumes the program. The HMI owns it: it reads the key as a button, applies its refusal checks, and sends the run to the controller itself. No HAL chain carries the panel key to the controller's run input, so a press the HMI refuses sends nothing and a press it accepts sends exactly one command.
