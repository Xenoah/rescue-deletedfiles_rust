"""Create six checksummed archive-comment fixtures using RARLAB's RAR4/5 writers."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def capture(output, rar4, rar5, unrar):
    corpus = Path(__file__).resolve().parents[1] / "corpus/files"
    output.mkdir(parents=True, exist_ok=False)
    result = {}
    for version, writer in [(4, rar4), (5, rar5)]:
        for label, flags, names, comment in [
            ("stored", ["-m0"], ["sample.gif", "sample.tif"], b"Stored images and an empty file.\r\n"),
            ("solid", ["-m5", "-s"], ["sample.html", "sample.xml", "sample.txt"], b"Notes about the archive.\n" * 145),
            ("unicode", ["-m3"], ["sample.wav", "sample.docx"], "復元の検証\n".encode("utf-8")),
        ]:
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                files = root / ("試験" if label == "unicode" else "payload")
                files.mkdir()
                for name in names:
                    shutil.copyfile(corpus / name, files / name)
                (files / "empty.bin").touch()
                for p in [*files.iterdir(), files]:
                    os.utime(p, (1600000000, 1600000000))
                (root / "comment.txt").write_bytes(comment)
                filename = f"rar{version}-comment-{label}.rar"
                dest = output / filename
                subprocess.run([writer, "a", "-idq", f"-ma{version}", *flags, "-zcomment.txt", dest, files.name], cwd=root, check=True)
                subprocess.run([unrar, "t", "-inul", "-p-", dest], check=True)
                copied_comment = root / "decoded-comment.txt"
                subprocess.run([writer, "cw", "-idq", dest, copied_comment], check=True)
                assert copied_comment.read_bytes() == comment
                members = {}
                for p in files.iterdir():
                    path = p.relative_to(root).as_posix()
                    data = subprocess.check_output([unrar, "p", "-inul", "-p-", dest, path])
                    assert data == p.read_bytes()
                    members[path] = hashlib.sha256(data).hexdigest()
                result[filename] = dict(bytes=dest.stat().st_size, format=version, flags=" ".join(flags),
                                        members=members, sha256=hashlib.sha256(dest.read_bytes()).hexdigest(),
                                        comment_sha256=hashlib.sha256(comment).hexdigest())
    (output / "manifest.json").write_text(json.dumps(result, sort_keys=True, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ["output", "rar4", "rar5", "unrar"]:
        parser.add_argument("--" + arg, required=True, type=Path)
    args = parser.parse_args()
    capture(args.output.resolve(), args.rar4.resolve(), args.rar5.resolve(), args.unrar.resolve())
