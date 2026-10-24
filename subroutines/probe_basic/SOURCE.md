# Probe Basic probing subroutines

These `.ngc` files originate from Probe Basic's probing subroutine bundle.
LiNC invokes them through LinuxCNC's `SUBROUTINE_PATH`; the HMI builds the
`o<...> call` and the `#3014`–`#3036` parameter block.

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

`SHA256SUMS` records the exact files. To update, copy the same upstream set
from a newer Probe Basic checkout, reapply the LiNC additions below, and
regenerate the checksums.

## LiNC additions

- `linc_probe_context.ngc` captures the active WCS, its raw XYZ offsets and the
  initial work position before each LiNC-owned probe. Explicit Apply uses this
  context so measured work coordinates become raw `G10 L2` offsets and repeated
  Apply is idempotent.
- The four `probe_corner_*_edge_angle.ngc` routines retain their already
  computed `c1x`/`c1y` corner origin in `#<_linc_corner_x>` and
  `#<_linc_corner_y>`. These values are absent from Probe Basic's public
  `#1001`–`#1012` result contract. The probing and reference Apply calculations
  are unchanged.
- The four `probe_top_*_edge_angle.ngc` routines refresh the captured start
  anchor after their reference `R0` reset. This keeps explicit Apply aligned
  with the actual probing coordinates when the previous WCS was rotated.
- The same four routines also capture the source WCS origin on their captured
  start axis (`workspace_x` for front/back, `workspace_y` for left/right) and
  add it to the start-axis term of both DIRECT `G10 L2` branches. Without it a
  DIRECT run stored the work position instead of the raw offset, so a nonzero
  source origin produced the wrong WCS row while explicit Apply (which adds the
  captured origin in `probe.rs`) produced the right one. Review finding PR-02;
  the post-`R0` start anchors and the first-edge delta signs are unchanged.
- Ten routines carry the current Probe Basic traverse-feed correction from
  `[3017]` to `[#3017]`; the corrected lines were checked against the current
  reference bundle.

The angle routines still clear the active WCS rotation at the start of the
cycle, as Probe Basic does, including in measure-only mode.

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
