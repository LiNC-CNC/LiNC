# Probe Basic probing subroutines

These `.ngc` files are vendored **verbatim** from Probe Basic's probing
subroutine bundle. LiNC invokes them through LinuxCNC's `SUBROUTINE_PATH`; the
HMI builds the `o<...> call` and the `#3014`–`#3036` parameter block.

## Source

- Project: [Probe Basic](https://github.com/kcjengr/probe_basic)
- Local source used for this import: `../probe_basic_dev`
- Commit: `596b16b15ca748975c747256fdafe47ecf0c8adb` (2026-03-17)
- Upstream path: `configs/probe_basic/subroutines`
- Imported files:
  - every `probe_*.ngc`
  - `touch_probe_param_update.ngc`
  - `reset_all_data.ngc`
  - `x_data_reset.ngc`
  - `y_data_reset.ngc`
  - `on_abort.ngc`

`SHA256SUMS` records the exact files. No local edits have been made. To update,
copy the same set from a newer Probe Basic checkout and regenerate the
checksums.

## Authors and license

The routines carry `(author: Chris P)` headers and are part of Probe Basic by
Chris Polanski and the Probe Basic contributors.

Upstream ships the GNU GPL version 3 text in `LICENSE.md`, while its README
also says "This software is released under the GPLv2." We preserve the GPLv3
text as `LICENSE` here and treat it as authoritative until upstream resolves
that conflict.

LiNC is `GPL-2.0-or-later`. Both readings are compatible with reuse: a
GPL-2.0-or-later project can be distributed under GPLv3, and if upstream
confirms GPLv2 the combined work can remain GPLv2-or-later. Keep `LICENSE`,
this provenance note and `SHA256SUMS` with any redistribution of these files.
