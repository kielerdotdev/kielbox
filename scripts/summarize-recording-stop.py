"""Summarize generated_stop_latency_eval JSON without running capture or FFmpeg."""
import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    report = json.loads(args.report.read_text(encoding="utf-8"))
    groups = defaultdict(list)
    for row in report["rows"]:
        assert row["decoded"] and row["frame_counters_verified"] and row["children_reaped"]
        groups[row["seconds"], row["audio"]].append(row)
    print(report["scope"])
    print("Legacy comparator: old full-clip AAC + remux only; current stop also includes encoder drain and durability wait.")
    print("seconds audio runs current_stop_median_ms current_stop_max_ms legacy_aac_mux_median_ms")
    for (seconds, audio), rows in sorted(groups.items()):
        stop = [row["stop_ms"] for row in rows]
        baseline = [row["legacy_aac_mux_ms"] for row in rows if row["legacy_aac_mux_ms"] is not None]
        legacy = f"{statistics.median(baseline):.1f}" if baseline else "n/a"
        print(f"{seconds:7} {str(audio):5} {len(rows):4} {statistics.median(stop):22.1f} {max(stop):19.1f} {legacy}")
    print("Every output fully decoded; every generated frame counter verified; owned FFmpeg children reaped.")


if __name__ == "__main__":
    main()
