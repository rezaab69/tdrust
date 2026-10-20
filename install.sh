#!/usr/bin/env bash
# Installs tdrust on a fresh Ubuntu or Debian server (x86_64):
# system packages, Rust, TDLib 1.8.67 from PyPI, the tdrust source from git,
# a release build, and a check that TDLib loads.
#
# Usage (as root):
#   sudo ./install.sh
#   sudo ./install.sh --dir /srv/tdrust --branch main
#
# Options:
#   --repo URL        Git repository to clone (default: https://github.com/rezaab69/tdrust.git)
#   --branch NAME     Branch to check out (default: main)
#   --dir PATH        Where the source goes (default: /opt/tdrust)
#   --tdlib-dir PATH  Where libtdjson goes (default: /opt/tdlib)
#   --force-tdlib     Download TDLib again even if it is already installed
#   --swap-file PATH  Swap file to create if memory is short (default: /var/tdrust.swap)
#   --keep-swap       Keep the swap file after the build and enable it at boot
#   --no-swap         Never create a swap file, even if memory is short
#   -h, --help        Show this help
#
# Compiling needs about 6 GB of memory. If RAM plus existing swap is below
# that, a swap file is created for the missing amount and removed again when
# the script ends (unless --keep-swap is given).
#
# Safe to run again: existing packages, TDLib and the git checkout are reused
# or updated.
set -euo pipefail

REPO_URL="https://github.com/rezaab69/tdrust.git"
BRANCH="main"
SRC_DIR="/opt/tdrust"
TDLIB_DIR="/opt/tdlib"
TDLIB_VERSION="1.8.67"
FORCE_TDLIB=0

# Memory the build needs (RAM + swap), in MB.
REQUIRED_MEM_MB=6144
SWAP_FILE="/var/tdrust.swap"
USE_SWAP=1
KEEP_SWAP=0
SWAP_CREATED=0

WORK="$(mktemp -d)"

cleanup() {
    rm -rf "$WORK"
    if [[ "$SWAP_CREATED" -eq 1 && "$KEEP_SWAP" -eq 0 ]]; then
        log "Removing the temporary swap file $SWAP_FILE"
        swapoff "$SWAP_FILE" 2>/dev/null || true
        rm -f "$SWAP_FILE"
    fi
}
trap cleanup EXIT

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Usage: sudo ./install.sh [options]

  --repo URL        Git repository to clone (default: https://github.com/rezaab69/tdrust.git)
  --branch NAME     Branch to check out (default: main)
  --dir PATH        Where the source goes (default: /opt/tdrust)
  --tdlib-dir PATH  Where libtdjson goes (default: /opt/tdlib)
  --force-tdlib     Download TDLib again even if it is already installed
  --swap-file PATH  Swap file to create if memory is short (default: /var/tdrust.swap)
  --keep-swap       Keep the swap file after the build and enable it at boot
  --no-swap         Never create a swap file, even if memory is short
  -h, --help        Show this help

Compiling needs about 6 GB of memory (RAM + swap). If there is less, a swap
file is created for the missing amount and removed when the script ends.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo) REPO_URL="${2:?--repo needs a value}"; shift 2 ;;
        --branch) BRANCH="${2:?--branch needs a value}"; shift 2 ;;
        --dir) SRC_DIR="${2:?--dir needs a value}"; shift 2 ;;
        --tdlib-dir) TDLIB_DIR="${2:?--tdlib-dir needs a value}"; shift 2 ;;
        --force-tdlib) FORCE_TDLIB=1; shift ;;
        --swap-file) SWAP_FILE="${2:?--swap-file needs a value}"; shift 2 ;;
        --keep-swap) KEEP_SWAP=1; shift ;;
        --no-swap) USE_SWAP=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

# ---- Preflight -------------------------------------------------------------

[[ "$(uname -s)" == "Linux" ]] || die "this script only runs on Linux"
[[ "$EUID" -eq 0 ]] || die "run as root: sudo $0"
command -v apt-get >/dev/null 2>&1 || die "apt-get not found; this script supports Ubuntu and Debian"

if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    case "${ID:-}" in
        ubuntu|debian) ;;
        *) log "warning: ${ID:-unknown} is not tested; continuing" ;;
    esac
fi

