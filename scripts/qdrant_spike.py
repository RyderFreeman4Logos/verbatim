#!/usr/bin/env python3
"""Benchmark harness for the Qdrant primary vector sink spike.

The harness is intentionally self-contained and deterministic. It compares the
current local-authoritative vector shape with Qdrant cache and Qdrant primary
prototype variants without changing Verbatim's production defaults.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import resource
import shutil
import sqlite3
import statistics
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import TypeAlias

__all__ = [
    "SpikeError",
    "build_manifest",
    "main",
    "run_failure_modes",
    "run_variant",
]

JsonValue: TypeAlias = (
    None | bool | int | float | str | list["JsonValue"] | dict[str, "JsonValue"]
)

VARIANTS = ("local", "qdrant-cache", "qdrant-primary")
VECTOR_DIMENSION = 16
TEXT_PREVIEW_CHARS = 240
RETRIEVE_ITERATIONS = 9
RESULT_SCHEMA_VERSION = "qdrant-spike-results-v1"
MANIFEST_SCHEMA_VERSION = "qdrant-spike-manifest-v1"
FAILURE_SCHEMA_VERSION = "qdrant-spike-failure-modes-v1"
DEFAULT_OUTPUT_ROOT = Path("target/qdrant-spike")
DEFAULT_QDRANT_URL = "http://127.0.0.1:6333"
UNAVAILABLE_QDRANT_URL = "http://127.0.0.1:9"
DEFAULT_QUERY = "Which evidence explains vector sink throughput and retrieval?"
EXPECTED_CAPABILITY = "deterministic-spike"
REAL_CONFIG_PATH = Path.home() / ".config" / "verbatim" / "config.toml"
REAL_DATA_DIR = Path.home() / ".local" / "share" / "verbatim"
FORBIDDEN_PAYLOAD_FIELDS = {
    "raw_chunk_text",
    "chunk_text",
    "full_document_text",
    "document_text",
    "source_path",
    "path",
    "absolute_path",
}
ALLOWED_PAYLOAD_FIELDS = {
    "profile_id",
    "profile_generation",
    "chunk_id",
    "source_id",
    "heading_path",
    "text_preview",
}


class SpikeError(RuntimeError):
    """Raised when a requested spike scenario cannot be executed safely."""


@dataclass(frozen=True)
class FixtureSource:
    """Deterministic source used by every benchmark variant."""

    source_id: str
    title: str
    text: str


@dataclass(frozen=True)
class ChunkRecord:
    """Chunk metadata that can be hydrated from the local SQLite store."""

    chunk_id: str
    source_id: str
    ordinal: int
    heading_path: list[str]
    text: str
    text_preview: str
    vector: list[float]
    profile_generation: int


@dataclass(frozen=True)
class RemoteHit:
    """Minimal remote hit shape returned by Qdrant and then hydrated locally."""

    chunk_id: str
    score: float
    profile_generation: int


@dataclass
class QdrantStats:
    """Qdrant operation counts and timings for result reporting."""

    operation_counts: dict[str, int]
    operation_timing_ms: dict[str, float]

    @classmethod
    def empty(cls) -> "QdrantStats":
        return cls(operation_counts={}, operation_timing_ms={})

    def add(self, operation: str, elapsed_ms: float, count: int = 1) -> None:
        self.operation_counts[operation] = self.operation_counts.get(operation, 0) + count
        self.operation_timing_ms[operation] = round(
            self.operation_timing_ms.get(operation, 0.0) + elapsed_ms,
            3,
        )

    def add_count(self, operation: str, count: int) -> None:
        self.operation_counts[operation] = self.operation_counts.get(operation, 0) + count

    def merge(self, other: "QdrantStats") -> None:
        for operation, count in other.operation_counts.items():
            self.operation_counts[operation] = self.operation_counts.get(operation, 0) + count
        for operation, elapsed_ms in other.operation_timing_ms.items():
            self.operation_timing_ms[operation] = round(
                self.operation_timing_ms.get(operation, 0.0) + elapsed_ms,
                3,
            )


class QdrantHttp:
    """Tiny Qdrant REST client for the spike harness."""

    def __init__(self, base_url: str, collection: str, timeout_seconds: float = 2.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.collection = collection
        self.timeout_seconds = timeout_seconds
        self.stats = QdrantStats.empty()

    def is_available(self) -> bool:
        try:
            self._request_json("GET", "/collections", None, "availability_check")
        except SpikeError:
            return False
        return True

    def reset_collection(self, dimension: int) -> None:
        try:
            self._request_json("DELETE", self._collection_path(), None, "collection_delete")
        except SpikeError:
            pass
        body: JsonValue = {"vectors": {"size": dimension, "distance": "Cosine"}}
        self._request_json("PUT", self._collection_path(), body, "collection_create")

    def upsert_points(self, records: list[ChunkRecord]) -> None:
        points: list[JsonValue] = []
        for record in records:
            payload = qdrant_payload(record)
            assert_payload_private(payload)
            points.append(
                {
                    "id": qdrant_point_id(record.chunk_id),
                    "vector": record.vector,
                    "payload": payload,
                }
            )
        self._request_json(
            "PUT",
            f"{self._collection_path()}/points?wait=true",
            {"points": points},
            "upsert_requests",
        )
        self.stats.add_count("upsert_points", len(points))

    def search(self, query_vector: list[float], limit: int) -> list[RemoteHit]:
        body: JsonValue = {
            "vector": query_vector,
            "limit": limit,
            "with_payload": ["chunk_id", "profile_generation"],
            "with_vector": False,
        }
        data = self._request_json(
            "POST",
            f"{self._collection_path()}/points/search",
            body,
            "search_requests",
        )
        result = data.get("result", []) if isinstance(data, dict) else []
        hits: list[RemoteHit] = []
        if not isinstance(result, list):
            return hits
        for point in result:
            if not isinstance(point, dict):
                continue
            payload = point.get("payload")
            if not isinstance(payload, dict):
                continue
            chunk_id = payload.get("chunk_id")
            profile_generation = payload.get("profile_generation")
            score = point.get("score", 0.0)
            if isinstance(chunk_id, str) and isinstance(profile_generation, int):
                hits.append(RemoteHit(chunk_id, float(score), profile_generation))
        return hits

    def _collection_path(self) -> str:
        return f"/collections/{self.collection}"

    def _request_json(
        self,
        method: str,
        path: str,
        body: JsonValue,
        operation: str,
        count: int = 1,
    ) -> dict[str, JsonValue]:
        url = f"{self.base_url}{path}"
        payload = None if body is None else json.dumps(body).encode("utf-8")
        headers = {"Content-Type": "application/json"}
        request = urllib.request.Request(url, data=payload, headers=headers, method=method)
        started = time.perf_counter()
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
                raw = response.read()
        except urllib.error.HTTPError as exc:
            elapsed_ms = (time.perf_counter() - started) * 1000.0
            self.stats.add(operation, elapsed_ms, count)
            if method == "DELETE" and exc.code == 404:
                return {}
            if method == "GET" and exc.code == 404:
                return {}
            raise SpikeError(f"{operation}: qdrant returned HTTP {exc.code}") from exc
        except (OSError, TimeoutError, urllib.error.URLError) as exc:
            elapsed_ms = (time.perf_counter() - started) * 1000.0
            self.stats.add(operation, elapsed_ms, count)
            raise SpikeError(f"{operation}: qdrant unavailable at {url}") from exc
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        self.stats.add(operation, elapsed_ms, count)
        if not raw:
            return {}
        try:
            decoded = json.loads(raw.decode("utf-8"))
        except json.JSONDecodeError as exc:
            raise SpikeError(f"{operation}: qdrant returned invalid JSON") from exc
        if not isinstance(decoded, dict):
            raise SpikeError(f"{operation}: qdrant returned non-object JSON")
        return decoded


def fixture_sources() -> list[FixtureSource]:
    return [
        FixtureSource(
            "spike-source-001",
            "Vector Sink Baseline",
            "Local vector storage writes every child embedding into SQLite before "
            "retrieval scans or resident HNSW indexing can use it. The benchmark "
            "needs source throughput, chunk throughput, vector throughput, CPU "
            "core seconds, write amplification, and retrieval latency evidence. "
            "This source repeats baseline ingest terms so dense retrieval has a "
            "stable target for comparison across all variants.",
        ),
        FixtureSource(
            "spike-source-002",
            "Qdrant Cache Path",
            "Qdrant cache mode mirrors vectors from the local authoritative table "
            "after local ingest commits. It may improve dense search latency when "
            "the remote service is healthy, but ingest still pays local vector "
            "writes and remote upsert work. Cache mode must fall back to local "
            "retrieval when Qdrant is unavailable.",
        ),
        FixtureSource(
            "spike-source-003",
            "Primary Prototype Path",
            "Qdrant primary mode is an explicit experimental prototype in this "
            "spike. SQLite still stores metadata, chunk text, stable identifiers, "
            "and profile generation, but the primary vector sink is remote. Remote "
            "hits must hydrate through SQLite before evidence is returned.",
        ),
        FixtureSource(
            "spike-source-004",
            "Correctness And Privacy",
            "Freshness checks must reject missing, stale, and unembedded collection "
            "members before collection scoped retrieval. Qdrant payloads must avoid "
            "raw private text and private source paths. Only stable ids, bounded "
            "previews, headings, profile generation, and bounded metadata are "
            "allowed in remote payloads.",
        ),
    ]


def fixture_hash(sources: list[FixtureSource]) -> str:
    digest = hashlib.sha256()
    for source in sources:
        digest.update(source.source_id.encode("utf-8"))
        digest.update(b"\0")
        digest.update(source.title.encode("utf-8"))
        digest.update(b"\0")
        digest.update(source.text.encode("utf-8"))
        digest.update(b"\0")
    return digest.hexdigest()


def build_manifest(
    variant: str,
    output_root: Path = DEFAULT_OUTPUT_ROOT,
    qdrant_url: str = DEFAULT_QDRANT_URL,
) -> dict[str, JsonValue]:
    validate_variant(variant)
    sources = fixture_sources()
    digest = fixture_hash(sources)
    variant_dir = output_root / variant
    collection_id = f"verbatim_spike_{digest[:12]}_{variant.replace('-', '_')}"
    manifest: dict[str, JsonValue] = {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "variant": variant,
        "experimental_mode": variant == "qdrant-primary",
        "collection": {
            "id": "qdrant-spike-fixture",
            "source_count": len(sources),
            "source_ids": [source.source_id for source in sources],
        },
        "fixture_hash": digest,
        "expected_qdrant_collection_id": collection_id,
        "paths": {
            "profile_dir": str(variant_dir / "profile"),
            "config_path": str(variant_dir / "profile" / "config" / "verbatim" / "config.toml"),
            "data_dir": str(variant_dir / "profile" / "data" / "verbatim"),
            "fixture_dir": str(variant_dir / "fixture"),
            "results_path": str(variant_dir / "results.json"),
        },
        "qdrant": {
            "url": qdrant_url,
            "collection": collection_id,
        },
        "commands": {
            "dry_run": f"just bench-qdrant-spike --variant {variant} --dry-run",
            "run": f"just bench-qdrant-spike --variant {variant}",
        },
    }
    manifest["run_manifest_hash"] = stable_json_hash(manifest)
    return manifest


def run_variant(
    variant: str,
    output_root: Path = DEFAULT_OUTPUT_ROOT,
    qdrant_url: str = DEFAULT_QDRANT_URL,
    dry_run: bool = False,
) -> dict[str, JsonValue]:
    manifest = build_manifest(variant, output_root, qdrant_url)
    paths = assert_isolated_manifest_paths(manifest)
    if dry_run:
        print_dry_run(manifest)
        return manifest

    reset_variant_dir(paths["variant_dir"])
    paths["fixture_dir"].mkdir(parents=True, exist_ok=True)
    paths["data_dir"].mkdir(parents=True, exist_ok=True)
    paths["config_path"].parent.mkdir(parents=True, exist_ok=True)
    write_fixture_files(paths["fixture_dir"])
    write_isolated_config(paths["config_path"], paths["data_dir"], manifest)

    start_wall = time.perf_counter()
    start_cpu = process_cpu_seconds()
    start_write_bytes = proc_write_bytes()
    start_data_bytes = directory_size(paths["data_dir"])

    conn = sqlite3.connect(paths["data_dir"] / "verbatim-spike.db")
    try:
        create_schema(conn)
        chunks = ingest_fixture(conn, variant, manifest)
        qdrant_stats = QdrantStats.empty()
        qdrant_available = False
        qdrant_error: str | None = None
        remote_hits_available = False
        if variant in {"qdrant-cache", "qdrant-primary"}:
            qdrant = QdrantHttp(qdrant_url, qdrant_collection(manifest))
            qdrant_available = qdrant.is_available()
            if qdrant_available:
                qdrant.reset_collection(VECTOR_DIMENSION)
                qdrant.upsert_points(chunks)
                remote_hits_available = True
            elif variant == "qdrant-primary":
                raise SpikeError(
                    "qdrant-primary requires reachable Qdrant; run dry-run or "
                    "start an isolated Qdrant service before benchmarking primary mode"
                )
            else:
                qdrant_error = "qdrant unavailable; qdrant-cache fell back to local search"
            qdrant_stats = qdrant.stats
        freshness = check_freshness(conn, manifest, variant, remote_hits_available)
        retrieve_samples, correctness, retrieve_qdrant_stats = measure_retrieve_latency(
            conn,
            manifest,
            variant,
            qdrant_url,
            qdrant_available,
        )
        qdrant_stats.merge(retrieve_qdrant_stats)
    finally:
        conn.close()

    duration_seconds = max(time.perf_counter() - start_wall, 0.000_001)
    cpu_seconds = max(process_cpu_seconds() - start_cpu, 0.0)
    end_write_bytes = proc_write_bytes()
    end_data_bytes = directory_size(paths["data_dir"])
    if end_write_bytes is None or start_write_bytes is None:
        write_bytes = max(end_data_bytes - start_data_bytes, 0)
    else:
        write_bytes = max(end_write_bytes - start_write_bytes, end_data_bytes - start_data_bytes, 0)

    source_count = int(manifest["collection"]["source_count"])  # type: ignore[index]
    chunk_count = len(chunks)
    vector_count = chunk_count
    privacy = inspect_privacy(chunks)
    local_vector_rows = count_rows(paths["data_dir"] / "verbatim-spike.db", "vectors")

    result: dict[str, JsonValue] = {
        "schema_version": RESULT_SCHEMA_VERSION,
        "variant": variant,
        "experimental_mode": variant == "qdrant-primary",
        "run_manifest_hash": manifest["run_manifest_hash"],
        "manifest": manifest,
        "metrics": {
            "source_per_sec": round(source_count / duration_seconds, 6),
            "chunks_per_sec": round(chunk_count / duration_seconds, 6),
            "vectors_per_sec": round(vector_count / duration_seconds, 6),
            "cpu_core_sec_per_source": round(cpu_seconds / source_count, 6),
            "physical_write_mib_per_source": round(
                write_bytes / (1024 * 1024) / source_count,
                6,
            ),
            "cpu_scope": "local_harness_process_only",
            "physical_write_scope": "local_harness_process_and_sqlite_profile_only",
            "qdrant_service_cpu_core_sec_per_source": None,
            "qdrant_service_physical_write_mib_per_source": None,
            "external_service_unmeasured": variant in {"qdrant-cache", "qdrant-primary"},
            "retrieve_latency": latency_summary(retrieve_samples),
            "run_duration_seconds": round(duration_seconds, 6),
        },
        "counts": {
            "sources": source_count,
            "chunks": chunk_count,
            "vectors": vector_count,
        },
        "freshness": freshness,
        "correctness": correctness,
        "privacy": privacy,
        "qdrant": {
            "url": qdrant_url,
            "collection": qdrant_collection(manifest),
            "available": qdrant_available,
            "error": qdrant_error,
            "operation_counts": qdrant_stats.operation_counts,
            "operation_timing_ms": qdrant_stats.operation_timing_ms,
            "local_vector_writes": variant in {"local", "qdrant-cache"},
            "local_vector_rows": local_vector_rows,
            "primary_vector_sink": "qdrant" if variant == "qdrant-primary" else "sqlite",
            "hnsw_primary_write": False,
            "external_service_unmeasured": variant in {"qdrant-cache", "qdrant-primary"},
            "service_cpu_core_sec_per_source": None,
            "service_physical_write_mib_per_source": None,
        },
        "measurement_scope": {
            "cpu_core_sec_per_source": "local harness process only; remote Qdrant service CPU is not included",
            "physical_write_mib_per_source": "local harness process and isolated SQLite/profile writes only; remote Qdrant service writes are not included",
            "qdrant_service_metrics": "unmeasured unless qdrant_service_* fields are non-null",
        },
        "artifacts": {
            "results_json": str(paths["results_path"]),
            "config_path": str(paths["config_path"]),
            "data_dir": str(paths["data_dir"]),
            "fixture_dir": str(paths["fixture_dir"]),
        },
        "limitations": result_limitations(variant, qdrant_available),
    }
    paths["results_path"].parent.mkdir(parents=True, exist_ok=True)
    paths["results_path"].write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(f"results_json={paths['results_path']}")
    print(f"RUN_RESULTS_JSON={json.dumps(result, sort_keys=True)}")
    return result


def run_failure_modes(
    output_root: Path = DEFAULT_OUTPUT_ROOT,
    qdrant_url: str = DEFAULT_QDRANT_URL,
) -> dict[str, JsonValue]:
    manifest = build_manifest("qdrant-cache", output_root, qdrant_url)
    paths = assert_isolated_manifest_paths(manifest)
    paths["variant_dir"].mkdir(parents=True, exist_ok=True)

    cases: list[dict[str, JsonValue]] = [
        qdrant_unavailable_case(output_root / "failure-modes-unavailable")
    ]

    reset_case = collection_reset_case(manifest, qdrant_url)
    cases.append(reset_case)
    cases.append(stale_remote_hit_case(output_root))

    report: dict[str, JsonValue] = {
        "schema_version": FAILURE_SCHEMA_VERSION,
        "manifest": manifest,
        "cases": cases,
        "verdict": aggregate_failure_verdict(cases),
    }
    output_path = paths["variant_dir"] / "failure-modes.json"
    output_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(format_failure_modes(report))
    print(f"failure_modes_json={output_path}")
    print(f"FAILURE_MODES_JSON={json.dumps(report, sort_keys=True)}")
    return report


def aggregate_failure_verdict(cases: list[dict[str, JsonValue]]) -> str:
    verdicts = {str(case.get("verdict", "fail")) for case in cases}
    if verdicts == {"pass"}:
        return "pass"
    if "fail" in verdicts:
        return "fail"
    if verdicts & {"not_covered", "skipped"}:
        return "not_covered"
    return "fail"


def qdrant_unavailable_case(output_root: Path) -> dict[str, JsonValue]:
    cache_result: dict[str, JsonValue] | None = None
    cache_error: str | None = None
    primary_error: str | None = None
    try:
        cache_result = run_variant(
            "qdrant-cache",
            output_root=output_root,
            qdrant_url=UNAVAILABLE_QDRANT_URL,
        )
    except SpikeError as exc:
        cache_error = str(exc)

    try:
        run_variant(
            "qdrant-primary",
            output_root=output_root,
            qdrant_url=UNAVAILABLE_QDRANT_URL,
        )
    except SpikeError as exc:
        primary_error = str(exc)

    cache_fell_back = False
    cache_final_evidence = 0
    cache_qdrant_available: bool | None = None
    cache_result_path: str | None = None
    if cache_result is not None:
        qdrant = cache_result.get("qdrant", {})
        correctness = cache_result.get("correctness", {})
        artifacts = cache_result.get("artifacts", {})
        if isinstance(qdrant, dict):
            cache_qdrant_available = bool(qdrant.get("available"))
            cache_fell_back = (
                qdrant.get("available") is False
                and isinstance(qdrant.get("error"), str)
                and "fell back to local search" in str(qdrant.get("error"))
            )
        if isinstance(correctness, dict):
            cache_final_evidence = int(correctness.get("final_evidence_count", 0))
        if isinstance(artifacts, dict):
            cache_result_path = str(artifacts.get("results_json", ""))

    primary_failed_closed = primary_error is not None and "qdrant-primary requires reachable Qdrant" in primary_error
    verdict = "pass" if cache_fell_back and cache_final_evidence > 0 and primary_failed_closed else "fail"
    return {
        "case": "qdrant_unavailable",
        "expected": "cache mode executes local fallback; primary mode executes fail-closed path",
        "observed": {
            "cache_result_path": cache_result_path,
            "cache_error": cache_error,
            "cache_qdrant_available": cache_qdrant_available,
            "cache_fell_back_to_local": cache_fell_back,
            "cache_final_evidence_count": cache_final_evidence,
            "primary_error": primary_error,
            "primary_failed_closed": primary_failed_closed,
        },
        "covered": True,
        "verdict": verdict,
    }


def collection_reset_case(manifest: dict[str, JsonValue], qdrant_url: str) -> dict[str, JsonValue]:
    qdrant = QdrantHttp(qdrant_url, qdrant_collection(manifest))
    if not qdrant.is_available():
        return {
            "case": "collection_reset",
            "expected": "remote collection reset does not return stale evidence",
            "observed": {
                "qdrant_available": False,
                "reset_exercised": False,
                "search_exercised": False,
                "reason": "Qdrant unavailable; reset coverage requires a reachable Qdrant service",
            },
            "covered": False,
            "verdict": "not_covered",
        }
    try:
        qdrant.reset_collection(VECTOR_DIMENSION)
        hits = qdrant.search(deterministic_vector(DEFAULT_QUERY), 5)
    except SpikeError as exc:
        return {
            "case": "collection_reset",
            "expected": "remote collection reset does not return stale evidence",
            "observed": f"Qdrant reset/search error: {exc}",
            "verdict": "fail",
        }
    return {
        "case": "collection_reset",
        "expected": "after reset, remote search returns no final evidence until re-upsert and hydration",
        "observed": {
            "qdrant_available": True,
            "reset_exercised": True,
            "search_exercised": True,
            "remote_hit_count": len(hits),
        },
        "covered": True,
        "verdict": "pass" if not hits else "fail",
    }


def stale_remote_hit_case(output_root: Path) -> dict[str, JsonValue]:
    with tempfile.TemporaryDirectory(prefix="verbatim-qdrant-stale-") as tmp:
        manifest = build_manifest("qdrant-primary", output_root=Path(tmp))
        paths = manifest_paths(manifest)
        paths["data_dir"].mkdir(parents=True, exist_ok=True)
        conn = sqlite3.connect(paths["data_dir"] / "verbatim-spike.db")
        try:
            create_schema(conn)
            chunks = ingest_fixture(conn, "qdrant-primary", manifest)
            current_generation = current_profile_generation(conn)
            conn.execute(
                "UPDATE chunks SET capability = ? WHERE id = ?",
                ("wrong-capability", chunks[1].chunk_id),
            )
            conn.commit()
            fake_hits = [
                RemoteHit(chunks[0].chunk_id, 0.99, current_generation - 1),
                RemoteHit("missing-chunk", 0.98, current_generation),
                RemoteHit(chunks[1].chunk_id, 0.97, current_generation),
            ]
            evidence, counters = hydrate_remote_hits(conn, fake_hits, current_generation, manifest)
        finally:
            conn.close()
    rejected = (
        counters["stale_generation_rejected"]
        + counters["missing_chunk_rejected"]
        + counters["capability_mismatch_rejected"]
    )
    return {
        "case": "stale_remote_hits",
        "expected": "stale generation, missing, and capability-mismatch remote hits are rejected before final evidence",
        "observed": (
            f"final_evidence={len(evidence)} rejected={rejected} "
            f"capability_mismatch_rejected={counters['capability_mismatch_rejected']}"
        ),
        "covered": True,
        "verdict": "pass" if not evidence and rejected == 3 else "fail",
    }


def validate_variant(variant: str) -> None:
    if variant not in VARIANTS:
        raise SpikeError(f"variant must be one of {', '.join(VARIANTS)}")


def manifest_paths(manifest: dict[str, JsonValue]) -> dict[str, Path]:
    paths_value = manifest["paths"]
    if not isinstance(paths_value, dict):
        raise SpikeError("manifest paths must be an object")
    results_path = Path(str(paths_value["results_path"]))
    return {
        "variant_dir": results_path.parent,
        "profile_dir": Path(str(paths_value["profile_dir"])),
        "config_path": Path(str(paths_value["config_path"])),
        "data_dir": Path(str(paths_value["data_dir"])),
        "fixture_dir": Path(str(paths_value["fixture_dir"])),
        "results_path": results_path,
    }


def assert_isolated_manifest_paths(manifest: dict[str, JsonValue]) -> dict[str, Path]:
    paths = manifest_paths(manifest)
    for path in paths.values():
        assert_isolated_path(path)
    return paths


def qdrant_collection(manifest: dict[str, JsonValue]) -> str:
    qdrant = manifest["qdrant"]
    if not isinstance(qdrant, dict):
        raise SpikeError("manifest qdrant must be an object")
    return str(qdrant["collection"])


def reset_variant_dir(variant_dir: Path) -> None:
    if variant_dir.exists():
        assert_isolated_path(variant_dir)
        shutil.rmtree(variant_dir)
    variant_dir.mkdir(parents=True, exist_ok=True)


def assert_isolated_path(path: Path) -> None:
    resolved = path.expanduser().resolve()
    real_config = REAL_CONFIG_PATH.expanduser().resolve()
    real_data = REAL_DATA_DIR.expanduser().resolve()
    if resolved == real_config or real_config in resolved.parents:
        raise SpikeError(f"refusing to use real Verbatim config path: {resolved}")
    if resolved == real_data or real_data in resolved.parents:
        raise SpikeError(f"refusing to use real Verbatim data dir: {resolved}")
    if "qdrant-spike" not in resolved.parts and not str(resolved).startswith(tempfile.gettempdir()):
        raise SpikeError(f"spike path must be under target/qdrant-spike or tempdir: {resolved}")


def print_dry_run(manifest: dict[str, JsonValue]) -> None:
    paths = manifest_paths(manifest)
    print(f"variant={manifest['variant']}")
    print(f"experimental_mode={str(manifest['experimental_mode']).lower()}")
    print(f"source_count={manifest['collection']['source_count']}")  # type: ignore[index]
    print(f"expected_collection_id={manifest['expected_qdrant_collection_id']}")
    print(f"isolated_data_dir={paths['data_dir']}")
    print(f"config_path={paths['config_path']}")
    if manifest["experimental_mode"]:
        print("mode_notice=qdrant-primary is experimental and changes only the spike harness path")
    print(f"RUN_MANIFEST_JSON={json.dumps(manifest, sort_keys=True)}")


def stable_json_hash(value: dict[str, JsonValue]) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def write_fixture_files(fixture_dir: Path) -> None:
    for source in fixture_sources():
        path = fixture_dir / f"{source.source_id}.txt"
        path.write_text(f"# {source.title}\n\n{source.text}\n")


def write_isolated_config(
    config_path: Path,
    data_dir: Path,
    manifest: dict[str, JsonValue],
) -> None:
    qdrant = manifest["qdrant"]
    if not isinstance(qdrant, dict):
        raise SpikeError("manifest qdrant must be an object")
    text = "\n".join(
        [
            "[store]",
            f'path = "{data_dir}"',
            "",
            "[embedding]",
            "enabled = false",
            'provider = "openai_compatible"',
            'base_url = "http://127.0.0.1:8002/v1"',
            'model = "deterministic-spike"',
            f"dimension = {VECTOR_DIMENSION}",
            "",
            "[chat]",
            "enabled = false",
            "",
            "[qdrant]",
            f"enabled = {str(manifest['variant'] in {'qdrant-cache', 'qdrant-primary'}).lower()}",
            f'url = "{qdrant["url"]}"',
            f'collection = "{qdrant["collection"]}"',
            f"prefer_for_search = {str(manifest['variant'] in {'qdrant-cache', 'qdrant-primary'}).lower()}",
            "timeout_seconds = 2",
            "",
            "[vector_index]",
            'residency = "low_memory"',
            "",
        ]
    )
    config_path.write_text(text)


def create_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS profile_meta (
            profile_id TEXT PRIMARY KEY,
            generation INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sources (
            id TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            indexed_generation INTEGER NOT NULL,
            embedded_chunks INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS chunks (
            id TEXT PRIMARY KEY,
            source_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            heading_path_json TEXT NOT NULL,
            text TEXT NOT NULL,
            text_preview TEXT NOT NULL,
            collection_id TEXT NOT NULL,
            capability TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS vectors (
            chunk_id TEXT PRIMARY KEY,
            source_id TEXT NOT NULL,
            profile_generation INTEGER NOT NULL,
            vector_json TEXT NOT NULL
        );
        """
    )
    conn.execute(
        "INSERT OR REPLACE INTO profile_meta(profile_id, generation) VALUES (?, ?)",
        ("default", 1),
    )
    conn.commit()


