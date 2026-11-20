"""Recreate the media patterns with FFmpeg and GStreamer. Writer timestamps may vary."""
from pathlib import Path
import argparse
import subprocess


def generate(folder):
    folder.mkdir(exist_ok=True, parents=True)
    cases = [
        ("speech.wma", "wmav1", None, 44100, 1, 3200), ("stereo.wma", "wmav2", None, 44100, 2, 2048),
        ("lowrate.wma", "wmav2", None, 16000, 1, 512), ("pcm.asf", "pcm_s16le", None, 16000, 1, 1024),
        ("mp3.asf", "libmp3lame", None, 32000, 1, 2048), ("silent.wmv", None, "wmv1", 0, 0, 512),
        ("motion.wmv", None, "wmv2", 0, 0, 2048), ("music-video.wmv", "wmav2", "wmv2", 44100, 2, 3200),
        ("mpeg4.asf", "wmav2", "msmpeg4v3", 44100, 1, 1200),
    ]
    for i, (name, audio, video, rate, channels, packet) in enumerate(cases):
        cmd = ["ffmpeg", "-v", "error", "-y"]
        if video:
            cmd += ["-f", "lavfi", "-i", f"testsrc2=size={64 + (i % 2) * 16}x48:rate=8:duration=1.4"]
        if audio:
            cmd += ["-f", "lavfi", "-i", f"sine=frequency={280 + 29*i}:sample_rate={rate}:duration=1.4",
                    "-c:a", audio, "-ac", str(channels)]
            if audio != "pcm_s16le":
                cmd += ["-b:a", "32k" if rate <= 32000 else "64k"]
        if video:
            cmd += ["-c:v", video, "-g", "3", "-threads:v", "1"]
        subprocess.run([*cmd, "-metadata", f"title=復元試験 {name}", "-packet_size", str(packet),
                        "-f", "asf", folder / name], check=True)
    (folder / "unsupported").mkdir(exist_ok=True)
    for name, source, options, streams in [
        ("gst-audio.wma", "stereo.wma", ["packet-size=1536", "padding=256"], ["audio"]),
        ("gst-video.wmv", "motion.wmv", ["packet-size=4096", "padding=256"], ["video"]),
        ("gst-av.wmv", "music-video.wmv", ["packet-size=3500", "padding=256"], ["audio", "video"]),
        ("unsupported/broadcast.wma", "stereo.wma", ["streamable=true"], ["audio"]),
    ]:
        cmd = ["gst-launch-1.0", "-q", "filesrc", "location=" + str(folder / source), "!", "asfdemux", "name=d",
               "asfmux", "name=m", *options, "!", "filesink", "location=" + str(folder / name)]
        for stream in streams:
            cmd += [f"d.{stream}_0", "!", "queue", "!", f"m.{stream}_0"]
        subprocess.run(cmd, check=True, timeout=60)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    generate(parser.parse_args().output)
