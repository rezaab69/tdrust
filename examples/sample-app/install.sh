#!/usr/bin/env bash
# Quick installer for the tdrust sample app (Linux). Run it from a checkout of
# the repository, as your normal user or as root:
#
#   git clone https://github.com/rezaab69/tdrust.git
#   bash tdrust/examples/sample-app/install.sh
#
# It sets up everything the app needs and asks no questions. It uses sudo only
# for the steps that need root (installing packages, adding swap, installing
# the service), and then sudo may ask for your password:
#
#   1. build tools, curl, unzip and python3 (apt), only if missing
#   2. Rust (rustup, for your user), only if missing or too old
#   3. TDLib 1.8.67 (from PyPI into ~/tdlib), only if it cannot be found
#   4. swap space, if RAM + swap is below the 6 GB the build needs
#   5. the release build of the sample app
#   6. the .env settings file, with the TDLib path and your credentials
#   7. a setup check and, with --service, the "tdrust-bot" systemd service
#
# Telegram credentials cannot be invented. Pass them as environment variables
# (preferred, options show up in `ps`) or options:
#
#   TELEGRAM_API_ID=12345678 TELEGRAM_API_HASH=<hash> TELEGRAM_BOT_TOKEN=<token> \
#       bash install.sh --service
#
# Without them the install still finishes. In a terminal the app then asks for
# them once (and saves them); otherwise it tells you what is missing.
#
# Options:
#   --api-id ID        Telegram API id (or TELEGRAM_API_ID)
#   --api-hash HASH    Telegram API hash (or TELEGRAM_API_HASH)
#   --bot-token TOKEN  Bot token from @BotFather (or TELEGRAM_BOT_TOKEN)
#   --tdjson PATH      Use this libtdjson instead of searching or downloading
#   --service          Install and start the bot as the systemd service
#                      "tdrust-bot" (needs all three credentials)
#   --swap-file PATH   Swap file to create if memory is short (default: /var/tdrust.swap)
#   --keep-swap        Keep the swap file after the build and enable it at boot
#   --no-swap          Never create a swap file, even if memory is short
#   -h, --help         Show this help
#
# Safe to run again: existing tools, TDLib and settings are reused.
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$APP_DIR/target/release/sample-app"
ENV_FILE="$APP_DIR/.env"
TDLIB_VERSION="1.8.67"
MIN_RUST_MINOR=85   # rust-version 1.85 in Cargo.toml

API_ID="${TELEGRAM_API_ID:-}"
API_HASH="${TELEGRAM_API_HASH:-}"
BOT_TOKEN="${TELEGRAM_BOT_TOKEN:-}"
TDJSON_ARG=""
INSTALL_SERVICE=0

# Memory the build needs (RAM + swap), in MB.
REQUIRED_MEM_MB=6144
SWAP_FILE="/var/tdrust.swap"
USE_SWAP=1
KEEP_SWAP=0
SWAP_CREATED=0

# The placeholders in .env.example; they count as "not filled in".
PLACEHOLDER_API_ID="12345678"
PLACEHOLDER_API_HASH="0123456789abcdef0123456789abcdef"
PLACEHOLDER_BOT_TOKEN="123456789:AAAbbbCCCdddEEEfffGGGhhhIIIjjjKKKll"

WORK="$(mktemp -d)"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Runs a command as root: directly if we are root, else through sudo.
as_root() {
    if [[ "$EUID" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        die "root is needed for this step and sudo is not installed; run this script as root"
    fi
}

cleanup() {
    rm -rf "$WORK"
    if [[ "$SWAP_CREATED" -eq 1 && "$KEEP_SWAP" -eq 0 ]]; then
        log "Removing the temporary swap file $SWAP_FILE"
        as_root swapoff "$SWAP_FILE" 2>/dev/null || true
        as_root rm -f "$SWAP_FILE"
    fi
}
trap cleanup EXIT

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --api-id) API_ID="${2:?--api-id needs a value}"; shift 2 ;;
        --api-hash) API_HASH="${2:?--api-hash needs a value}"; shift 2 ;;
        --bot-token) BOT_TOKEN="${2:?--bot-token needs a value}"; shift 2 ;;
        --tdjson) TDJSON_ARG="${2:?--tdjson needs a value}"; shift 2 ;;
        --service) INSTALL_SERVICE=1; shift ;;
        --swap-file) SWAP_FILE="${2:?--swap-file needs a value}"; shift 2 ;;
        --keep-swap) KEEP_SWAP=1; shift ;;
        --no-swap) USE_SWAP=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

