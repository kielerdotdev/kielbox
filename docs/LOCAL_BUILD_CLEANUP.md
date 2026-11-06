# Local build cleanup

The retained application package is `dist/windows-reviewed-2026-11-05/`.
Launch it with its `Start-rcr.ps1` and the same existing working/data folder.
Its `rcr-clean.exe` is an intentional packaged helper; keep the entire package.

Preview and remove obsolete development copies and packaging-test executables:

```powershell
powershell -NoProfile -File scripts/clean-local-builds.ps1 -WhatIf
powershell -NoProfile -File scripts/clean-local-builds.ps1
```

Run cleanup after packaging checks when their fake executables are no longer
needed. Those fixtures are not runnable app builds. The cleanup script uses an
explicit list of obsolete build locations and GUID-named packaging fixtures;
it does not infer obsolescence from a package's date. Review that list when
retiring another build.

Cleanup removes only application/test executables and the redundant `rcr.pdb`
and `rcr.d` files in those locations. It preserves recordings, projects,
preferences, reports, package metadata, dependencies, and Cargo build caches.
Old package launchers remain as historical metadata and cannot run after their
executable is removed. Do not use those launchers.

The script refuses paths outside the checkout and paths through reparse points,
skips junctions inside packaging fixtures, and refuses to delete a running RCR
executable. No application is stopped automatically. Removed local artifacts
are ignored by Git; the script and this policy are the committed cleanup.

On November 6, 2026, cleanup removed 38 obsolete executables, symbol files,
and dependency files (1,826,108,542 bytes). Before/after SHA256 checks confirmed
76 retained package, data, and metadata files were unchanged. The reviewed
package passed its 22-file integrity check, and a second cleanup run found zero
candidates. Cargo caches and separate dependency/tool executables remain.
