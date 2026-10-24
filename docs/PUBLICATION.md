# Publishing source

Publish source and licensed assets only. Recordings, screenshots, projects,
preferences, credentials, databases, crash dumps, compiler output and local
audit artifacts stay outside Git. Review the staged file list before pushing;
ignore rules do not remove previously tracked files.

Generated FFmpeg provenance is sanitized with
`python scripts/sanitize-provenance.py <public-copy.json>`. Keep the original
locally under `.downloads`. Public component versions, hashes and copyright
notices remain in the source tree.

Vendored egui documentation screenshots and visual baselines are omitted because
they contained local paths or original demo account text. The matching fixtures
use generic placeholders. See `vendor/ssx/RCR-UPSTREAM.md` for regeneration.

The GitHub publication starts with a clean snapshot and a generic contributor
identity. Older local history and other remotes are preserved locally; do not
push them with `--mirror`, `--all` or tags without a separate review.

Before publishing updates, inspect staged content and run a secret scanner on an
export of that exact tree. The initial audit used Gitleaks 8.30.1 with decoded
content scanning: its two findings were a keyboard shortcut fixture and the
public AWS SigV4 example signing key. Neither is a usable credential. Copyright
emails and public upstream attribution are intentionally retained.
