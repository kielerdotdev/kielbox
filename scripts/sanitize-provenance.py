#!/usr/bin/env python3
"""Remove machine-local paths and run IDs from generated provenance JSON."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

_WINDOWS_PATH = re.compile(r"(?i)(?<![A-Za-z0-9+.-])[A-Z]:[\\/][^\s\"'<>;,]+")
_WINDOWS_ROOT = re.compile(r"(?i)^(?:\\\\\?\\)?[A-Z]:[\\/]")
_WINDOWS_PROFILE = re.compile(r"(?i)^[A-Z]:\\Users\\[^\\]+(?P<tail>(?:\\.*)?)$")
_POSIX_PROFILE = re.compile(r"^/(?:home|Users)/[^/]+(?P<tail>/.*)?$")
_UUID = re.compile(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
_GENERATED_ID = re.compile(r"(?i)\b((?:inputs|generation|run)-)[0-9a-f]{32}\b")


def _join_placeholder(label: str, suffix: str = "") -> str:
    suffix = suffix.strip("\\/")
    return f"{label}/{suffix.replace(chr(92), '/') }" if suffix else label


def _normalize_path(path: str, workspace: str | None, key: str | None) -> str:
    path = path.removeprefix("\\\\?\\")
    normalized = path.replace("/", "\\")
    if key in {"runtime_path", "runtime_directory"}:
        return "<ffmpeg-runtime>"

    sdk = re.search(r"(?i)\\Windows Kits\\[^\\]+\\Include(?:\\(?P<tail>.*))?$", normalized)
    if sdk:
        return _join_placeholder("<windows-sdk-include>", sdk.group("tail") or "")

    msvc = re.search(r"(?i)\\VC\\Tools\\MSVC\\[^\\]+\\include(?:\\(?P<tail>.*))?$", normalized)
    if msvc:
        return _join_placeholder("<msvc-include>", msvc.group("tail") or "")

    if workspace:
        root = workspace.rstrip("\\/").replace("/", "\\")
        if normalized.casefold() == root.casefold():
            return "<workspace>"
        if normalized.casefold().startswith(root.casefold() + "\\"):
            suffix = normalized[len(root):].lstrip("\\")
            tooling = re.match(r"(?i)\.downloads\\tooling\\(?P<tail>.*)$", suffix)
            if tooling:
                return _join_placeholder("<tooling>", tooling.group("tail"))
            return _join_placeholder("<workspace>", suffix)

    profile = _WINDOWS_PROFILE.match(normalized)
    if profile:
        return _join_placeholder("<user-home>", profile.group("tail") or "")

    posix_profile = _POSIX_PROFILE.match(path)
    if posix_profile:
        return _join_placeholder("<user-home>", posix_profile.group("tail") or "")

    if _WINDOWS_ROOT.match(path):
        basename = re.split(r"[\\/]", path.rstrip("\\/"))[-1]
        return _join_placeholder("<local-path>", basename)

    return path


def _workspace_root(document: Any) -> str | None:
    if isinstance(document, dict):
        inputs = document.get("inputs")
        if isinstance(inputs, dict):
            root = inputs.get("root")
            if isinstance(root, str) and _WINDOWS_ROOT.match(root):
                return root
        root = document.get("root")
        if isinstance(root, str) and _WINDOWS_ROOT.match(root):
            return root
    return None


def sanitize_value(value: Any, workspace: str | None = None, key: str | None = None) -> Any:
    """Recursively sanitize JSON-compatible data while preserving public evidence."""
    if isinstance(value, dict):
        return {name: sanitize_value(item, workspace, name) for name, item in value.items()}
    if isinstance(value, list):
        return [sanitize_value(item, workspace, key) for item in value]
    if not isinstance(value, str):
        return value

    if key == "root" and _WINDOWS_ROOT.match(value):
        value = "<workspace>"
    elif key in {"runtime_path", "runtime_directory"} and _WINDOWS_ROOT.match(value):
        value = "<ffmpeg-runtime>"
    elif re.match(r"(?i)^(?:-I|/I)[A-Z]:[\\/]", value):
        value = value[:2] + _normalize_path(value[2:], workspace, key)
    elif _WINDOWS_ROOT.match(value):
        value = _normalize_path(value, workspace, key)
    else:
        value = _WINDOWS_PATH.sub(lambda match: _normalize_path(match.group(0), workspace, key), value)
        profile = _POSIX_PROFILE.match(value)
        if profile:
            value = _join_placeholder("<user-home>", profile.group("tail") or "")

    value = _UUID.sub("<id>", value)
    value = _GENERATED_ID.sub(r"\1<id>", value)
    return value


def sanitize_document(document: Any) -> Any:
    """Sanitize a parsed provenance document using its root for every section."""
    return sanitize_value(document, _workspace_root(document))


def main(argv: list[str] | None = None) -> int:
    args = argv if argv is not None else sys.argv[1:]
    if len(args) != 1:
        print(f"usage: {Path(sys.argv[0]).name} <provenance.json>", file=sys.stderr)
        return 2
    path = Path(args[0])
    document = json.loads(path.read_text(encoding="utf-8"))
    sanitized = sanitize_document(document)
    path.write_text(json.dumps(sanitized, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
