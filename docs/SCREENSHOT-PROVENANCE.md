# Screenshot library source provenance

Source: https://github.com/kielerdotdev/ssx

Revision: `bd8edc6f56b22becaaca15d2e0b226a5d967cb4c`

These libraries are maintained inside the RCR workspace. Original GPL-3.0-or-later license, copyright notices, tests and documentation are retained. The Rune desktop uses the libraries directly; optional standalone interfaces are separate targets.

Local renderer changes in `crates/image-editor/src/render.rs`: copy opaque unpadded base-image regions directly at 1× scale, and return complete rendered viewports without allocating and copying a second output buffer. These support rcr's incremental tiled annotation previews; export behavior is preserved.

Local interaction changes in `crates/image-editor/src/input.rs` and `session/mod.rs`: the UI overlay also reports the live object being created. This provides accurate effect-region feedback while dragging; it does not change rendered/exported image pixels.

For publication, demo profile names and credential-shaped test strings are replaced with generic placeholders. Upstream egui documentation screenshots and visual-test baselines are omitted because they show local paths or the original demo text. They remain outside this source snapshot; regenerate them with the upstream `UPDATE_SNAPSHOTS=1` workflow if working on those standalone interfaces. RCR's Rune interface does not use these images. Original license and copyright notices are preserved.

## Native workspace migration

All 23 former SSX crates now live directly under RCR's `crates/` directory with
`rcr-*` package names. They share the root dependency declarations, lockfile and
lint configuration. The default workspace targets remain the Rune desktop and
its original packages; platform-specific and standalone tools are opt-in targets.

Existing `.ssxe` documents, `.sxcu` uploaders, serialized settings, IPC identifiers
and system-keyring service names retain their original identifiers for data
compatibility. These are format/storage compatibility details, not dependencies
on a separate SSX installation. Existing RCR data paths remain unchanged.
