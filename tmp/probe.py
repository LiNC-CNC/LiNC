import pathlib

# The shell sets the opacity from the motion kind, and samples error only on cuts.
p = pathlib.Path('crates/linc-ui/src/screens/common.rs')
s = p.read_text()
old = '        let mut view = view;'
if old not in s:
    print('MISS view copy')
else:
    print('found view copy')
print('has TrajectoryView::new' if 'TrajectoryView::new' in s else 'no')