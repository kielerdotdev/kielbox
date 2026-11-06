"""Opt-in local DXGI/NVENC stop evaluation; microphone is never enabled.

Build separately: cargo build --locked -p rcr-windows --example hardware-stop-eval -j1
Run outside the app sandbox when Windows denies Desktop Duplication there.
Existing clips/reports are never overwritten. --validate-only performs no capture.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


ROOT = Path(__file__).resolve().parents[1]
HIDDEN = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0


def desktop_availability_preflight():
    """Read process/class/desktop state only; no screenshots, wake or unlock."""
    if os.name != "nt":
        return {"status": "blocked_external_state", "reasons": ["Windows is required"]}
    import ctypes
    from ctypes import wintypes
    user = ctypes.WinDLL("user32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    user.GetForegroundWindow.argtypes = []
    user.GetForegroundWindow.restype = wintypes.HWND
    user.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user.GetClassNameW.restype = ctypes.c_int
    user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user.GetWindowThreadProcessId.restype = wintypes.DWORD
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR,
                                                 ctypes.POINTER(wintypes.DWORD)]
    kernel.QueryFullProcessImageNameW.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    user.OpenInputDesktop.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    user.OpenInputDesktop.restype = wintypes.HANDLE
    user.GetUserObjectInformationW.argtypes = [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID,
                                              wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    user.GetUserObjectInformationW.restype = wintypes.BOOL
    user.CloseDesktop.argtypes = [wintypes.HANDLE]
    user.CloseDesktop.restype = wintypes.BOOL
    window = user.GetForegroundWindow()
    class_name = ctypes.create_unicode_buffer(256)
    user.GetClassNameW(window, class_name, len(class_name))
    pid = wintypes.DWORD()
    user.GetWindowThreadProcessId(window, ctypes.byref(pid))
    process_name = None
    process = kernel.OpenProcess(0x1000, False, pid.value)
    if process:
        try:
            name = ctypes.create_unicode_buffer(32768)
            length = wintypes.DWORD(len(name))
            if kernel.QueryFullProcessImageNameW(process, 0, name, ctypes.byref(length)):
                process_name = Path(name.value).name
        finally:
            kernel.CloseHandle(process)
    desktop_name = None
    desktop = user.OpenInputDesktop(0, False, 0x0001)
    desktop_error = None
    if desktop:
        try:
            name = ctypes.create_unicode_buffer(256)
            needed = wintypes.DWORD()
            if user.GetUserObjectInformationW(desktop, 2, name, ctypes.sizeof(name), ctypes.byref(needed)):
                desktop_name = name.value
            else:
                desktop_error = ctypes.get_last_error()
        finally:
            user.CloseDesktop(desktop)
    else:
        desktop_error = ctypes.get_last_error()
    display_active = None
    try:
        display_active = run(["nvidia-smi", "--query-gpu=display_active", "--format=csv,noheader"], 5).decode().strip()
    except Exception:
        pass
    reasons = []
    if (process_name or "").lower() in {"lockapp.exe", "logonui.exe"}:
        reasons.append("foreground_is_lock_or_logon_ui")
    if desktop_name != "Default":
        reasons.append("interactive_default_desktop_unavailable")
    if process_name is None and class_name.value == "Windows.UI.Core.CoreWindow":
        reasons.append("corewindow_foreground_process_could_not_be_verified")
    if display_active and all(line.strip().lower() == "disabled" for line in display_active.splitlines()):
        reasons.append("capture_display_inactive")
    return {"status": "blocked_external_state" if reasons else "available", "reasons": reasons,
            "foreground_process": process_name, "foreground_class": class_name.value,
            "foreground_pid": pid.value, "input_desktop": desktop_name,
            "input_desktop_error": desktop_error, "display_active": display_active,
            "read_only": True, "screenshots_taken": False}


@contextlib.contextmanager
def matrix_display_awake(enabled, output):
    """Keep an already available display awake across all live child captures.

    No monitor wake, power-plan change, input event or unlock is performed.
    Validation-only calls never enter a Windows power API.
    """
    if not enabled or os.name != "nt":
        yield
        return
    import ctypes
    from ctypes import wintypes
    owner = ctypes.WinDLL("kernel32", use_last_error=True)
    owner.SetThreadExecutionState.argtypes = [wintypes.DWORD]
    owner.SetThreadExecutionState.restype = wintypes.DWORD
    owner.GetCurrentThreadId.argtypes = []
    owner.GetCurrentThreadId.restype = wintypes.DWORD
    thread_id = owner.GetCurrentThreadId()
    previous = owner.SetThreadExecutionState(0x80000000 | 0x00000001 | 0x00000002)
    if not previous:
        raise OSError(ctypes.get_last_error(), "Could not hold the available display awake")
    state = {"owner_thread_id": thread_id, "previous_execution_state": previous,
             "monitor_wake_performed": False,
             "scope": "One parent-thread keep-awake request across all live children; no wake, unlock, input or power-plan change"}
    try:
        yield
    finally:
        assert owner.GetCurrentThreadId() == thread_id, "Power guard moved off its owner thread"
        state["request_restored"] = bool(owner.SetThreadExecutionState(0x80000000))
        (output / "display-power-context.json").write_text(json.dumps(state, indent=2), encoding="utf-8")


def run(command, timeout):
    result = subprocess.run(command, cwd=ROOT, capture_output=True, timeout=timeout,
                            creationflags=HIDDEN)
    if result.returncode:
        raise RuntimeError(f"{Path(command[0]).name} exited {result.returncode}: "
                           + result.stderr.decode(errors="replace")[-4096:])
    return result.stdout


def packet_count(stream):
    value = stream.get("nb_read_packets", "0")
    return int(value) if value not in {"N/A", None} else 0


def source_evidence(path, seconds, rate):
    """Analyze existing decoded file pixels only; never reads the live desktop."""
    ffmpeg = os.environ.get("RCR_FFMPEG", "ffmpeg")
    interval = max(1, round(rate / 2))
    raw = run([ffmpeg, "-v", "error", "-threads", "2", "-i", str(path), "-map", "0:v:0", "-an",
               "-vf", f"select=not(mod(n\\,{interval})),scale=160:90:flags=area,format=gray",
               "-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1"], max(90, seconds * 8 + 30))
    size = 160 * 90
    assert raw and len(raw) % size == 0, "Source evidence decode is incomplete"
    frames = [raw[offset:offset + size] for offset in range(0, len(raw), size)]
    hashes = [hashlib.sha256(frame).hexdigest() for frame in frames]
    black = [sum(pixel <= 20 for pixel in frame) / size for frame in frames]
    differences = [sum(abs(left - right) for left, right in zip(before, after)) / size
                   for before, after in zip(frames, frames[1:])]
    return {"method": "Existing-file software decode, original first frame then ~2 frames/s at160x90 gray",
            "sampled_frames": len(frames), "first_frame_black_fraction": black[0],
            "maximum_black_fraction": max(black), "minimum_black_fraction": min(black),
            "all_sampled_frames_black": all(value >= 0.999 for value in black),
            "unique_sampled_frames": len(set(hashes)),
            "frozen_sampled_content": len(frames) >= 2 and len(set(hashes)) == 1,
            "mean_absolute_gray_frame_delta": sum(differences) / len(differences) if differences else 0.,
            "new_desktop_capture_performed": False}


def validate_clip(path, audio, profile=None, pipeline=None):
    ffprobe = os.environ.get("RCR_FFPROBE", "ffprobe")
    ffmpeg = os.environ.get("RCR_FFMPEG", "ffmpeg")
    probe = json.loads(run([ffprobe, "-v", "error", "-count_packets", "-show_streams",
                            "-show_format", "-show_packets", "-show_entries",
                            "packet=stream_index,pts_time,flags:stream:format", "-of", "json",
                            str(path)], 90))
    video = [stream for stream in probe["streams"] if stream["codec_type"] == "video"]
    tracks = [stream for stream in probe["streams"] if stream["codec_type"] == "audio"]
    assert len(video) == 1 and video[0]["codec_name"] == "av1", "Expected one AV1 video"
    assert len(tracks) == int(audio), "Audio track count changed"
    for stream in tracks:
        assert stream["codec_name"] == "pcm_f32le"
        assert int(stream["sample_rate"]) == 48000 and int(stream["channels"]) == 2
    if profile:
        assert video[0]["width"] == profile["width"] and video[0]["height"] == profile["height"]
        assert video[0]["r_frame_rate"] == f"{profile['fps']}/1"
    tags = probe["format"].get("tags", {})
    metadata = json.loads(next(value for key, value in tags.items() if key.upper() == "_RCR_METADATA"))
    assert all(track["source_type"] == "system" for track in metadata["audio_tracks"]), "Microphone/application audio unexpectedly present"
    count = packet_count(video[0])
    assert count > 0 and metadata["clip"]["video_frames"] == count, "Embedded/video packet counts disagree"
    if pipeline:
        assert pipeline["encoded"] == count and pipeline["submitted"] == count, "Recording lost encoded frames"
        assert pipeline["raw_cpu_readbacks"] == 0, "Unexpected raw capture readback"
        assert not pipeline["audio_errors"] and pipeline["error"] is None
    previous = {}
    first_video = True
    for packet in probe["packets"]:
        index = packet["stream_index"]
        timestamp = float(packet["pts_time"])
        assert timestamp >= previous.get(index, float("-inf")), "Packet PTS moved backwards"
        previous[index] = timestamp
        if index == video[0]["index"] and first_video:
            assert "K" in packet["flags"], "Clip does not begin with a keyframe"
            first_video = False
    seconds = float(probe["format"]["duration"])
    assert abs(seconds - metadata["clip"]["duration_ns"] / 1e9) < 0.02
    started = time.perf_counter()
    progress = run([ffmpeg, "-v", "error", "-threads", "2", "-i", str(path), "-map", "0",
                    "-fps_mode", "passthrough", "-progress", "pipe:1", "-nostats", "-f", "null", "-"],
                   max(90, seconds * 8 + 30)).decode(errors="replace")
    decoded = [int(line.split("=", 1)[1]) for line in progress.splitlines() if line.startswith("frame=")]
    assert decoded and decoded[-1] == count, "Full decoder frame count changed"
    rate_parts = video[0]["r_frame_rate"].split("/")
    evidence = source_evidence(path, seconds, int(rate_parts[0]) / int(rate_parts[1]))
    return {"path": str(path), "fully_decoded": True, "video_frames": count,
            "decoded_frames": decoded[-1], "seconds": seconds,
            "audio_tracks": len(tracks), "audio_packets": sum(packet_count(track) for track in tracks),
            "loopback_contains_packets": any(packet_count(track) for track in tracks),
            "monotonic_pts": True, "starts_on_keyframe": True,
            "full_decode_seconds": time.perf_counter() - started,
            "metadata": metadata, "source_evidence": evidence}


def validate_report(path, require_audio_packets=False):
    report = json.loads(path.read_text(encoding="utf-8"))
    errors = report.get("errors", []) or ([report["error"]] if report.get("error") else [])
    external_errors = [error for error in errors if "no desktop frames" in error.lower()
                       or ("owned" in error.lower() and "fixture" in error.lower() and "updates" in error.lower())]
    unexpected_errors = [error for error in errors if error not in external_errors]
    assert not unexpected_errors, "Recorder/encoder/audio failure is not an external-source blocker: " + str(unexpected_errors)
    context_path = path.parent / "harness-context.json"
    context = json.loads(context_path.read_text(encoding="utf-8")) if context_path.exists() else {}
    if path.name == "desktop-report.json":
        clips = [validate_clip(Path(row["path"]), row["desktop_loopback"], pipeline=row["pipeline"])
                 for row in report["rows"]]
    else:
        assert report["microphone"] is False
        if report["files"]:
            assert report["terminal_event_seen"], "Published media lacks the recorder terminal event"
            assert report["stop_requested_ms"] is not None, "Published media lacks the independent Stop request"
        clips = []
        for item in report["files"]:
            clip = Path(item["path"])
            stats = report["pipeline"] if report["scenario"] != "replay-on-stop" and clip.name == "recording.mkv" else None
            clips.append(validate_clip(clip, report["desktop_loopback"], report["requested_video"], stats))
    audio_clips = [clip for clip in clips if clip["audio_tracks"]]
    require_packets = bool(context.get("require_audio_packets") or require_audio_packets)
    if require_packets:
        assert all(clip["audio_packets"] > 0 for clip in audio_clips), "Owned silent render did not produce captured loopback packets"
    blockers = []
    limited = []
    for clip in clips:
        evidence = clip["source_evidence"]
        if evidence["all_sampled_frames_black"]:
            blockers.append("decoded_source_is_black; normal_desktop_capture_not_proven")
        if evidence["frozen_sampled_content"]:
            if report.get("owned_animated_fixture"):
                blockers.append("owned_animated_fixture_not_visible_in_recorded_content")
            else:
                limited.append("decoded_source_is_frozen; active_content_capture_not_proven")
    if not clips:
        if any("no desktop frames" in error.lower() for error in errors):
            blockers.append("no_initial_capture_frame; start_and_stop_were_not_measured")
        else:
            raise AssertionError("No published clip to validate: " + str(errors))
    if any("owned" in error.lower() and "updates" in error.lower() for error in errors):
        blockers.append("active_source_updates_not_observed")
    status = "blocked_external_state" if blockers else ("limited_pipeline_only" if limited else "validated")
    if errors and not blockers:
        raise AssertionError(str(errors))
    return {"report": str(path), "validated": status == "validated", "status": status,
            "blockers": sorted(set(blockers)), "limitations": limited,
            "pipeline_stream_validation_passed": bool(clips),
            "audio_packets_required": require_packets,
            "partial_measurements": {key: report.get(key) for key in
                                     ["stop_to_worker_return_ms", "stop_to_worker_join_ms", "phase_after_stop_ms", "rows"]},
            "errors": errors, "clips": clips}


def start_silent_render(seconds):
    """Own a finite zero-PCM output stream; do not change device volume/settings."""
    executable = os.environ.get("RCR_FFPLAY", "ffplay")
    process = subprocess.Popen([executable, "-v", "error", "-nodisp", "-autoexit", "-f", "lavfi",
                                "-i", "anullsrc=r=48000:cl=stereo", "-t", str(seconds + 15)],
                               cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.PIPE, creationflags=HIDDEN)
    time.sleep(0.5)
    if process.poll() is not None:
        error = process.communicate(timeout=2)[1].decode(errors="replace")
        raise RuntimeError("Could not start the owned silent audio render: " + error[-4096:])
    return process


def stop_owned_render(process):
    if process.poll() is None:
        process.terminate()
    try:
        _, stderr = process.communicate(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        _, stderr = process.communicate(timeout=3)
    return stderr


def require_available_desktop(output, cases, case=None):
    state = desktop_availability_preflight()
    state["case"] = case
    (output / "availability-preflight.json").write_text(json.dumps(state, indent=2), encoding="utf-8")
    with (output / "availability-preflights.jsonl").open("a", encoding="utf-8") as log:
        log.write(json.dumps(state) + "\n")
    if state["status"] != "available":
        partial = []
        for path in output.rglob("worker-report.json"):
            report = json.loads(path.read_text(encoding="utf-8"))
            partial.append({"report": str(path), "stop_to_worker_return_ms": report.get("stop_to_worker_return_ms"),
                            "note": "Partial raw measurement only; source-content validation still required"})
        summary = {"success": False, "status": "blocked_external_state", "availability": state,
                   "capture_cases": cases, "partial_measurements": partial,
                   "capture_started_after_failed_preflight": False, "monitor_wake_performed": False}
        (output / "hardware-evaluation.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
        print(json.dumps(summary, indent=2), flush=True)
        raise SystemExit(2)
    return state


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/debug/examples/hardware-stop-eval.exe")
    parser.add_argument("--seconds", default="6", help="Comma-separated real capture durations, e.g. 6,60")
    parser.add_argument("--audio", default="none,system")
    parser.add_argument("--scenarios", default="continuous,replay-on-stop,record-and-replay,replay-save-near-stop")
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--validate-only", action="store_true")
    parser.add_argument("--preflight-only", action="store_true", help="Read availability and exit; no capture, renderer, wake, unlock or power request")
    parser.add_argument("--static-desktop", action="store_true", help="Disable the small owned animated marker; diagnose no-initial-frame conditions")
    parser.add_argument("--render-silence", action="store_true", help="Own a finite inaudible ffplay render stream for system cases; require captured WASAPI packets")
    args = parser.parse_args()
    durations = [float(value) for value in args.seconds.split(",")]
    sources = args.audio.split(",")
    scenarios = args.scenarios.split(",")
    assert all(1 <= value <= 120 for value in durations)
    assert set(sources) <= {"none", "system"} and 1 <= args.repeats <= 10
    assert set(scenarios) <= {"continuous", "replay-on-stop", "record-and-replay", "replay-save-near-stop"}
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    cases = []
    if args.preflight_only:
        print(json.dumps(require_available_desktop(output, cases), indent=2))
        return
    if not args.validate_only:
        require_available_desktop(output, cases)
        with matrix_display_awake(True, output):
            assert args.exe.is_file(), f"Build the hardware example first: {args.exe}"
            for repeat in range(args.repeats):
                for seconds in durations:
                    for audio in sources:
                        for scenario in scenarios:
                            case = output / f"{scenario}-{audio}-{seconds:g}s-run{repeat}"
                            assert not case.exists(), f"Refusing to overwrite existing hardware result: {case}"
                            require_available_desktop(output, cases, case.name)
                            render = None
                            render_stderr = b""
                            result = None
                            case_error = None
                            timeout_stdout = b""
                            timeout_stderr = b""
                            try:
                                if args.render_silence and audio == "system":
                                    render = start_silent_render(seconds)
                                result = subprocess.run([str(args.exe.resolve()), "--seconds", str(seconds), "--audio", audio,
                                                         "--scenario", scenario, "--animate-fixture", str(not args.static_desktop).lower(), "--output", str(case)],
                                                        cwd=ROOT, capture_output=True, timeout=seconds + 90, creationflags=HIDDEN)
                            except Exception as error:
                                case_error = str(error)
                                if isinstance(error, subprocess.TimeoutExpired):
                                    timeout_stdout = error.stdout or b""
                                    timeout_stderr = error.stderr or b""
                            finally:
                                if render is not None:
                                    render_stderr = stop_owned_render(render)
                            case.mkdir(parents=True, exist_ok=True)
                            (case / "harness-context.json").write_text(json.dumps({
                                "render_silence": render is not None,
                                "require_audio_packets": render is not None,
                                "silent_render_child_reaped": render is None or render.poll() is not None,
                                "silent_render_exit_code": None if render is None else render.returncode,
                            }, indent=2), encoding="utf-8")
                            if render is not None:
                                (case / "silence-render.stderr.log").write_bytes(render_stderr)
                            (case / "stdout.log").write_bytes(result.stdout if result else timeout_stdout)
                            (case / "stderr.log").write_bytes(result.stderr if result else timeout_stderr)
                            row = {"case": case.name, "exit_code": result.returncode if result else -1,
                                   "error": case_error}
                            cases.append(row)
                            print(json.dumps(row), flush=True)
    validations = []
    failures = []
    reports = sorted(output.rglob("worker-report.json")) + sorted(output.rglob("desktop-report.json"))
    for report in reports:
        try:
            validation = validate_report(report, args.render_silence)
            validations.append(validation)
            print(f"{validation['status']}: {report}", flush=True)
        except Exception as error:
            failures.append({"report": str(report), "error": str(error)})
            print(f"FAILED validation: {report}: {error}", flush=True)
    summary = {"scope": "Actual local DXGI/NVENC/WASAPI worker and/or separate hidden Desktop proof. No microphone or uploads.",
               "capture_cases": cases, "validations": validations, "failures": failures,
               "blocked_external_state": [item for item in validations if item["status"] == "blocked_external_state"],
               "success": bool(validations) and not failures and all(item["validated"] for item in validations)
                          and all(case["exit_code"] == 0 for case in cases)}
    (output / "hardware-evaluation.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    if not summary["success"]:
        raise SystemExit("Hardware evaluation has failures; preserved reports/logs/clips remain local.")


if __name__ == "__main__":
    main()
