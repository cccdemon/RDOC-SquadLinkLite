#!/usr/bin/env bash
# End-to-end room-key test against a REAL mesh: one InitConnection server and
# three headless engines in Docker, real signaling, real WebRTC, real PQC key
# hand-out. Reproduces the 0.2.1 field bug deterministically: the client with
# the SMALLEST user_id joins LAST, becomes key authority from an empty state,
# and must still converge with the room instead of minting a generation nobody
# accepts (the pre-0.2.1 failure: silent in both directions).
#
#   bash e2e/run.sh            # from the repo root
#
# Needs Docker (Desktop) with the Linux engine. ~5 min cold, seconds warm.
set -euo pipefail
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

DOCKER="${DOCKER:-docker}"
if ! command -v "$DOCKER" >/dev/null; then
  # Docker Desktop on Windows: the CLI and its credential helper live here but
  # may be missing from a Git-Bash PATH.
  export PATH="$PATH:/c/Program Files/Docker/Docker/resources/bin"
  DOCKER="docker.exe"
fi

IMG=subraum-e2e
ROOM=e2e-room
SECRET=e2e-secret-$(date +%s)
CLIENTS=(sb-m sb-t sb-a)   # user_ids: m-bravo, t-charlie join first; a-alpha last (smallest!)

cleanup() {
  "$DOCKER" rm -f sb-init "${CLIENTS[@]}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

echo "== build =="
"$DOCKER" build -q -t "$IMG" -f e2e/Dockerfile . >/dev/null

echo "== server =="
"$DOCKER" run -d --name sb-init \
  -e TLS_DISABLE=1 -e ROOM_AUTH_SECRET="$SECRET" \
  "$IMG" init-connection >/dev/null
sleep 2
TOKEN=$("$DOCKER" exec sb-init init-connection mint "$ROOM" | tr -d '[:space:]' | tail -c 64)
[ ${#TOKEN} -eq 64 ] || { echo "FAIL: could not mint a token"; exit 1; }

# All clients share the server's network namespace, so ws://127.0.0.1 satisfies
# the client's loopback-only-plain-ws rule and ICE runs inside one netns.
client() { # name user_id
  "$DOCKER" run -d --name "$1" --network container:sb-init \
    -e SERVER=ws://127.0.0.1:8080/ws -e ROOM="$ROOM" -e USER_ID="$2" -e NAME="$2" \
    -e TOKEN="$TOKEN" -e RELAY_DISABLED=1 \
    "$IMG" >/dev/null
}

# Wait until $1's log contains $2 (regex), up to $3 seconds.
wait_log() {
  for _ in $(seq 1 "$3"); do
    if "$DOCKER" logs "$1" 2>&1 | grep -qE "$2"; then return 0; fi
    sleep 1
  done
  echo "FAIL: $1 never logged /$2/ within $3 s — last lines:"
  "$DOCKER" logs --tail 15 "$1" 2>&1
  exit 1
}

echo "== phase 1: two-member room forms and gets a key =="
client sb-m m-bravo
client sb-t t-charlie
wait_log sb-m '\[room-audio gen[0-9]+' 60
wait_log sb-t '\[room-audio gen[0-9]+' 60
echo "== phase 1b: churn the room so its generation climbs past 1 =="
# At generation 1 even a pre-fix build converges: the newcomer's freshly minted
# gen1 wins the same-generation tie-break (smaller authority id). The field bug
# only bites once the room sits ABOVE the newcomer's starting point — so drive
# real rotations first: t leaving rotates (forward secrecy), t rejoining
# rotates again (fresh epoch for the joiner).
"$DOCKER" rm -f sb-t >/dev/null
wait_log sb-m '\[room-audio gen2 authority=true' 60
client sb-t t-charlie
wait_log sb-t '\[room-audio gen[3-9]' 60
wait_log sb-m '\[room-audio gen[3-9]' 60
GEN_BEFORE=$("$DOCKER" logs sb-m 2>&1 | grep -oE '\[room-audio gen[0-9]+' | grep -oE '[0-9]+' | tail -1)
echo "   room settled at generation $GEN_BEFORE"
[ "$GEN_BEFORE" -ge 2 ] || { echo "FAIL: churn did not raise the generation"; exit 1; }

echo "== phase 2: smallest id joins LAST and takes over the authority =="
client sb-a a-alpha
wait_log sb-a '\[room-audio gen[0-9]+ authority=true' 60
# The takeover must land ABOVE the room's settled generation — the pre-0.2.1
# bug was minting from an empty state (gen1), which sb-m/sb-t silently reject
# as stale, leaving the newcomer mute in both directions.
wait_log sb-m "\[room-audio gen$((GEN_BEFORE + 1))" 60
wait_log sb-t "\[room-audio gen$((GEN_BEFORE + 1))" 60

echo "== phase 3: all three see each other and agree on ONE generation =="
for c in "${CLIENTS[@]}"; do
  wait_log "$c" '\[Teilnehmer 3\]' 30
done
sleep 3 # let any trailing rotation settle on every node
GENS=""
for c in "${CLIENTS[@]}"; do
  g=$("$DOCKER" logs "$c" 2>&1 | grep -oE '\[room-audio gen[0-9]+' | grep -oE '[0-9]+' | tail -1)
  GENS="$GENS $c=gen$g"
done
set -- $GENS
G1=${1#*=}; G2=${2#*=}; G3=${3#*=}
if [ "$G1" != "$G2" ] || [ "$G2" != "$G3" ]; then
  echo "FAIL: generations diverged:$GENS"
  exit 1
fi

echo "== phase 4: nobody is stuck keyless =="
for c in "${CLIENTS[@]}"; do
  if "$DOCKER" logs "$c" 2>&1 | grep -q "Sprach-Schluessel nicht erhalten"; then
    echo "FAIL: $c raised the keyless warning"
    exit 1
  fi
done

echo "PASS:$GENS — authority handover to the late joiner converged the whole room"
