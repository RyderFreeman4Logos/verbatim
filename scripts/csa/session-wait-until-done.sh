#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: session-wait-until-done.sh <session-id> [--cd <path>]" >&2
  exit 2
fi

session_id="$1"
shift

wait_args=(--session "${session_id}")
if [ "$#" -gt 0 ]; then
  wait_args+=("$@")
fi

exec csa session wait "${wait_args[@]}"