# ---- Preflight -------------------------------------------------------------

[[ "$(uname -s)" == "Linux" ]] || die "this script only runs on Linux"
[[ -f "$APP_DIR/Cargo.toml" && -f "$APP_DIR/../../tdrust/Cargo.toml" ]] \
    || die "run this script from a checkout of https://github.com/rezaab69/tdrust (examples/sample-app/install.sh)"

# Reject bad input now, before the long install.
[[ -z "$API_ID" || "$API_ID" =~ ^[0-9]+$ ]] || die "the API id must be a number"
[[ -z "$API_HASH" || "$API_HASH" =~ ^[0-9a-fA-F]{32}$ ]] || die "the API hash must be 32 hex characters"
[[ -z "$BOT_TOKEN" || "$BOT_TOKEN" =~ ^[0-9]+:[A-Za-z0-9_-]+$ ]] || die "the bot token looks wrong (expected 123456789:AAA...)"
if [[ "$INSTALL_SERVICE" -eq 1 ]]; then
    [[ -d /run/systemd/system ]] || die "--service needs systemd, which is not running here"
fi

# The service (and the app) run as the user who started the install.
RUN_USER="${SUDO_USER:-$(id -un)}"
id "$RUN_USER" >/dev/null 2>&1 || RUN_USER="$(id -un)"
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a GIT_TERMINAL_PROMPT=0

# ---- Memory ----------------------------------------------------------------

# Prints a /proc/meminfo value (for example MemTotal) in MB.
mem_mb() { awk -v key="$1:" '$1 == key { print int($2 / 1024) }' /proc/meminfo; }

# make_swap METHOD SIZE_MB: creates and enables $SWAP_FILE. METHOD is
# "fallocate" (fast) or "dd" (works on file systems where fallocate does not).
make_swap() {
    as_root rm -f "$SWAP_FILE"
    if [[ "$1" == "fallocate" ]]; then
        as_root sh -c "umask 077; fallocate -l ${2}M '$SWAP_FILE'"
    else
        as_root sh -c "umask 077; dd if=/dev/zero of='$SWAP_FILE' bs=1M count=$2 status=none"
    fi && as_root chmod 600 "$SWAP_FILE" && as_root mkswap "$SWAP_FILE" >/dev/null && as_root swapon "$SWAP_FILE"
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
        die "$SWAP_FILE is already in use but memory is still short (${ram} MB RAM + ${swap} MB swap). Run 'sudo swapoff $SWAP_FILE; sudo rm $SWAP_FILE' and rerun, or pass another --swap-file."
    fi
    as_root rm -f "$SWAP_FILE"

    need=$(( REQUIRED_MEM_MB - ram - swap ))
    (( need >= 1024 )) || need=1024
    as_root install -d "$(dirname "$SWAP_FILE")"
    free="$(df -Pm "$(dirname "$SWAP_FILE")" | awk 'NR == 2 { print $4 }')"
    if (( free < need + 1024 )); then
        die "need ${need} MB of swap in $(dirname "$SWAP_FILE") but only ${free} MB is free (1 GB must stay spare). Free disk space, pass --swap-file on a larger disk, or pass --no-swap."
    fi

    log "Only ${ram} MB RAM + ${swap} MB swap: creating a ${need} MB swap file at $SWAP_FILE"
    if make_swap fallocate "$need" || make_swap dd "$need"; then
        SWAP_CREATED=1
        if [[ "$KEEP_SWAP" -eq 1 ]]; then
            grep -qsF "$SWAP_FILE" /etc/fstab || echo "$SWAP_FILE none swap sw 0 0" | as_root tee -a /etc/fstab >/dev/null
            log "Swap file kept and enabled at boot (remove it with: sudo swapoff $SWAP_FILE; sudo rm $SWAP_FILE; and delete its line in /etc/fstab)"
        fi
        log "Memory is now $(mem_mb MemTotal) MB RAM + $(mem_mb SwapTotal) MB swap"
    else
        as_root rm -f "$SWAP_FILE"
        log "warning: could not enable a swap file here (a container or unsupported file system?); the build may run out of memory"
    fi
}

