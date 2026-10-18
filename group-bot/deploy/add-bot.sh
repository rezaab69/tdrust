#!/bin/bash
# Adds a bot that shares the group bot's database and Redis.
#
#   sudo ./add-bot.sh <name>            # asks for the token (not echoed)
#   sudo ./add-bot.sh <name> <token>
#
# <name> is a short label such as "bot2". The bot runs as the systemd
# service group-bot@<name>; see its log with: journalctl -u group-bot@<name> -f
#
# Defaults suit WSL (root, ~/group-bot). On a VPS (see DEPLOY-VPS.md):
#   GROUP_BOT_HOME=/opt/group-bot GROUP_BOT_USER=groupbot \
#   GROUP_BOT_UNIT=/opt/group-bot/src/group-bot/deploy/vps/group-bot@.service ./add-bot.sh bot2
set -euo pipefail

name="${1:-}"
token="${2:-}"
if [[ ! "$name" =~ ^[a-z0-9_-]+$ ]]; then
    echo "usage: $0 <name> [token]   (name: lowercase letters, digits, - and _)" >&2
    exit 1
fi
if [[ -z "$token" ]]; then
    read -rsp "Bot token from @BotFather for $name: " token
    echo
fi
if [[ ! "$token" =~ ^[0-9]+:[A-Za-z0-9_-]+$ ]]; then
    echo "that does not look like a bot token (123456:ABC...)" >&2
    exit 1
fi

here="$(cd "$(dirname "$0")" && pwd)"
base="${GROUP_BOT_HOME:-$HOME/group-bot}"
unit="${GROUP_BOT_UNIT:-$here/group-bot@.service}"
owner="${GROUP_BOT_USER:-}"
if [[ ! -x "$base/group-bot" ]]; then
    echo "$base/group-bot is missing: build with 'cargo build -p group-bot --release' and copy the binary there" >&2
    exit 1
fi

# The template unit, installed (or updated) once.
if [[ ! -f /etc/systemd/system/group-bot@.service ]] || ! cmp -s "$unit" /etc/systemd/system/group-bot@.service; then
    install -m 644 "$unit" /etc/systemd/system/group-bot@.service
    systemctl daemon-reload
fi

install -d -m 700 "$base/bots/$name"
umask 077
cat > "$base/bots/$name.env" <<EOF
TELEGRAM_BOT_TOKEN=$token
TDLIB_DATA_DIR=$base/bots/$name/tdlib
EOF
if [[ -n "$owner" ]]; then
    chown -R "$owner:$owner" "$base/bots/$name" "$base/bots/$name.env"
fi

systemctl enable "group-bot@$name" >/dev/null 2>&1
systemctl restart "group-bot@$name"
echo "Started group-bot@$name; waiting for it to log in..."
sleep 15
systemctl is-active "group-bot@$name"
journalctl -u "group-bot@$name" -n 5 -o cat --no-pager
