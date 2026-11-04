#!/usr/bin/env bash
set -euo pipefail

# This task already has an isolated checkout; do not create another worktree.
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_HOME="${CARGO_HOME:-/workspace/.rescue-tools/cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-/workspace/.rescue-tools/rustup}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export PATH="$CARGO_HOME/bin:$PATH"

if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    [[ "$(uname -sm)" == "Linux x86_64" ]] || { echo "This bootstrap is for the Linux x64 cloud environment." >&2; exit 1; }
    setup_dir="$(mktemp -d /tmp/rescue-rustup.XXXXXX)"
    trap 'rm -rf -- "$setup_dir"' EXIT
    curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
        https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init -o "$setup_dir/rustup-init"
    curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
        https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init.sha256 -o "$setup_dir/rustup-init.sha256"
    python3 - "$setup_dir" <<'PY'
import hashlib, sys
from pathlib import Path
root = Path(sys.argv[1])
expected = (root/"rustup-init.sha256").read_text().split()[0]
actual = hashlib.sha256((root/"rustup-init").read_bytes()).hexdigest()
if expected != actual:
    raise SystemExit("rustup-init checksum mismatch")
PY
    chmod +x "$setup_dir/rustup-init"
    "$setup_dir/rustup-init" -y --no-modify-path --profile minimal --default-toolchain 1.99.1
fi

rustup toolchain install 1.99.1 --profile minimal --component rustfmt,clippy
rustup target add x86_64-pc-windows-msvc --toolchain 1.99.1
cd "$repo_root"
cargo fetch --locked
cargo build --workspace --locked --offline
cargo test --workspace --locked --offline
python3 scripts/e2e.py --binary target/debug/rescue
python3 scripts/e2e_streams.py --binary target/debug/rescue
python3 scripts/e2e_fat.py --binary target/debug/rescue

python3 scripts/e2e_carve.py --binary target/debug/rescue

python3 scripts/e2e_audit.py --binary target/debug/rescue
