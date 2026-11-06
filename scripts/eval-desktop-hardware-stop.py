"""Opt-in real Desktop Stop evaluation; owns no microphone or user settings.

Uses the same button handler and Studio polling/worker-retirement path, with
hidden Studio renders and a small unfocused, owned visible activity marker.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "rcr_hardware_eval", ROOT / "scripts/eval-hardware-recording-stop.py")
HARDWARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARDWARE)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/debug/rcr-desktop.exe")
    parser.add_argument("--seconds", default="6,60")
    args = parser.parse_args()
    durations = [float(value) for value in args.seconds.split(",")]
    if not durations or any(not 1 <= value <= 120 for value in durations):
        parser.error("Each duration must be between 1 and 120 seconds")
    executable = args.exe.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    summary = {"scope": "Actual production Desktop recording/Stop, hidden Studio paint, "
               "owned unfocused activity marker and inaudible zero-PCM render; no microphone or uploads",
               "executable": str(executable),
               "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(), "cases": []}
    try:
        availability = HARDWARE.desktop_availability_preflight()
        (output / "desktop-availability.json").write_text(json.dumps(availability, indent=2), encoding="utf-8")
        if availability["status"] != "available":
            summary["status"] = "blocked_external_state"
            raise RuntimeError("Live Desktop evaluation requires an unlocked active display: "
                               + "; ".join(availability["reasons"]))
        with HARDWARE.matrix_display_awake(True, output):
            for seconds in durations:
                case = output / f"desktop-{seconds:g}s"
                case.mkdir()
                availability = HARDWARE.desktop_availability_preflight()
                (case / "availability-before.json").write_text(json.dumps(availability, indent=2), encoding="utf-8")
                if availability["status"] != "available":
                    summary["status"] = "blocked_external_state"
                    raise RuntimeError("Desktop became unavailable before capture: " + "; ".join(availability["reasons"]))
                environment = dict(os.environ, RCR_HARDWARE_STOP_SECONDS=str(seconds))
                render = HARDWARE.start_silent_render(seconds * 2 + 30)
                try:
                    result = subprocess.run(
                        [str(executable), "--eval-hardware-stop", "--proof-output", str(case)],
                        cwd=ROOT, env=environment, stdin=subprocess.DEVNULL,
                        capture_output=True, timeout=seconds * 2 + 90,
                        creationflags=HARDWARE.HIDDEN)
                    (case / "stdout.log").write_bytes(result.stdout)
                    (case / "stderr.log").write_bytes(result.stderr)
                    if result.returncode:
                        raise RuntimeError(f"Desktop proof exited {result.returncode}: "
                                           + result.stderr.decode(errors="replace")[-4096:])
                finally:
                    (case / "silent-render.stderr.log").write_bytes(HARDWARE.stop_owned_render(render))
                validated = HARDWARE.validate_report(case / "desktop-report.json", require_audio_packets=True)
                report = json.loads((case / "desktop-report.json").read_text(encoding="utf-8"))
                summary["cases"].append({"seconds": seconds, "rows": report["rows"], "validation": validated})
                availability = HARDWARE.desktop_availability_preflight()
                (case / "availability-after.json").write_text(json.dumps(availability, indent=2), encoding="utf-8")
                if not validated["validated"] or availability["status"] != "available":
                    summary["status"] = validated["status"] if not validated["validated"] else "blocked_external_state"
                    raise RuntimeError("Desktop live-content gate did not pass: "
                                       + str(validated.get("blockers", []) + availability["reasons"]))
                print(f"Actual Desktop {seconds:g}s no-audio/system cases fully decoded", flush=True)
        summary["success"] = True
    except Exception as error:
        summary["success"] = False
        summary["error"] = str(error)
        raise
    finally:
        (output / "desktop-evaluation.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
