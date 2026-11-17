#!/usr/bin/env bash
# Live smoke test against real providers.
#
# This host cannot reach openrouter.ai or api.sailresearch.com (blocked
# network), so this script is meant to be run from a machine that can.
#
#   OPENROUTER_API_KEY=... SAIL_API_KEY=... scripts/live-smoke.sh
#
# Optional: GATEWAY_URL (default http://127.0.0.1:8081),
#           ADMIN_EMAIL (default admin@example.com),
#           ADMIN_PASSWORD (required from env or prompt),
#           OPENROUTER_BASE (default https://openrouter.ai/api/v1),
#           SAIL_BASE (default https://api.sailresearch.com/v1),
#           SAIL_MODEL (a model id; discovery prints candidates).
#
# The keys are read from the environment only; nothing is written to disk.
set -euo pipefail

GATEWAY_URL="${GATEWAY_URL:-http://127.0.0.1:8081}"
ADMIN_EMAIL="${ADMIN_EMAIL:-admin@example.com}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-}"
OPENROUTER_BASE="${OPENROUTER_BASE:-https://openrouter.ai/api/v1}"
SAIL_BASE="${SAIL_BASE:-https://api.sailresearch.com/v1}"

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "missing dependency: $1"; }
need curl
need jq

[[ -n "${ADMIN_PASSWORD}" ]] || die "set ADMIN_PASSWORD"
[[ -n "${OPENROUTER_API_KEY:-}" ]] || die "set OPENROUTER_API_KEY"
[[ -n "${SAIL_API_KEY:-}" ]] || die "set SAIL_API_KEY"

jar="$(mktemp)"
trap 'rm -f "$jar"' EXIT

api() { # method path [json-body]
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    curl -sf -b "$jar" -c "$jar" -X "$method" "$GATEWAY_URL$path" \
      -H 'content-type: application/json' -H "x-csrf-token: $CSRF" -d "$body"
  else
    curl -sf -b "$jar" -c "$jar" -X "$method" "$GATEWAY_URL$path"
  fi
}

step() { printf '\n== %s ==\n' "$*"; }

step "login"
login="$(curl -sf -c "$jar" -X POST "$GATEWAY_URL/api/auth/login" \
  -H 'content-type: application/json' \
  -d "$(jq -nc --arg e "$ADMIN_EMAIL" --arg p "$ADMIN_PASSWORD" '{email:$e,password:$p}')")" \
  || die "login failed (wrong password, or the admin still needs a password change)"
CSRF="$(jq -r .csrf_token <<<"$login")"
[[ "$(jq -r '.user.must_change_password' <<<"$login")" == "false" ]] \
  || die "admin must change the password first (do it in the UI, then re-run)"
echo "ok: $(jq -r '.user.email + " (" + .user.role + ")"' <<<"$login")"

provider_id() { # name base_url
  local name="$1" base="$2" id
  id="$(curl -sf -b "$jar" "$GATEWAY_URL/api/providers" \
    | jq -r --arg n "$name" '.[] | select(.name==$n) | .id' | head -1)"
  if [[ -z "$id" ]]; then
    id="$(api POST /api/providers "$(jq -nc --arg n "$name" --arg b "$base" '{name:$n,base_url:$b}')" | jq -r .id)"
    echo "created provider $name ($id)" >&2
  fi
  printf '%s' "$id"
}

credential_id() { # provider_id label secret
  local pid="$1" label="$2" secret="$3" id
  id="$(curl -sf -b "$jar" "$GATEWAY_URL/api/providers/$pid/credentials" \
    | jq -r --arg l "$label" '.[] | select(.label==$l) | .id' | head -1)"
  if [[ -z "$id" ]]; then
    id="$(api POST "/api/providers/$pid/credentials" \
      "$(jq -nc --arg l "$label" --arg s "$secret" '{label:$l,secret:$s}')" | jq -r .id)"
    echo "created credential $label ($id)" >&2
  fi
  printf '%s' "$id"
}

check_provider() { # name base key
  local name="$1" base="$2" key="$3" pid cid preview
  step "$name: discovery"
  pid="$(provider_id "$name" "$base")"
  cid="$(credential_id "$pid" live "$key")"
  preview="$(api POST "/api/providers/$pid/discover-models" \
    "$(jq -nc --arg c "$cid" '{credential_id:$c,dry_run:true}')")" \
    || die "$name: discovery request failed (network or credential)"
  jq -c '{discovered,created_models,created_offerings,unchanged}' <<<"$preview"
  api POST "/api/providers/$pid/discover-models" \
    "$(jq -nc --arg c "$cid" '{credential_id:$c,dry_run:false}')" >/dev/null
  printf '%s %s\n' "$pid" "$cid"
}

read -r OR_PID OR_CID < <(check_provider openrouter "$OPENROUTER_BASE" "$OPENROUTER_API_KEY")
read -r SAIL_PID SAIL_CID < <(check_provider sailresearch "$SAIL_BASE" "$SAIL_API_KEY")

step "pick models"
model_of() { # provider_id
  curl -sf -b "$jar" "$GATEWAY_URL/api/models/overview?provider=$1" \
    | jq -r '.rows[] | select(.enabled==true) | .canonical_name' | head -3
}
echo "openrouter candidates:"; model_of "$OR_PID" | sed 's/^/  - /'
echo "sailresearch candidates:"; model_of "$SAIL_PID" | sed 's/^/  - /'

OR_MODEL="${OR_MODEL:-$(model_of "$OR_PID" | head -1)}"
SAIL_MODEL="${SAIL_MODEL:-$(model_of "$SAIL_PID" | head -1)}"
[[ -n "$OR_MODEL" ]] || die "no openrouter model discovered"
[[ -n "$SAIL_MODEL" ]] || die "no sailresearch model discovered (set SAIL_MODEL)"

chat() { # provider-id model stream(1|0)
  local pid="$1" model="$2" stream="$3" body
  body="$(jq -nc --arg m "$model" --arg pid "$pid" --argjson s "$stream" \
    '{model:$m,stream:$s,max_tokens:64,messages:[{role:"user",content:"Reply with the single word: pong"}]}' 2>/dev/null || true)"
  # Route by provider by using combo-free explicit model ids the gateway knows.
  if [[ "$stream" == "1" ]]; then
    curl -sN -b "$jar" -D /tmp/live-headers.txt -X POST "$GATEWAY_URL/v1/chat/completions" \
      -H 'content-type: application/json' -d "$body" | head -40
  else
    curl -s -b "$jar" -D /tmp/live-headers.txt -X POST "$GATEWAY_URL/v1/chat/completions" \
      -H 'content-type: application/json' -d "$body" | jq -c '{model, usage, choices: [.choices[]?.message.content]}'
  fi
}

step "chat: $OR_MODEL (non-streaming)"
chat "$OR_PID" "$OR_MODEL" 0
step "chat: $SAIL_MODEL (streaming)"
chat "$SAIL_PID" "$SAIL_MODEL" 1

step "usage rows (last 5)"
curl -sf -b "$jar" "$GATEWAY_URL/api/logs?limit=5" \
  | jq -c '.entries[] | {requested_model, provider, status, error_class, cost_usd}' 2>/dev/null \
  || curl -sf -b "$jar" "$GATEWAY_URL/api/logs?limit=5" | head -c 800

step "done"
echo "Note: revoke/rotate any key that was pasted into a chat or a ticket."
