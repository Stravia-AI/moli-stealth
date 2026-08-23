"""Schema validation and ephemeral-id normalization for Browser Owner JSONL."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def load_browser_owner_trace(path: Path, offset: int = 0) -> list[dict[str, Any]]:
    if not path.exists():
        raise RuntimeError(f"Browser Owner trace was not created: {path}")
    with path.open("rb") as trace_file:
        trace_file.seek(offset)
        payload = trace_file.read()
    if payload and not payload.endswith(b"\n"):
        raise RuntimeError(f"Browser Owner trace ended with a partial JSONL record: {path}")
    records: list[dict[str, Any]] = []
    for line_number, raw_line in enumerate(payload.splitlines(), start=1):
        try:
            record = json.loads(raw_line)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"invalid Browser Owner JSONL record {line_number}: {error}") from error
        if not isinstance(record, dict) or record.get("schema_version") != 1:
            raise RuntimeError(f"unsupported Browser Owner trace record: {record!r}")
        records.append(record)
    return records


def normalize_lifecycle_milestones(records: list[dict[str, Any]]) -> list[str]:
    document_labels: dict[tuple[int, int, int], str] = {}
    milestones: list[str] = []
    for record in records:
        if record.get("stage") != "renderer_lifecycle_reached":
            continue
        kind = record.get("renderer_lifecycle_kind")
        if kind not in {"dom-content-loaded", "load"}:
            continue
        document_key = _trace_document_key(record)
        if document_key is None:
            continue
        label = document_labels.setdefault(document_key, f"D{len(document_labels) + 1}")
        milestones.append(f"{label}:{kind}")
    return milestones


def normalize_owner_shape(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    action_labels: dict[int, str] = {}
    request_labels: dict[int, str] = {}
    page_labels: dict[int, str] = {}
    document_labels: dict[tuple[int, int, int], str] = {}
    normalized: list[dict[str, Any]] = []
    for record in records:
        action_id = record.get("browser_action_id")
        stage = record.get("stage")
        if not isinstance(action_id, int) or not isinstance(stage, str) or stage.startswith("frontend_"):
            continue
        action = action_labels.setdefault(action_id, f"A{len(action_labels) + 1}")
        request_id = record.get("navigation_request_id")
        request = (
            request_labels.setdefault(request_id, f"R{len(request_labels) + 1}")
            if isinstance(request_id, int)
            else None
        )
        page_generation = record.get("page_residence_generation")
        page = (
            page_labels.setdefault(page_generation, f"P{len(page_labels) + 1}")
            if isinstance(page_generation, int)
            else None
        )
        document_key = _trace_document_key(record)
        document = (
            document_labels.setdefault(document_key, f"D{len(document_labels) + 1}")
            if document_key is not None
            else None
        )
        normalized.append(
            {
                "stage": stage,
                "source": record.get("source"),
                "origin": record.get("navigation_origin"),
                "before": record.get("owner_state_before"),
                "after": record.get("owner_state_after"),
                "action": action,
                "request": request,
                "page": page,
                "document": document,
            }
        )
    return normalized


def _trace_document_key(record: dict[str, Any]) -> tuple[int, int, int] | None:
    document = record.get("document_lifecycle_identity")
    if not isinstance(document, dict):
        return None
    values = (
        document.get("renderer_page_id"),
        document.get("document_generation"),
        document.get("lifecycle_epoch"),
    )
    if not all(isinstance(value, int) for value in values):
        return None
    return int(values[0]), int(values[1]), int(values[2])
