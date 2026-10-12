"""Settings that depend on where a run happens, set once by an entry point with set_paths().

Library modules read them at call time through path(); nothing in this folder names a machine location.
"""
import os

_PATHS = {}


def set_paths(**kw):
    """Store settings given as keyword arguments; None leaves the setting unset."""
    for name, value in kw.items():
        _PATHS[name] = os.path.abspath(value) if value else None


def path(name):
    """Return a setting or stop with the matching command-line flag named."""
    value = _PATHS.get(name)
    if not value:
        raise SystemExit(f"required setting missing: --{name.replace('_', '-')}")
    return value
