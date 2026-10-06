# rightkit-fs

Safe filesystem state for Right Suite apps: descriptor-pinned state directories
(`StateDir`), atomic writes (`atomic_write`, `AtomicWriter`), cross-process locks
(`FileLock`), owner-only files (`write_private_file`, `ensure_private_dir`), and
stable file identity across renames (`IdentityStore`).

See `API.md` in the lane folder for the per-app migration table. Unix is fully
descriptor-pinned; Windows uses a path-based fallback that refuses reparse points.