# ---- Memory ----------------------------------------------------------------

# Prints a /proc/meminfo value (for example MemTotal) in MB.
mem_mb() { awk -v key="$1:" '$1 == key { print int($2 / 1024) }' /proc/meminfo; }

# make_swap METHOD SIZE_MB: creates and enables $SWAP_FILE. METHOD is
# "fallocate" (fast) or "dd" (works on file systems where fallocate does not).
make_swap() {
    rm -f "$SWAP_FILE"
    if [[ "$1" == "fallocate" ]]; then
        (umask 077; fallocate -l "${2}M" "$SWAP_FILE")
    else
        (umask 077; dd if=/dev/zero of="$SWAP_FILE" bs=1M count="$2" status=none)
    fi && chmod 600 "$SWAP_FILE" && mkswap "$SWAP_FILE" >/dev/null && swapon "$SWAP_FILE"
}

# Makes sure RAM + swap reaches REQUIRED_MEM_MB, by creating a swap file for
# the missing amount. Problems with swap are warnings: the build may still fit.
ensure_memory() {
    local ram swap need free
    ram="$(mem_mb MemTotal)"
    swap="$(mem_mb SwapTotal)"

    if (( ram + swap >= REQUIRED_MEM_MB )); then
        log "Memory: ${ram} MB RAM + ${swap} MB swap is enough (the build needs ${REQUIRED_MEM_MB} MB)"
        return
    fi
    if [[ "$USE_SWAP" -eq 0 ]]; then
        log "warning: ${ram} MB RAM + ${swap} MB swap is below the ${REQUIRED_MEM_MB} MB the build needs; it may be killed (--no-swap given)"
        return
    fi
    if ! command -v mkswap >/dev/null 2>&1 || ! command -v swapon >/dev/null 2>&1; then
        log "warning: mkswap/swapon not found, cannot add swap; the build may run out of memory"
        return
    fi

    if grep -qs "^$SWAP_FILE " /proc/swaps; then
        die "$SWAP_FILE is already in use but memory is still short (${ram} MB RAM + ${swap} MB swap). Run 'swapoff $SWAP_FILE; rm $SWAP_FILE' and rerun, or pass another --swap-file."
    fi
    rm -f "$SWAP_FILE"

    need=$(( REQUIRED_MEM_MB - ram - swap ))
    (( need >= 1024 )) || need=1024
    install -d "$(dirname "$SWAP_FILE")"
    free="$(df -Pm "$(dirname "$SWAP_FILE")" | awk 'NR == 2 { print $4 }')"
    if (( free < need + 1024 )); then
        die "need ${need} MB of swap in $(dirname "$SWAP_FILE") but only ${free} MB is free (1 GB must stay spare). Free disk space, pass --swap-file on a larger disk, or pass --no-swap."
    fi

    log "Only ${ram} MB RAM + ${swap} MB swap: creating a ${need} MB swap file at $SWAP_FILE"
    if make_swap fallocate "$need" || make_swap dd "$need"; then
        SWAP_CREATED=1
        if [[ "$KEEP_SWAP" -eq 1 ]]; then
            grep -qsF "$SWAP_FILE" /etc/fstab || echo "$SWAP_FILE none swap sw 0 0" >> /etc/fstab
            log "Swap file kept and enabled at boot (remove it with: swapoff $SWAP_FILE; rm $SWAP_FILE; and delete its line in /etc/fstab)"
        fi
        log "Memory is now $(mem_mb MemTotal) MB RAM + $(mem_mb SwapTotal) MB swap"
    else
        rm -f "$SWAP_FILE"
        log "warning: could not enable a swap file here (a container or unsupported file system?); the build may run out of memory"
    fi
}

ensure_memory

# ---- System packages -------------------------------------------------------

log "Installing system packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y build-essential pkg-config curl unzip ca-certificates python3 git

# ---- Rust ------------------------------------------------------------------

if ! command -v cargo >/dev/null 2>&1 && [[ ! -x "$HOME/.cargo/bin/cargo" ]]; then
    log "Installing Rust with rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
log "Using $(cargo --version)"

# ---- TDLib -----------------------------------------------------------------