# ---- Tools -----------------------------------------------------------------

missing_tools=()
for tool in cc curl unzip python3; do
    command -v "$tool" >/dev/null 2>&1 || missing_tools+=("$tool")
done
if (( ${#missing_tools[@]} > 0 )); then
    command -v apt-get >/dev/null 2>&1 \
        || die "missing: ${missing_tools[*]}. Install them with your package manager and rerun."
    log "Installing missing tools (${missing_tools[*]}) with apt"
    as_root apt-get update -y
    as_root apt-get install -y -o Dpkg::Options::=--force-confold \
        build-essential pkg-config curl unzip ca-certificates python3
fi

# ---- Rust ------------------------------------------------------------------

# shellcheck disable=SC1091
[[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"

rust_ok() {
    command -v cargo >/dev/null 2>&1 || return 1
    local ver major minor
    ver="$(cargo --version | awk '{ print $2 }')"
    IFS=. read -r major minor _ <<<"$ver"
    (( major > 1 || (major == 1 && minor >= MIN_RUST_MINOR) ))
}

if ! rust_ok; then
    if command -v rustup >/dev/null 2>&1; then
        log "Updating Rust (the sample app needs 1.$MIN_RUST_MINOR or newer)"
        rustup update stable
    else
        log "Installing Rust with rustup"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    fi
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    rust_ok || die "Rust 1.$MIN_RUST_MINOR or newer is needed, found: $(cargo --version 2>&1)"
fi
log "Using $(cargo --version)"

# ---- TDLib -----------------------------------------------------------------

# env_value FILE KEY: prints the value of KEY in an .env file (empty if unset).
env_value() { [[ -f "$1" ]] && sed -n "s/^$2=//p" "$1" | tail -n 1 || true; }

# set_env_value FILE KEY VALUE: sets KEY=VALUE in an .env file, keeping the
# other lines (the value is passed through the environment, so any character
# in it is safe).
set_env_value() {
    local tmp="$WORK/env.tmp"
    KEY="$2" VAL="$3" awk '
        BEGIN { k = ENVIRON["KEY"] "="; v = ENVIRON["VAL"] }
        index($0, k) == 1 { print k v; found = 1; next }
        { print }
        END { if (!found) print k v }' "$1" > "$tmp"
    cat "$tmp" > "$1"
}

# Prints the first libtdjson that exists: the option, TDJSON_PATH, the .env
# file, then the usual install locations. A leading ~ is expanded.
find_tdlib() {
    local candidate
    for candidate in "$TDJSON_ARG" "${TDJSON_PATH:-}" "$(env_value "$ENV_FILE" TDJSON_PATH)" \
        /opt/tdlib/libtdjson.so "$HOME/tdlib/libtdjson.so"; do
        candidate="${candidate/#\~/$HOME}"
        if [[ -n "$candidate" && -f "$candidate" ]]; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

download_tdlib() {
    local dir="$HOME/tdlib" url lib missing
    [[ "$(uname -m)" == "x86_64" ]] \
        || die "no prebuilt TDLib for $(uname -m). Build TDLib (https://tdlib.github.io/td/build.html) and pass its path with --tdjson."
    log "Downloading TDLib $TDLIB_VERSION from PyPI into $dir"
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
    install -d -m 755 "$dir"
    cp -f "$WORK"/tdjson.libs/* "$dir"/
    lib="$(find "$dir" -maxdepth 1 -name 'libtdjson-*.so*' | head -n 1)"
    [[ -n "$lib" ]] || die "libtdjson was not found in the wheel"
    ln -sfn "$(basename "$lib")" "$dir/libtdjson.so"

    missing="$(ldd "$dir/libtdjson.so" | grep 'not found' || true)"
    [[ -z "$missing" ]] || die "libtdjson is missing libraries:"$'\n'"$missing"
}

if ! TDJSON="$(find_tdlib)"; then
    download_tdlib
    TDJSON="$HOME/tdlib/libtdjson.so"
else
    log "Using TDLib at $TDJSON"
fi

# ---- Build -----------------------------------------------------------------

ensure_memory

log "Building the sample app (the first build takes several minutes)"
(cd "$APP_DIR" && cargo build --release)

# ---- Settings --------------------------------------------------------------

log "Writing $ENV_FILE"
[[ -f "$ENV_FILE" ]] || cp "$APP_DIR/.env.example" "$ENV_FILE"
chmod 600 "$ENV_FILE"
set_env_value "$ENV_FILE" TDJSON_PATH "$TDJSON"
[[ -z "$API_ID" ]] || set_env_value "$ENV_FILE" TELEGRAM_API_ID "$API_ID"
[[ -z "$API_HASH" ]] || set_env_value "$ENV_FILE" TELEGRAM_API_HASH "$API_HASH"
[[ -z "$BOT_TOKEN" ]] || set_env_value "$ENV_FILE" TELEGRAM_BOT_TOKEN "$BOT_TOKEN"

# credentials_state: sets API_READY (id and hash set) and BOT_READY (token set too).
credentials_state() {
    local id hash token
    id="$(env_value "$ENV_FILE" TELEGRAM_API_ID)"
    hash="$(env_value "$ENV_FILE" TELEGRAM_API_HASH)"
    token="$(env_value "$ENV_FILE" TELEGRAM_BOT_TOKEN)"
    API_READY=0
    BOT_READY=0
    if [[ -n "$id" && "$id" != "$PLACEHOLDER_API_ID" && -n "$hash" && "$hash" != "$PLACEHOLDER_API_HASH" ]]; then
        API_READY=1
        if [[ -n "$token" && "$token" != "$PLACEHOLDER_BOT_TOKEN" ]]; then
            BOT_READY=1
        fi
    fi
}
credentials_state

# ---- Check -----------------------------------------------------------------

if [[ "$API_READY" -eq 1 ]]; then
    log "Checking the setup"
    # No terminal input: the check reports problems instead of asking.
    "$BIN" check </dev/null
elif [[ -t 0 && -t 1 ]]; then
    log "Your Telegram credentials are missing: the app asks for them now and saves them"
    "$BIN" check || true
    credentials_state
else
    log "Your Telegram credentials are not set yet (see the end of this output)"
fi

# ---- Service ---------------------------------------------------------------

SERVICE_STARTED=0
if [[ "$INSTALL_SERVICE" -eq 1 ]]; then
    if [[ "$BOT_READY" -eq 1 ]]; then
        log "Installing the tdrust-bot service (runs as $RUN_USER)"
        as_root tee /etc/systemd/system/tdrust-bot.service >/dev/null <<EOF
[Unit]
Description=tdrust sample bot
After=network-online.target
Wants=network-online.target

[Service]
User=$RUN_USER
ExecStart=$BIN bot
Restart=on-failure
RestartSec=5
# The bot shuts down cleanly on Ctrl+C (SIGINT), not on systemd's default SIGTERM.
KillSignal=SIGINT

[Install]
WantedBy=multi-user.target
EOF
        as_root systemctl daemon-reload
        as_root systemctl enable tdrust-bot >/dev/null 2>&1
        as_root systemctl restart tdrust-bot
        SERVICE_STARTED=1
    else
        log "warning: --service skipped: the API id, API hash and bot token are not all set"
    fi
fi

# ---- Summary ---------------------------------------------------------------

cat <<EOF

The sample app is installed.

  App:       $APP_DIR
  Settings:  $ENV_FILE
  TDLib:     $TDJSON
  Binary:    $BIN

EOF

if [[ "$SERVICE_STARTED" -eq 1 ]]; then
    cat <<EOF
The bot is running as the "tdrust-bot" service and starts at boot.
  Status: systemctl status tdrust-bot
  Logs:   journalctl -u tdrust-bot -f
EOF
elif [[ "$BOT_READY" -eq 1 ]]; then
    cat <<EOF
Everything is set. Run the bot:
  $BIN bot
Run it permanently as a service:
  bash $APP_DIR/install.sh --service
EOF
else
    cat <<EOF
One thing is left: your Telegram credentials (API id and hash from
https://my.telegram.org, bot token from @BotFather). Either rerun this script
with TELEGRAM_API_ID, TELEGRAM_API_HASH and TELEGRAM_BOT_TOKEN set, or edit
$ENV_FILE, then run:
  $BIN check
EOF
fi

cat <<EOF

Other modes:
  $BIN user    # log in with your own account
  Guide:       $APP_DIR/README.md
EOF
