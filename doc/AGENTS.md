# LiNC operator manual (LaTeX)

## What this is

Operator manual for LiNC, the LinuxCNC HMI in `crates/`, and the panel that drives it.
LaTeX (scrbook), built with plain `pdflatex`. Output `manual.pdf`.

## Build

```bash
make            # pdflatex manual.tex twice
make clean
```

Two passes are required: cross references and the table of contents.

## Layout

- `manual.tex` — preamble, colours, boxes, keycap macros, cover, chapter list.
- `chapters/00-disclaimer.tex` — front-matter safety notice and disclaimer.
- `chapters/01-introduction.tex` … `chapters/12-quickref.tex` — one chapter per topic.
- `img/ui/*.png` — screenshots at 1600×1000 from the current build.
- `img/crops/*.png` — crops of the panel and the shell used as figures.

## Source of truth

The repository, not older documents:

| Doc topic | Source |
| --- | --- |
| Screens, keys, gating | `crates/linc-ui/src/` (`shell/keyboard.rs`, `shell/hints.rs`, `screens/`) |
| Panel controls and modes | `crates/linc-panel/src/`, `crates/linc-core/src/model.rs` (`PanelMode`) |
| Network interlock and `NET` | `crates/linc-core/src/network.rs`, `crates/linc-ui/src/shell/status.rs` |
| HAL outputs | `crates/linc-core/src/command.rs`, `configs/base.toml` |
| Configuration keys | `CONFIGURATION.md` |
| Normative description | `SPEC.md` |
| Feature summary | `README.md` |

## Regenerating the screenshots

The captures come from the simulator, not from a machine:

```bash
cargo build --release -p linc-app -p linc-panel
# Xvfb :99 at 1600x1000, linc-panel --sim --no-hal --window,
# linc --sim --no-hal --demo-mode auto, then drive the pages by key.
```

Pages that the panel mode owns (Manual, Homing) need the simulated panel's mode keys
`4`/`5`/`7`/`8` pressed with the panel window focused, and the HMI window raised again
before each capture — an obscured window captures black.

## Editing pitfalls

- Escape underscores in text: `PROGRAM\_PREFIX`, `linc.net-safe` is safe (hyphen),
  `cnc_panel` is not.
- `\begin{notebox}` … `\end{notebox}` must pair. A missing end produces the cryptic
  `tcb@savebox ended by \end{document}` fatal error.
- Cross references: run `pdflatex` twice and check
  `grep -c "LaTeX Warning: Reference" manual.log` is 0. Build files are ignored by
  `.gitignore`; `make clean` removes them.
- `\shot{path}{caption}{label}` is a full-width figure; `\shotn` takes a width fraction.
- Keycaps are `\keycap{...}`, panel buttons `\pbtn{...}`, screen labels `\sbtn{...}`,
  state words `\state{...}`, HAL pins `\halpin{...}`.

## Update workflow

1. `git log --oneline` since the last manual commit; read the commits that touch
   `crates/linc-ui` or `crates/linc-panel`.
2. Re-check the key tables in chapter 12 against `shell/hints.rs` and
   `state.rs` (`Keymap::from_config`).
3. Re-capture the screenshots if a screen changed, then `make` twice.
4. Diff the prose against the shipped behaviour — the manual must describe what the
   software does, not what it was designed to do.
5. If the safety notice in `chapters/00-disclaimer.tex` or the HMI dialog changes in a way
   that matters legally, bump `SAFETY_NOTICE_VERSION` in
   `crates/linc-core/src/preferences.rs`; every installation is then asked once more.