install_tdlib() {
    log "Downloading TDLib $TDLIB_VERSION from PyPI"
    local url
    url="$(curl -fsSL "https://pypi.org/pypi/tdjson/$TDLIB_VERSION/json" | python3 -c '
import json, sys
for f in json.load(sys.stdin)["urls"]:
    name = f["filename"]
    if name.endswith(".whl") and "manylinux" in name and "x86_64" in name:
        print(f["url"])
        break
')"
    [[ -n "$url" ]] || die "no x86_64 manylinux wheel found for tdjson $TDLIB_VERSION"

    curl -fL -o "$WORK/tdjson.whl" "$url"
    unzip -q "$WORK/tdjson.whl" 'tdjson.libs/*' -d "$WORK"

    # Copy the whole bundle: libtdjson needs the libraries packed beside it.
    install -d -m 755 "$TDLIB_DIR"
    cp -f "$WORK"/tdjson.libs/* "$TDLIB_DIR"/

    local lib
    lib="$(find "$TDLIB_DIR" -maxdepth 1 -name 'libtdjson-*.so*' | head -n 1)"
    [[ -n "$lib" ]] || die "libtdjson was not found in the wheel"
    ln -sfn "$(basename "$lib")" "$TDLIB_DIR/libtdjson.so"

    local missing
    missing="$(ldd "$TDLIB_DIR/libtdjson.so" | grep 'not found' || true)"
    [[ -z "$missing" ]] || die "libtdjson is missing libraries:"$'\n'"$missing"
}

if [[ -f "$TDLIB_DIR/libtdjson.so" && "$FORCE_TDLIB" -eq 0 ]]; then
    log "TDLib already installed at $TDLIB_DIR/libtdjson.so (use --force-tdlib to replace it)"
elif [[ "$(uname -m)" == "x86_64" ]]; then
    install_tdlib
else
    die "no prebuilt TDLib for $(uname -m). Build TDLib from source (https://tdlib.github.io/td/build.html), copy libtdjson.so to $TDLIB_DIR/libtdjson.so, and rerun."
fi
TDJSON_PATH="$TDLIB_DIR/libtdjson.so"

# ---- Source ----------------------------------------------------------------

if [[ -d "$SRC_DIR/.git" ]]; then
    log "Updating $SRC_DIR to $BRANCH"
    git -C "$SRC_DIR" fetch origin "$BRANCH"
    git -C "$SRC_DIR" checkout "$BRANCH"
    git -C "$SRC_DIR" pull --ff-only origin "$BRANCH"
elif [[ -e "$SRC_DIR" && -n "$(ls -A "$SRC_DIR" 2>/dev/null)" ]]; then
    die "$SRC_DIR exists and is not a git checkout; choose another --dir"
else
    log "Cloning $REPO_URL ($BRANCH) into $SRC_DIR"
    install -d "$(dirname "$SRC_DIR")"
    git clone --branch "$BRANCH" "$REPO_URL" "$SRC_DIR"
fi

# ---- Build and check -------------------------------------------------------

cd "$SRC_DIR"
log "Building tdrust (the first build takes several minutes)"
cargo build --release -p tdrust --examples

log "Checking that TDLib loads"
TDJSON_PATH="$TDJSON_PATH" cargo run --release -q -p tdrust --example offline

# ---- Environment -----------------------------------------------------------

log "Setting TDJSON_PATH for login shells in /etc/profile.d/tdrust.sh"
cat > /etc/profile.d/tdrust.sh <<EOF
export TDJSON_PATH="$TDJSON_PATH"
EOF
chmod 644 /etc/profile.d/tdrust.sh

cat <<EOF

tdrust is installed.

  Source:          $SRC_DIR
  TDLib:           $TDJSON_PATH
  Examples:        $SRC_DIR/target/release/examples/

Next steps:
  - Load cargo and TDJSON_PATH into this shell (new login shells do it for you):
      source ~/.cargo/env && source /etc/profile.d/tdrust.sh
  - Use it from your own project:
      tdrust = { git = "$REPO_URL" }
  - Try the sample app: copy $SRC_DIR/examples/sample-app/.env.example to
    $SRC_DIR/examples/sample-app/.env, fill it in, then run
      cd $SRC_DIR/examples/sample-app && cargo run -- check
  - Update later by rerunning: sudo bash $SRC_DIR/install.sh --dir $SRC_DIR
EOF
