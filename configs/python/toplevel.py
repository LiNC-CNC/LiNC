# Entry point for the LinuxCNC Python plugin ([PYTHON] TOPLEVEL).
#
# The plugin imports this module, which imports the remap handlers in
# remap.py, so that [RS274NGC] REMAP lines can find them by function name.

import remap  # noqa: F401  # imported for the side effect of loading handlers
