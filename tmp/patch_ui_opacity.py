import pathlib
p = pathlib.Path('crates/linc-ui/src/screens/common.rs')
s = p.read_text()
old = '''    view.origin = origin;'''
new = '''    view.origin = origin;
    // A rapid is not a cut: the picture steps back so the machine's own motion
    // is what the operator watches, and comes back the moment the tool bites.
    view.opacity = if app.snapshot.motion_kind.is_rapid() {
        0.45
    } else {
        1.0
    };'''
assert s.count(old) == 1, s.count(old)
p.write_text(s.replace(old, new))
print('opacity wired')