#!/usr/bin/env bash
# End-to-end: desktop app -> OpsTrellis Cloud gateway (OpenAI-compatible) -> self-hosted model, plus
# device-code sign-in, metering and BYOK/local usage sync. Needs Postgres and the server's model.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SERVER=$HERE/../../server
WAV=/tmp/probe/sherpa-onnx-moonshine-tiny-en-int8/test_wavs/0.wav
export DATABASE_URL=${DATABASE_URL:-postgres://postgres:postgres@localhost:5432/opstrellis_e2e}
psql "${DATABASE_URL%/*}/postgres" -qc "DROP DATABASE IF EXISTS opstrellis_e2e WITH (FORCE)" -c "CREATE DATABASE opstrellis_e2e" >/dev/null
(cd "$SERVER" && npm run -s db:migrate >/dev/null)
(cd "$SERVER" && PORT=8787 PUBLIC_URL=http://localhost:8787 MASTER_KEY=e2e-master-key-for-the-test-run-only LOCAL_MODEL_DIR=models exec node dist/index.js >/tmp/server-e2e.log 2>&1) &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do curl -sf localhost:8787/api/catalog >/dev/null && break; sleep 0.2; done

J=$(mktemp)
H=(-H "Origin: http://localhost:8787" -H "Content-Type: application/json" -b "$J" -c "$J")
curl -sf "${H[@]}" localhost:8787/api/auth/signup -d '{"email":"e2e@example.com","password":"correct-horse-battery"}' >/dev/null
# Device flow exactly as the desktop performs it.
START=$(curl -sf -H "Content-Type: application/json" localhost:8787/api/device/start -d '{"device_name":"E2E desktop"}')
DEV=$(echo "$START" | python3 -c "import json,sys; print(json.load(sys.stdin)['device_code'])")
UC=$(echo "$START" | python3 -c "import json,sys; print(json.load(sys.stdin)['user_code'])")
[ "$(curl -s -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' localhost:8787/api/device/poll -d "{\"device_code\":\"$DEV\"}")" = 428 ]
curl -sf "${H[@]}" localhost:8787/api/device/approve -d "{\"user_code\":\"$UC\"}" >/dev/null
TOKEN=$(curl -sf -H "Content-Type: application/json" localhost:8787/api/device/poll -d "{\"device_code\":\"$DEV\"}" | python3 -c "import json,sys; print(json.load(sys.stdin)['token'])")
echo "device flow: token ${TOKEN:0:8}…"

me() { curl -sf -H "Authorization: Bearer $TOKEN" localhost:8787/api/me; }

echo "--- dictation through OpsTrellis Cloud gateway"
SETTINGS_JSON='{"onboarded":true,"language":"en","cloud":{"baseUrl":"http://localhost:8787","email":"e2e@example.com"},
 "sttChain":[{"id":"c","provider":"cloud","model":"local/moonshine-tiny-en","streaming":false,"enabled":true}]}' \
SECRETS_JSON="{\"cloud\":\"$TOKEN\"}" "$HERE/dictate_test.sh" "$WAV" "yellow lamps" | grep -E "inserted|release|PASS|FAIL"
me | python3 -c "import json,sys; u=json.load(sys.stdin)['usage']; print('metered managed minutes:', u['managed_minutes']); assert u['managed_minutes']>0"

echo "--- local dictation, usage metadata synced"
SETTINGS_JSON='{"onboarded":true,"language":"en","cloud":{"baseUrl":"http://localhost:8787"},"privacy":{"historyRetentionDays":null,"syncUsage":true},
 "sttChain":[{"id":"l","provider":"local","model":"moonshine-tiny-en","streaming":false,"enabled":true}]}' \
SECRETS_JSON="{\"cloud\":\"$TOKEN\"}" KEEP_LOG=/tmp/opstrellis-e2e-local.log LINGER=2 "$HERE/dictate_test.sh" "$WAV" "yellow lamps" | grep -E "inserted|PASS|FAIL"
sleep 1
curl -sf -H "Authorization: Bearer $TOKEN" "localhost:8787/api/stats" | python3 -c "
import json,sys; s=json.load(sys.stdin); print('server stats:', json.dumps(s.get('totals', s))[:300])
assert s['totals']['dictations'] == 2, 'local dictation not synced'
assert any(m.get('provider') == 'local' and m.get('mode', 'local') != 'cloud' for m in s['by_model'] if 'gateway' not in json.dumps(m)) or len(s['by_model']) >= 2, s['by_model']
print('PASS usage sync')"
