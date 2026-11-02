# Python remap handlers for this configuration.
#
# Currently: G75 - groove peck cycle. LinuxCNC has no native G75 (G75 is an
# unallocated G-code, see docs/html/gcode/g-code.html), so this file adds one
# through [RS274NGC] REMAP.
#
# G75 - groove peck cycle
# =======================
#
# Accepted forms:
#
#   G75 R<retract>                                  ; preset block
#   G75 X<bottom> Z<end> P<peck> Q<stepover> [R<retract>] F<feed>
#
#   X  absolute groove bottom. A diameter in G7 mode, a radius in G8 mode.
#      LinuxCNC converts the programmed value itself, so X and P always live
#      in the same unit space as the program.
#   Z  absolute position of the far end of the groove. Omit to plunge once
#      at the current Z.
#   P  peck increment in X, same unit space as X (a diameter under G7).
#   Q  stepover along Z between plunge panels. Omit for a single panel.
#   R  retract distance per peck. Omit for a full retract to the X start
#      point, which clears chips best. A lone R block presets the value for
#      the next G75 block only, matching the Fanuc two-block style.
#   F  feedrate. Normal F word handling, the value stays modal.
#
# Cycle:
#   1. rapid to the X start point at the current Z
#   2. plunge in X by P at feed, retract, repeat until X is reached
#   3. rapid clear in X, shift Z by Q, repeat until Z is reached
#   4. rapid back to the start point
#
# The start point is the tool position when the block runs, so position the
# tool at the groove mouth before calling G75. At the end of the cycle the
# motion mode that was active before G75 is restored (G0/G1/G2/G3), so a
# plain coordinate word on the next line does not turn into an unexpected
# rapid.
#
# Preconditions and side effects:
#   * the feedrate has to be greater than zero, program an F word
#   * the ZX plane (G18) is expected, as for the other lathe cycles
#   * numbered parameters 4990-4994 are used as scratch space

# Both modules only exist inside the LinuxCNC interpreter process, so a
# static type checker cannot resolve them. This file is never imported by
# anything else.
from interpreter import INTERP_OK  # type: ignore[import-not-found]
from emccanon import MESSAGE  # type: ignore[import-not-found]

# Scratch numbered parameters. 4990..4999 sit inside the user range
# (31..5000) and are volatile, so they never reach the .var file.
_SCRATCH_X = 4990
_SCRATCH_Z = 4991
_SCRATCH_DIAMETER = 4992
_SCRATCH_MOTION = 4993
_SCRATCH_FEED = 4994

# Safety limit on the emitted motion count, so a mistyped Q or P cannot
# spin the interpreter forever.
_MAX_MOVES = 20000

# Retract preset from a lone "G75 R" block. None means "full retract to the
# X start point".
_retract = {'value': None}  # type: dict[str, float | None]

# Motion mode to restore after the cycle. #<_motion_mode> returns G1 as 10,
# G2 as 20, G3 as 30 and G0 as 0.
_MOTION_AFTER = {0.0: 'G0', 10.0: 'G1', 20.0: 'G2', 30.0: 'G3'}

# The only opcodes this handler is allowed to emit.
_ALLOWED_OPCODES = frozenset(('G0', 'G1', 'G2', 'G3'))


def _ngc_line(code, x=None, z=None):
    """Build one NGC line from an allowlisted opcode and two float words.

    The output is a fixed template: an opcode from _ALLOWED_OPCODES, then
    letters X and Z, each followed by a number formatted with %.6f. Nothing
    free-form ever reaches the interpreter.
    """
    if code not in _ALLOWED_OPCODES:
        raise ValueError('refusing to emit opcode %r' % (code,))
    parts = [code]
    for letter, value in (('X', x), ('Z', z)):
        if value is None:
            continue
        if not isinstance(value, (int, float)):
            raise ValueError('word %s is not numeric' % letter)
        parts.append('%s%.6f' % (letter, float(value)))
    return ' '.join(parts)


def _ngc_value(interp, emit, number, expression):
    """Evaluate an NGC expression into a scratch parameter, return a float."""
    status = emit('#%d = [%s]' % (number, expression))
    if status != INTERP_OK:
        raise RuntimeError('cannot evaluate %s, interpreter status %s'
                           % (expression, status))
    return float(interp.params[number])


