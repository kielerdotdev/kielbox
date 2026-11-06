"""Compare measured hidden-window evaluations, without launching the application."""
import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path


def summarize(path):
    report = json.loads(path.read_text(encoding="utf-8-sig"))
    failures = [run for run in report["runs"] if run["exit_code"] != 0]
    if failures:
        raise ValueError(f"Failed evaluations in {path}: {failures}")
    samples = defaultdict(list)
    for run in report["viewer"]:
        viewport = run["name"].split("-")[1]
        fixture = run.get("fixture", "video")
        metrics = run["metrics"]
        if not metrics["original_media_and_edits_unchanged"]:
            raise ValueError("Source/edit preservation did not pass")
        for row in metrics["samples"]:
            samples[viewport, fixture, row["operation"]].append(row["latency_ms"])
            if row.get("audio_clock_advancing_ms") is not None:
                samples[viewport, fixture, row["operation"] + ":audio_clock"].append(row["audio_clock_advancing_ms"])
    return {
        "report": str(path.resolve()),
        "sha256": report["sha256"],
        "scope": report["scope"],
        "measurements": [
            {"viewport": viewport, "fixture": fixture, "operation": operation, "runs": len(values),
             "median_ms": statistics.median(values), "max_ms": max(values)}
            for (viewport, fixture, operation), values in sorted(samples.items())
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument("--json", type=Path, dest="output")
    args = parser.parse_args()
    results = [summarize(path) for path in args.reports]
    for index, result in enumerate(results, 1):
        print(f"\nEvaluation {index}: {result['report']}")
        print(f"Build SHA256: {result['sha256']}")
        print("viewport  fixture operation                                             runs   median_ms      max_ms")
        for row in result["measurements"]:
            print(f"{row['viewport']:9} {row['fixture']:7} {row['operation']:55} {row['runs']:4} {row['median_ms']:11.1f} {row['max_ms']:11.1f}")
    if len(results) == 2:
        previous = {(row["viewport"], row["fixture"], row["operation"]): row for row in results[0]["measurements"]}
        print("\nChanges (current median / previous median; lower is faster):")
        for row in results[1]["measurements"]:
            old = previous.get((row["viewport"], row["fixture"], row["operation"]))
            if old and old["median_ms"] > 0:
                print(f"{row['viewport']:9} {row['operation']:50} {row['median_ms'] / old['median_ms']:7.2f}x")
    if args.output:
        args.output.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print("\nTimings end at hidden GPUI paint; they do not measure physical scanout or live game capture.")


if __name__ == "__main__":
    main()
