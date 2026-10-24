"""Independently check the quick_clip_fixture's audio choices and volume."""
import array
import json
import math
from pathlib import Path
import subprocess
import sys


def run(*args):
    return subprocess.run(args, check=True, capture_output=True).stdout


def magnitude(samples, hz):
    step = math.tau * hz / 48000
    return 2 * abs(sum(value * complex(math.cos(i * step), math.sin(i * step))
                       for i, value in enumerate(samples))) / len(samples)


def main(prefix):
    for name, wanted, unwanted, expected in [("game", 440, 880, .1), ("mic", 880, 440, .2)]:
        path = f"{prefix}-{name}.mp4"
        meta = json.loads(run("ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", path))
        video = [s for s in meta["streams"] if s["codec_type"] == "video"]
        audio = [s for s in meta["streams"] if s["codec_type"] == "audio"]
        assert len(video) == len(audio) == 1
        assert (video[0]["width"], video[0]["height"]) == (1280, 720)
        assert abs(float(meta["format"]["duration"]) - 2) < .05
        run("ffmpeg", "-v", "error", "-threads", "2", "-i", path, "-map", "0", "-f", "null", "-")
        pcm = run("ffmpeg", "-v", "error", "-threads", "2", "-i", path, "-map", "0:a:0", "-ar", "48000", "-ac", "2", "-f", "f32le", "-")
        samples = array.array("f")
        samples.frombytes(pcm)
        if sys.byteorder != "little":
            samples.byteswap()
        samples = samples[::2]  # Measure one channel; automatic mono downmix adds gain.
        assert 96000 <= len(samples) <= 97024, "Decoded audio has a timestamp gap or missing samples"
        # Check both sides of the join, including the piece sped up to 2x.
        for start in [12000, 60000]:
            window = samples[start:start+24000]
            assert len(window) == 24000 and all(map(math.isfinite, window))
            a, b = magnitude(window, wanted), magnitude(window, unwanted)
            assert a > b * 100, f"{name}: muted audio leaked through"
            assert abs(a-expected) < .015, f"{name}: volume/speed mismatch: {a}"
        print(f"{name}: correct source and volume on both pieces, >40 dB rejection of muted source")
    print("Verified complete two-second exports, join, 2x pitch-preserving speed, global mute and volume.")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python scripts/verify-quick-clip.py recordings/quick-editor-proof-001")
    main(Path(sys.argv[1]))
