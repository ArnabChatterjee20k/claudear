#!/bin/sh
set -e

# If no API key is set, ensure Claude Code is logged in via OAuth
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "No ANTHROPIC_API_KEY set, checking Claude Code auth..."
    if ! claude auth status >/dev/null 2>&1; then
        if [ -t 0 ]; then
            echo "Not logged in. Starting Claude Code login..."
            echo "Open the URL below in your browser to authenticate:"
            echo ""
            claude auth login 2>&1
            echo ""
        else
            echo "ERROR: Not logged in and no TTY available for interactive OAuth login."
            echo "Please set ANTHROPIC_API_KEY or run with -it for interactive login."
            exit 1
        fi
    else
        echo "Claude Code already authenticated."
    fi
fi

exec "$@"
