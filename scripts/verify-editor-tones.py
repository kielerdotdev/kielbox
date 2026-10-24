"""Verify the 440/880 Hz mux_fixture -> editor_fixture path, using stdlib only.

This checks synthetic fixtures, not arbitrary user recordings. It does not play
audio or open a window. FFmpeg/FFprobe must be installed (or use RCR_* overrides).
"""
import array
import json
import math
import os
from pathlib import Path
import subprocess
import sys


def run(program, *arguments):
    executable = os.environ.get(f"RCR_{program.upper()}", program)
    return subprocess.run(
        [executable, *map(str, arguments)],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout


def magnitude(samples, frequency):
    step = 2 * math.pi * frequency / 48000
    real = sum(value * math.cos(step * index) for index, value in enumerate(samples))
    imag = sum(value * math.sin(step * index) for index, value in enumerate(samples))
    return 2 * math.hypot(real, imag) / len(samples)


def main(path):
    metadata = json.loads(run("ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", path))
    video = [stream for stream in metadata["streams"] if stream["codec_type"] == "video"]
    audio = [stream for stream in metadata["streams"] if stream["codec_type"] == "audio"]
    assert len(video) == 1 and video[0]["codec_name"] == "av1", "AV1 video missing"
    assert len(audio) == 3, "Expected a mix and two independent audio stems"
    assert abs(float(metadata["format"]["duration"]) - 2) < 0.05, "Unexpected edit duration"
    run("ffmpeg", "-v", "error", "-threads", "2", "-i", path, "-map", "0", "-f", "null", "-")
    results = []
    for stream in audio:
        assert stream["codec_name"] == "aac", "Expected an AAC export track"
        raw = run("ffmpeg", "-v", "error", "-threads", "2", "-i", path, "-map", f"0:{stream['index']}",
                  "-vn", "-t", "2", "-ar", "48000", "-ac", "1", "-f", "f32le", "-")
        samples = array.array("f")
        samples.frombytes(raw)
        if sys.byteorder != "little":
            samples.byteswap()
        # One exact second away from the outer fades and AAC padding.
        samples = samples[12000:60000]
        assert len(samples) == 48000 and all(map(math.isfinite, samples)), "Incomplete/invalid decoded audio"
        a, b = magnitude(samples, 440), magnitude(samples, 880)
        name = stream.get("tags", {}).get("handler_name", stream.get("tags", {}).get("title", ""))
        results.append((name, a, b))
    assert results[0][0] == "Mix" and min(results[0][1:]) > 0.01, "Mix lacks one source"
    assert results[1][0] == "System audio" and results[1][1] > 0.01 and results[1][1] > results[1][2] * 100, "System stem leaked microphone audio"
    assert results[2][0] == "Microphone" and results[2][2] > 0.01 and results[2][2] > results[2][1] * 100, "Microphone stem leaked system audio"
    for name, a, b in results:
        print(f"{name}: 440 Hz={a:.6f}, 880 Hz={b:.6f}")
    print("Verified AV1 edit, complete decode, named mix/stems, and >40 dB synthetic source separation.")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python scripts/verify-editor-tones.py recordings/edited.mp4")
    main(Path(sys.argv[1]))
