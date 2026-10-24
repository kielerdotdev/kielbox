# SSX source provenance

Source: https://github.com/kielerdotdev/ssx

Revision: `bd8edc6f56b22becaaca15d2e0b226a5d967cb4c`

Imported for the rcr toolkit integration. Original GPL-3.0-or-later license, copyright notices, tests and documentation are retained. The egui executables are not used by rcr; its native GPUI front end drives the shared engines.

Local renderer changes in `crates/ssx-editor/src/render.rs`: copy opaque unpadded base-image regions directly at 1× scale, and return complete rendered viewports without allocating and copying a second output buffer. These support rcr's incremental tiled annotation previews; export behavior is preserved.

Local interaction changes in `crates/ssx-editor/src/input.rs` and `session/mod.rs`: the UI overlay also reports the live object being created. This provides accurate effect-region feedback while dragging; it does not change rendered/exported image pixels.

For publication, demo profile names and credential-shaped test strings are replaced with generic placeholders. Upstream egui documentation screenshots and visual-test baselines are omitted because they show local paths or the original demo text. They remain outside this source snapshot; regenerate them with the upstream `UPDATE_SNAPSHOTS=1` workflow if working on those standalone interfaces. RCR's GPUI interface does not use these images. Original license and copyright notices are preserved.