def ingest_fixture(
    conn: sqlite3.Connection,
    variant: str,
    manifest: dict[str, JsonValue],
) -> list[ChunkRecord]:
    profile_generation = current_profile_generation(conn)
    collection = manifest["collection"]
    if not isinstance(collection, dict):
        raise SpikeError("manifest collection must be an object")
    collection_id = str(collection["id"])
    chunks: list[ChunkRecord] = []
    for source in fixture_sources():
        words = source.text.split()
        chunk_words = 42
        source_chunk_count = 0
        for ordinal, offset in enumerate(range(0, len(words), chunk_words)):
            chunk_text = " ".join(words[offset : offset + chunk_words])
            if not chunk_text:
                continue
            chunk_id = f"{source.source_id}-chunk-{ordinal:03d}"
            record = ChunkRecord(
                chunk_id=chunk_id,
                source_id=source.source_id,
                ordinal=ordinal,
                heading_path=[source.title],
                text=chunk_text,
                text_preview=chunk_text[:TEXT_PREVIEW_CHARS],
                vector=deterministic_vector(chunk_text),
                profile_generation=profile_generation,
            )
            chunks.append(record)
            source_chunk_count += 1
            conn.execute(
                """
                INSERT OR REPLACE INTO chunks(
                    id, source_id, ordinal, heading_path_json, text, text_preview,
                    collection_id, capability
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    record.chunk_id,
                    record.source_id,
                    record.ordinal,
                    json.dumps(record.heading_path),
                    record.text,
                    record.text_preview,
                    collection_id,
                    EXPECTED_CAPABILITY,
                ),
            )
            if variant in {"local", "qdrant-cache"}:
                conn.execute(
                    """
                    INSERT OR REPLACE INTO vectors(
                        chunk_id, source_id, profile_generation, vector_json
                    ) VALUES (?, ?, ?, ?)
                    """,
                    (
                        record.chunk_id,
                        record.source_id,
                        record.profile_generation,
                        json.dumps(record.vector),
                    ),
                )
        content_hash = hashlib.sha256(source.text.encode("utf-8")).hexdigest()
        conn.execute(
            """
            INSERT OR REPLACE INTO sources(
                id, content_hash, indexed_generation, embedded_chunks
            ) VALUES (?, ?, ?, ?)
            """,
            (source.source_id, content_hash, profile_generation, source_chunk_count),
        )
    conn.commit()
    return chunks


def current_profile_generation(conn: sqlite3.Connection) -> int:
    row = conn.execute(
        "SELECT generation FROM profile_meta WHERE profile_id = ?",
        ("default",),
    ).fetchone()
    if row is None:
        raise SpikeError("missing default profile generation")
    return int(row[0])


def deterministic_vector(text: str) -> list[float]:
    values: list[float] = []
    counter = 0
    while len(values) < VECTOR_DIMENSION:
        digest = hashlib.sha256(f"{counter}:{text}".encode("utf-8")).digest()
        for byte in digest:
            values.append((byte / 127.5) - 1.0)
            if len(values) == VECTOR_DIMENSION:
                break
        counter += 1
    norm = math.sqrt(sum(value * value for value in values)) or 1.0
    return [round(value / norm, 8) for value in values]


def qdrant_point_id(chunk_id: str) -> str:
    digest = hashlib.sha256(f"verbatim:qdrant:default:{chunk_id}".encode("utf-8")).digest()
    data = bytearray(digest[:16])
    data[6] = (data[6] & 0x0F) | 0x50
    data[8] = (data[8] & 0x3F) | 0x80
    return (
        f"{data[0]:02x}{data[1]:02x}{data[2]:02x}{data[3]:02x}-"
        f"{data[4]:02x}{data[5]:02x}-{data[6]:02x}{data[7]:02x}-"
        f"{data[8]:02x}{data[9]:02x}-"
        f"{data[10]:02x}{data[11]:02x}{data[12]:02x}{data[13]:02x}{data[14]:02x}{data[15]:02x}"
    )


def qdrant_payload(record: ChunkRecord) -> dict[str, JsonValue]:
    return {
        "profile_id": "default",
        "profile_generation": record.profile_generation,
        "chunk_id": record.chunk_id,
        "source_id": record.source_id,
        "heading_path": record.heading_path,
        "text_preview": record.text_preview[:TEXT_PREVIEW_CHARS],
    }


def assert_payload_private(payload: dict[str, JsonValue]) -> None:
    fields = set(payload)
    forbidden = fields & FORBIDDEN_PAYLOAD_FIELDS
    if forbidden:
        raise SpikeError(f"forbidden Qdrant payload fields: {sorted(forbidden)}")
    unexpected = fields - ALLOWED_PAYLOAD_FIELDS
    if unexpected:
        raise SpikeError(f"unexpected Qdrant payload fields: {sorted(unexpected)}")
    preview = payload.get("text_preview", "")
    if not isinstance(preview, str):
        raise SpikeError("Qdrant text_preview must be a string")
    if len(preview) > TEXT_PREVIEW_CHARS:
        raise SpikeError("Qdrant text_preview exceeds bounded preview length")
    source_id = payload.get("source_id", "")
    if not isinstance(source_id, str) or "/" in source_id or str(Path.home()) in source_id:
        raise SpikeError("Qdrant source_id must be stable and path-free")


def inspect_privacy(chunks: list[ChunkRecord]) -> dict[str, JsonValue]:
    fields_seen: set[str] = set()
    max_preview = 0
    for chunk in chunks:
        payload = qdrant_payload(chunk)
        assert_payload_private(payload)
        fields_seen.update(payload)
        preview = payload.get("text_preview", "")
        if isinstance(preview, str):
            max_preview = max(max_preview, len(preview))
    return {
        "inspected_payload_fields": sorted(fields_seen),
        "allowed_payload_fields": sorted(ALLOWED_PAYLOAD_FIELDS),
        "forbidden_payload_fields": sorted(FORBIDDEN_PAYLOAD_FIELDS),
        "raw_text_fields_present": sorted(fields_seen & FORBIDDEN_PAYLOAD_FIELDS),
        "bounded_preview_max_chars": TEXT_PREVIEW_CHARS,
        "max_preview_chars_seen": max_preview,
        "private_source_paths_present": False,
        "verdict": "pass",
    }


def check_freshness(
    conn: sqlite3.Connection,
    manifest: dict[str, JsonValue],
    variant: str,
    remote_hits_available: bool,
) -> dict[str, JsonValue]:
    source_ids = manifest["collection"]["source_ids"]  # type: ignore[index]
    if not isinstance(source_ids, list):
        raise SpikeError("manifest source ids must be a list")
    current_generation = current_profile_generation(conn)
    missing: list[str] = []
    stale: list[str] = []
    unembedded: list[str] = []
    for source_id in source_ids:
        row = conn.execute(
            "SELECT indexed_generation, embedded_chunks FROM sources WHERE id = ?",
            (str(source_id),),
        ).fetchone()
        if row is None:
            missing.append(str(source_id))
            continue
        if int(row[0]) != current_generation:
            stale.append(str(source_id))
        if int(row[1]) == 0:
            unembedded.append(str(source_id))
    if variant == "qdrant-primary" and not remote_hits_available:
        unembedded.extend(str(source_id) for source_id in source_ids if source_id not in unembedded)
    action = "continue" if not missing and not stale and not unembedded else "ingest_and_wait_required"
    return {
        "checked": True,
        "collection_id": manifest["collection"]["id"],  # type: ignore[index]
        "missing_sources": missing,
        "stale_sources": stale,
        "unembedded_sources": unembedded,
        "action": action,
        "verdict": "pass" if action == "continue" else "fail",
    }


def measure_retrieve_latency(
    conn: sqlite3.Connection,
    manifest: dict[str, JsonValue],
    variant: str,
    qdrant_url: str,
    qdrant_available: bool,
) -> tuple[list[float], dict[str, JsonValue], QdrantStats]:
    samples: list[float] = []
    correctness: dict[str, int] = {
        "hydrated_remote_hits": 0,
        "stale_generation_rejected": 0,
        "missing_chunk_rejected": 0,
        "capability_mismatch_rejected": 0,
        "final_evidence_count": 0,
    }
    current_generation = current_profile_generation(conn)
    qdrant = QdrantHttp(qdrant_url, qdrant_collection(manifest), 1.0) if qdrant_available else None
    if variant == "qdrant-primary" and qdrant is None:
        raise SpikeError("qdrant-primary requires remote search before retrieval measurement")
    for _ in range(RETRIEVE_ITERATIONS):
        started = time.perf_counter()
        if qdrant is not None and variant in {"qdrant-cache", "qdrant-primary"}:
            try:
                remote_hits = qdrant.search(deterministic_vector(DEFAULT_QUERY), 5)
            except SpikeError as exc:
                if variant == "qdrant-primary":
                    raise SpikeError("qdrant-primary remote search failed") from exc
                remote_hits = []
            evidence, counters = hydrate_remote_hits(conn, remote_hits, current_generation, manifest)
            if variant == "qdrant-primary" and not evidence:
                raise SpikeError("qdrant-primary remote search returned no hydrated evidence")
            if not evidence and variant == "qdrant-cache":
                evidence = local_dense_search(conn, DEFAULT_QUERY, 5)
        else:
            evidence = local_dense_search(conn, DEFAULT_QUERY, 5)
            counters = {
                "hydrated_remote_hits": 0,
                "stale_generation_rejected": 0,
                "missing_chunk_rejected": 0,
                "capability_mismatch_rejected": 0,
            }
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        samples.append(elapsed_ms)
        for key, value in counters.items():
            correctness[key] += int(value)
        correctness["final_evidence_count"] += len(evidence)
    correctness_value: dict[str, JsonValue] = {
        key: value for key, value in correctness.items()
    }
    correctness_value["evidence_hydration_required"] = variant in {"qdrant-cache", "qdrant-primary"}
    correctness_value["stale_or_mismatch_hits_entered_final_evidence"] = False
    correctness_value["verdict"] = correctness_verdict(variant, correctness)
    return samples, correctness_value, qdrant.stats if qdrant is not None else QdrantStats.empty()


def correctness_verdict(variant: str, correctness: dict[str, int]) -> str:
    if variant == "qdrant-primary" and correctness.get("hydrated_remote_hits", 0) == 0:
        return "fail"
    if correctness.get("final_evidence_count", 0) == 0:
        return "fail"
    return "pass"


def local_dense_search(
    conn: sqlite3.Connection,
    query: str,
    limit: int,
) -> list[dict[str, JsonValue]]:
    query_vector = deterministic_vector(query)
    rows = conn.execute(
        """
        SELECT chunks.id, chunks.source_id, chunks.text_preview, vectors.vector_json
        FROM vectors
        JOIN chunks ON chunks.id = vectors.chunk_id
        """
    ).fetchall()
    scored: list[tuple[float, dict[str, JsonValue]]] = []
    for row in rows:
        vector = json.loads(str(row[3]))
        if not isinstance(vector, list):
            continue
        score = cosine(query_vector, [float(value) for value in vector])
        scored.append(
            (
                score,
                {
                    "chunk_id": str(row[0]),
                    "source_id": str(row[1]),
                    "score": round(score, 6),
                    "text_preview": str(row[2]),
                },
            )
        )
    scored.sort(key=lambda item: item[0], reverse=True)
    return [item[1] for item in scored[:limit]]


def hydrate_remote_hits(
    conn: sqlite3.Connection,
    remote_hits: list[RemoteHit],
    current_generation: int,
    manifest: dict[str, JsonValue],
) -> tuple[list[dict[str, JsonValue]], dict[str, int]]:
    allowed_sources = set(manifest["collection"]["source_ids"])  # type: ignore[index]
    collection_id = str(manifest["collection"]["id"])  # type: ignore[index]
    counters = {
        "hydrated_remote_hits": 0,
        "stale_generation_rejected": 0,
        "missing_chunk_rejected": 0,
        "capability_mismatch_rejected": 0,
    }
    evidence: list[dict[str, JsonValue]] = []
    for hit in remote_hits:
        if hit.profile_generation != current_generation:
            counters["stale_generation_rejected"] += 1
            continue
        row = conn.execute(
            """
        SELECT id, source_id, text_preview, collection_id, capability
            FROM chunks
            WHERE id = ?
            """,
            (hit.chunk_id,),
        ).fetchone()
        if row is None:
            counters["missing_chunk_rejected"] += 1
            continue
        source_id = str(row[1])
        if (
            source_id not in allowed_sources
            or str(row[3]) != collection_id
            or str(row[4]) != EXPECTED_CAPABILITY
        ):
            counters["capability_mismatch_rejected"] += 1
            continue
        counters["hydrated_remote_hits"] += 1
        evidence.append(
            {
                "chunk_id": str(row[0]),
                "source_id": source_id,
                "score": round(hit.score, 6),
                "text_preview": str(row[2]),
                "hydrated_from_sqlite": True,
            }
        )
    return evidence, counters


def cosine(left: list[float], right: list[float]) -> float:
    if not left or not right or len(left) != len(right):
        return 0.0
    return sum(a * b for a, b in zip(left, right))


def latency_summary(samples: list[float]) -> dict[str, JsonValue]:
    rounded = [round(sample, 3) for sample in samples]
    return {
        "p50_ms": round(statistics.median(samples), 3) if samples else 0.0,
        "p95_ms": round(percentile(samples, 95), 3) if samples else 0.0,
        "samples_ms": rounded,
    }


def percentile(samples: list[float], percentile_value: int) -> float:
    if not samples:
        return 0.0
    ordered = sorted(samples)
    rank = math.ceil((percentile_value / 100.0) * len(ordered)) - 1
    return ordered[max(0, min(rank, len(ordered) - 1))]


def process_cpu_seconds() -> float:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return float(usage.ru_utime + usage.ru_stime)


def proc_write_bytes() -> int | None:
    path = Path("/proc/self/io")
    if not path.exists():
        return None
    try:
        for line in path.read_text().splitlines():
            if line.startswith("write_bytes:"):
                return int(line.split(":", 1)[1].strip())
    except OSError:
        return None
    return None


def directory_size(path: Path) -> int:
    if not path.exists():
        return 0
    total = 0
    for entry in path.rglob("*"):
        if entry.is_file():
            try:
                total += entry.stat().st_size
            except OSError:
                continue
    return total


def count_rows(db_path: Path, table: str) -> int:
    conn = sqlite3.connect(db_path)
    try:
        row = conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()
    finally:
        conn.close()
    return int(row[0]) if row else 0


def result_limitations(variant: str, qdrant_available: bool) -> list[str]:
    limitations: list[str] = [
        "Harness uses deterministic fixture embeddings; production migration remains separate.",
    ]
    if variant == "qdrant-primary":
        limitations.append("Primary mode is a spike prototype, not a production storage migration.")
    if variant in {"qdrant-cache", "qdrant-primary"} and not qdrant_available:
        limitations.append("Qdrant was unavailable, so remote timing was not measured.")
    return limitations


def format_failure_modes(report: dict[str, JsonValue]) -> str:
    lines = ["failure_modes:"]
    cases = report.get("cases", [])
    if isinstance(cases, list):
        for case in cases:
            if not isinstance(case, dict):
                continue
            lines.append(
                "- {case}: expected={expected}; observed={observed}; verdict={verdict}".format(
                    case=case.get("case", ""),
                    expected=case.get("expected", ""),
                    observed=case.get("observed", ""),
                    verdict=case.get("verdict", ""),
                )
            )
    lines.append(f"overall_verdict={report.get('verdict', '')}")
    return "\n".join(lines)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the Qdrant primary vector sink spike harness.",
    )
    parser.add_argument("--variant", choices=VARIANTS, default="local")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--failure-modes", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--output-root", type=Path, default=DEFAULT_OUTPUT_ROOT)
    parser.add_argument("--qdrant-url", default=os.environ.get("QDRANT_URL", DEFAULT_QDRANT_URL))
    return parser.parse_args(argv)


class HarnessTests(unittest.TestCase):
    def test_dry_run_manifest_uses_isolated_paths(self) -> None:
        manifest = build_manifest("local")
        paths = manifest_paths(manifest)
        self.assertEqual(manifest["schema_version"], MANIFEST_SCHEMA_VERSION)
        self.assertEqual(manifest["collection"]["source_count"], 4)  # type: ignore[index]
        self.assertNotEqual(paths["config_path"], REAL_CONFIG_PATH)
        self.assertNotEqual(paths["data_dir"], REAL_DATA_DIR)
        assert_isolated_path(paths["config_path"])
        assert_isolated_path(paths["data_dir"])

    def test_primary_dry_run_is_explicitly_experimental(self) -> None:
        manifest = build_manifest("qdrant-primary")
        self.assertTrue(manifest["experimental_mode"])
        self.assertIn("qdrant_primary", str(manifest["expected_qdrant_collection_id"]))

    def test_dry_run_rejects_unsafe_output_root(self) -> None:
        with self.assertRaisesRegex(SpikeError, "real Verbatim data dir"):
            run_variant("local", output_root=REAL_DATA_DIR, dry_run=True)

    def test_failure_modes_reject_unsafe_output_root_before_writes(self) -> None:
        with self.assertRaisesRegex(SpikeError, "real Verbatim data dir"):
            run_failure_modes(REAL_DATA_DIR, "http://127.0.0.1:9")

    def test_result_schema_and_primary_skip_local_vectors(self) -> None:
        with tempfile.TemporaryDirectory(prefix="verbatim-qdrant-test-") as tmp:
            output_root = Path(tmp) / "qdrant-spike"
            local = run_variant("local", output_root=output_root)
            self.assert_required_metrics(local)
            primary_manifest = build_manifest("qdrant-primary", output_root)
            paths = manifest_paths(primary_manifest)
            paths["data_dir"].mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(paths["data_dir"] / "verbatim-spike.db")
            try:
                create_schema(conn)
                ingest_fixture(conn, "qdrant-primary", primary_manifest)
                row = conn.execute("SELECT COUNT(*) FROM vectors").fetchone()
            finally:
                conn.close()
            self.assertEqual(row[0], 0)

    def test_primary_retrieve_requires_remote_hydrated_evidence(self) -> None:
        with tempfile.TemporaryDirectory(prefix="verbatim-qdrant-test-") as tmp:
            manifest = build_manifest("qdrant-primary", Path(tmp) / "qdrant-spike")
            paths = manifest_paths(manifest)
            paths["data_dir"].mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(paths["data_dir"] / "verbatim-spike.db")
            try:
                create_schema(conn)
                ingest_fixture(conn, "qdrant-primary", manifest)
                with self.assertRaisesRegex(SpikeError, "requires remote search"):
                    measure_retrieve_latency(
                        conn,
                        manifest,
                        "qdrant-primary",
                        UNAVAILABLE_QDRANT_URL,
                        qdrant_available=False,
                    )
            finally:
                conn.close()

    def test_hydration_rejects_capability_mismatch(self) -> None:
        with tempfile.TemporaryDirectory(prefix="verbatim-qdrant-test-") as tmp:
            manifest = build_manifest("qdrant-primary", Path(tmp) / "qdrant-spike")
            paths = manifest_paths(manifest)
            paths["data_dir"].mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(paths["data_dir"] / "verbatim-spike.db")
            try:
                create_schema(conn)
                chunks = ingest_fixture(conn, "qdrant-primary", manifest)
                conn.execute(
                    "UPDATE chunks SET capability = ? WHERE id = ?",
                    ("wrong-capability", chunks[0].chunk_id),
                )
                conn.commit()
                evidence, counters = hydrate_remote_hits(
                    conn,
                    [RemoteHit(chunks[0].chunk_id, 0.99, current_profile_generation(conn))],
                    current_profile_generation(conn),
                    manifest,
                )
            finally:
                conn.close()
        self.assertEqual(evidence, [])
        self.assertEqual(counters["capability_mismatch_rejected"], 1)

    def test_payload_privacy(self) -> None:
        manifest = build_manifest("qdrant-cache")
        with tempfile.TemporaryDirectory(prefix="verbatim-qdrant-test-") as tmp:
            paths = manifest_paths(build_manifest("qdrant-cache", Path(tmp) / "qdrant-spike"))
            paths["data_dir"].mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(paths["data_dir"] / "verbatim-spike.db")
            try:
                create_schema(conn)
                chunks = ingest_fixture(conn, "qdrant-cache", manifest)
            finally:
                conn.close()
        privacy = inspect_privacy(chunks)
        self.assertEqual(privacy["verdict"], "pass")
        self.assertEqual(privacy["raw_text_fields_present"], [])
        self.assertLessEqual(privacy["max_preview_chars_seen"], TEXT_PREVIEW_CHARS)

    def test_failure_modes_report_expected_cases(self) -> None:
        with tempfile.TemporaryDirectory(prefix="verbatim-qdrant-test-") as tmp:
            report = run_failure_modes(Path(tmp) / "qdrant-spike", "http://127.0.0.1:9")
        cases = {case["case"]: case for case in report["cases"]}  # type: ignore[index]
        self.assertEqual(set(cases), {"qdrant_unavailable", "collection_reset", "stale_remote_hits"})
        self.assertEqual(report["verdict"], "not_covered")
        unavailable = cases["qdrant_unavailable"]
        observed = unavailable["observed"]
        self.assertIsInstance(observed, dict)
        self.assertTrue(observed["cache_fell_back_to_local"])  # type: ignore[index]
        self.assertGreater(observed["cache_final_evidence_count"], 0)  # type: ignore[index]
        self.assertTrue(observed["primary_failed_closed"])  # type: ignore[index]
        self.assertEqual(unavailable["verdict"], "pass")
        reset = cases["collection_reset"]
        reset_observed = reset["observed"]
        self.assertEqual(reset["verdict"], "not_covered")
        self.assertFalse(reset["covered"])
        self.assertIsInstance(reset_observed, dict)
        self.assertFalse(reset_observed["reset_exercised"])  # type: ignore[index]
        stale = cases["stale_remote_hits"]
        self.assertIn("capability_mismatch_rejected=1", str(stale["observed"]))

    def test_collection_reset_pass_requires_exercised_reset(self) -> None:
        manifest = build_manifest("qdrant-cache")
        calls: dict[str, JsonValue] = {
            "reset_dimension": None,
            "search_limit": None,
        }
        original_qdrant_http = QdrantHttp

        class FakeQdrantHttp:
            def __init__(
                self,
                base_url: str,
                collection: str,
                timeout_seconds: float = 2.0,
            ) -> None:
                calls["base_url"] = base_url
                calls["collection"] = collection
                calls["timeout_seconds"] = timeout_seconds

            def is_available(self) -> bool:
                return True

            def reset_collection(self, dimension: int) -> None:
                calls["reset_dimension"] = dimension

            def search(self, query_vector: list[float], limit: int) -> list[RemoteHit]:
                calls["search_limit"] = limit
                calls["query_dimension"] = len(query_vector)
                return []

        try:
            globals()["QdrantHttp"] = FakeQdrantHttp
            case = collection_reset_case(manifest, DEFAULT_QDRANT_URL)
        finally:
            globals()["QdrantHttp"] = original_qdrant_http

        observed = case["observed"]
        self.assertEqual(case["verdict"], "pass")
        self.assertTrue(case["covered"])
        self.assertEqual(calls["reset_dimension"], VECTOR_DIMENSION)
        self.assertEqual(calls["search_limit"], 5)
        self.assertIsInstance(observed, dict)
        self.assertTrue(observed["reset_exercised"])  # type: ignore[index]
        self.assertTrue(observed["search_exercised"])  # type: ignore[index]

    def assert_required_metrics(self, result: dict[str, JsonValue]) -> None:
        metrics = result["metrics"]
        self.assertIsInstance(metrics, dict)
        for key in (
            "source_per_sec",
            "chunks_per_sec",
            "vectors_per_sec",
            "cpu_core_sec_per_source",
            "physical_write_mib_per_source",
            "retrieve_latency",
            "cpu_scope",
            "physical_write_scope",
            "qdrant_service_cpu_core_sec_per_source",
            "qdrant_service_physical_write_mib_per_source",
            "external_service_unmeasured",
        ):
            self.assertIn(key, metrics)
        retrieve_latency = metrics["retrieve_latency"]  # type: ignore[index]
        self.assertIn("p50_ms", retrieve_latency)
        self.assertIn("p95_ms", retrieve_latency)
        self.assertEqual(metrics["cpu_scope"], "local_harness_process_only")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if args.self_test:
            suite = unittest.defaultTestLoader.loadTestsFromTestCase(HarnessTests)
            result = unittest.TextTestRunner(verbosity=2).run(suite)
            return 0 if result.wasSuccessful() else 1
        if args.failure_modes:
            run_failure_modes(args.output_root, args.qdrant_url)
            return 0
        run_variant(args.variant, args.output_root, args.qdrant_url, args.dry_run)
    except SpikeError as exc:
        print(f"error={exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
