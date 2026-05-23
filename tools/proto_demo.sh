#!/usr/bin/env bash
# Module-protocol smoke test. Run terminite first, then this script.
# Uses `socat` to speak to the Unix socket. `brew install socat` if missing.
#
# Usage:
#   ./proto_demo.sh tabs                       # list tabs
#   ./proto_demo.sh blocks [tab_id]            # list blocks for tab_id (default 0)
#   ./proto_demo.sh get [tab_id] [block_id]    # fetch one block's command + output
#   ./proto_demo.sh subscribe                  # stream block_opened / block_closed events
#                                              # (run blocks_demo.sh in another pane to see events)

set -euo pipefail

SOCKET="${TERMINITE_SOCKET:-$HOME/.terminite/socket}"

if ! command -v socat >/dev/null 2>&1; then
    echo "needs socat — brew install socat" >&2
    exit 1
fi
if [ ! -S "$SOCKET" ]; then
    echo "no socket at $SOCKET — is terminite running?" >&2
    exit 1
fi

cmd="${1:-tabs}"
shift || true

case "$cmd" in
    tabs)
        echo '{"id":1,"method":"list_tabs"}' | socat - UNIX-CONNECT:"$SOCKET"
        ;;
    blocks)
        tab_id="${1:-0}"
        printf '{"id":1,"method":"list_blocks","params":{"tab_id":%d}}\n' "$tab_id" \
            | socat - UNIX-CONNECT:"$SOCKET"
        ;;
    get)
        tab_id="${1:-0}"
        block_id="${2:-1}"
        printf '{"id":1,"method":"get_block","params":{"tab_id":%d,"block_id":%d}}\n' \
            "$tab_id" "$block_id" | socat - UNIX-CONNECT:"$SOCKET"
        ;;
    subscribe)
        # Send subscribe, then keep stdin open so socat doesn't close the
        # socket. Ctrl-C to exit.
        ( printf '{"id":1,"method":"subscribe"}\n'; cat ) \
            | socat - UNIX-CONNECT:"$SOCKET"
        ;;
    *)
        echo "usage: $0 {tabs | blocks [tab_id] | get [tab_id] [block_id] | subscribe}" >&2
        exit 1
        ;;
esac
