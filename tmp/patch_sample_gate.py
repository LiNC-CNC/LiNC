import pathlib
p = pathlib.Path('crates/linc-ui/src/shell.rs')
s = p.read_text()
old = '''        for (axis, value) in values {
            if self.following_history.get(&axis).is_none() {'''
new = '''        // A rapid's following error is the machine catching up with itself, not
        // the cut's accuracy: the numbers are the run's cutting error.
        let cutting = self.snapshot.motion_kind.is_cutting();
        for (axis, value) in values {
            if self.following_history.get(&axis).is_none() {'''
assert s.count(old) == 1, 'loop'
s = s.replace(old, new)

old = '''            let worst = self.following_run_max.get(&axis).copied().unwrap_or(0.0);
            self.following_run_max.insert(axis.clone(), worst.max(value.abs()));'''
new = '''            if !cutting {
                continue;
            }
            let worst = self.following_run_max.get(&axis).copied().unwrap_or(0.0);
            self.following_run_max.insert(axis.clone(), worst.max(value.abs()));'''
assert s.count(old) == 1, 'accumulate'
s = s.replace(old, new)
p.write_text(s)
print('sampling gated')