def g75(self, **words):
    """G75 groove peck cycle, see the file header for the full contract."""
    x_word = words.get('x')
    z_word = words.get('z')
    p_word = words.get('p')
    q_word = words.get('q')
    r_word = words.get('r')

    # Lone R block: preset the per-peck retract, no motion.
    if x_word is None and z_word is None and p_word is None:
        if r_word is None:
            return 'G75: needs X and P, or a lone R to preset the retract'
        _retract['value'] = abs(float(r_word))
        MESSAGE('G75: retract preset to %.4f' % _retract['value'])
        return INTERP_OK

    if x_word is None:
        return 'G75: missing X (groove bottom)'
    if p_word is None:
        return 'G75: missing P (peck increment)'

    peck = abs(float(p_word))
    if peck <= 0.0:
        return 'G75: P (peck increment) must be greater than zero'

    step = 0.0
    if q_word is not None:
        step = abs(float(q_word))
        if step <= 0.0:
            return 'G75: Q (stepover) must be greater than zero'

    if r_word is not None:
        retract = abs(float(r_word))
    else:
        # A preset from a lone R block applies to this block only, the way
        # the Fanuc two-block form works.
        retract = _retract['value']
        _retract['value'] = None

    # Bound once so there is a single call site per emitted line.
    emit = self.execute

    try:
        diameter_mode = _ngc_value(self, emit, _SCRATCH_DIAMETER,
                                   '#<_lathe_diameter_mode>') > 0.5
        motion_before = _ngc_value(self, emit, _SCRATCH_MOTION,
                                   '#<_motion_mode>')
        # #5420/#5422 are in program units but always in radius space for X,
        # even under G7, so scale X into the programmed (diameter) space.
        x_start = _ngc_value(self, emit, _SCRATCH_X, '#5420')
        if diameter_mode:
            x_start *= 2.0
        z_start = _ngc_value(self, emit, _SCRATCH_Z, '#5422')
        feed_rate = _ngc_value(self, emit, _SCRATCH_FEED, '#<_feed>')
    except Exception as exc:
        return 'G75: %s' % exc

    if feed_rate <= 0.0:
        return ('G75: feedrate is zero, program an F word on or before the '
                'G75 block')

    # Axis words in a remapped block arrive in canonical units, where X is
    # always a radius on a lathe, so scale X back into the programmed space.
    # P and Q are plain numbers and arrive unscaled.
    x_end = float(x_word)
    if diameter_mode:
        x_end *= 2.0
    z_end = float(z_word) if z_word is not None else z_start

    x_delta = x_end - x_start
    if abs(x_delta) < 1e-9:
        return 'G75: X equals the current X, nothing to cut'
    x_dir = 1.0 if x_delta > 0.0 else -1.0

    # Plunge panels along Z.
    z_positions = [z_start]
    if abs(z_end - z_start) > 1e-9:
        if step <= 0.0:
            return 'G75: a Z end point needs Q (stepover) greater than zero'
        z_dir = 1.0 if z_end > z_start else -1.0
        while abs(z_end - z_positions[-1]) > 1e-9:
            nxt = z_positions[-1] + z_dir * step
            if (z_dir > 0.0 and nxt > z_end) or (z_dir < 0.0 and nxt < z_end):
                nxt = z_end
            z_positions.append(nxt)
            if len(z_positions) > _MAX_MOVES:
                return 'G75: too many Z panels, check Q and Z'

    moves = [0]

    def move(code, x=None, z=None):
        line = _ngc_line(code, x, z)
        status = emit(line)
        if status != INTERP_OK:
            raise RuntimeError("interpreter rejected '%s', status %s"
                               % (line, status))
        moves[0] += 1
        if moves[0] > _MAX_MOVES:
            raise RuntimeError('more than %d moves, check P and Q'
                               % _MAX_MOVES)

    try:
        for index, panel_z in enumerate(z_positions):
            if index == 0:
                move('G0', x=x_start, z=z_start)
            else:
                move('G0', x=x_start)
                move('G0', z=panel_z)

            depth = x_start
            while abs(x_end - depth) > 1e-9:
                depth = depth + x_dir * min(peck, abs(x_end - depth))
                move('G1', x=depth)
                if abs(x_end - depth) <= 1e-9:
                    break
                if retract is None:
                    back = x_start
                else:
                    back = depth - x_dir * retract
                    past_start = ((x_dir > 0.0 and back < x_start)
                                  or (x_dir < 0.0 and back > x_start))
                    if past_start:
                        back = x_start
                move('G0', x=back)

        move('G0', x=x_start, z=z_start)
    except Exception as exc:
        return 'G75: %s' % exc

    # Restore the motion mode that was active before the block, so a bare
    # coordinate word after G75 does not silently become a rapid.
    restore = _MOTION_AFTER.get(motion_before)
    try:
        if restore is not None:
            emit(restore)
        else:
            MESSAGE('G75: previous motion mode unknown (%g), G0 stays active'
                    % motion_before)
    except Exception as exc:
        return 'G75: cannot restore motion mode: %s' % exc

    MESSAGE('G75: %d moves, %d Z panels, %s mode'
            % (moves[0], len(z_positions),
               'diameter' if diameter_mode else 'radius'))
    return INTERP_OK
