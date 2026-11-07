# Publishing source

Publish source and licensed assets only. Recordings, screenshots, projects,
preferences, credentials, databases, crash dumps, compiler output and local
audit artifacts stay outside Git. Review the staged file list before pushing;
ignore rules do not remove previously tracked files.

Generated FFmpeg provenance is sanitized with
`python scripts/sanitize-provenance.py <public-copy.json>`. Keep the original
locally under `.downloads`. Public component versions, hashes and copyright
notices remain in the source tree.

Optional egui documentation screenshots and visual baselines are omitted because
they contained local paths or original demo account text. The matching fixtures
use generic placeholders. See `docs/SCREENSHOT-PROVENANCE.md` for regeneration.

The original GitHub publication started with a clean snapshot and a generic
contributor identity. Later commits may use the maintainer's normal identity.
The current repository is `kielerdotdev/kielbox`, made public at the maintainer's
explicit request. Push only the reviewed branch;
older local branches remain separate and must not be pushed with `--mirror`,
`--all` or tags without a separate review. Removing obsolete remotes locally
does not delete the repositories hosted at those addresses.

Before publishing updates, inspect staged content and run a secret scanner on an
export of that exact tree. The initial audit used Gitleaks 8.30.1 with decoded
content scanning: its two findings were a keyboard shortcut fixture and the
public AWS SigV4 example signing key. Neither is a usable credential. Copyright
emails and public upstream attribution are intentionally retained.
