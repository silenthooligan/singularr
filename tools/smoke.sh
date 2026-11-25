#!/usr/bin/env bash
# Start Singularr for a few seconds and fetch its pages and routes.
#
# It proves what the tests cannot: that the built binary starts, applies the migrations,
# hands out the web client at its clean addresses, and answers the API. The sync is switched off, so no engine is asked. With an image it also starts the
# same image as Singularr Requests (SINGULARR_FACE=requests) on the role of
# deploy/requests-role.sql, and checks that it serves its pages, knows no owner route and
# refuses to start with an engine key. Everything it starts is stopped and removed when the
# script ends.
#
#   SMOKE_IMAGE=<image> tools/smoke.sh
#       the image as built: a throwaway PostgreSQL container and the image on a network of
#       their own, the requests sent from inside the app container. The image job of
#       CI runs this in its image job.
#   DATABASE_URL=postgres://... tools/smoke.sh
#       a local release build (cargo build --release -p singularr) against a throwaway database,
#       on a Linux box with cargo. Optional; the image job is the check of record.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$ROOT/target"
LOG="$ROOT/target/smoke.log"
TOKEN="$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
EXEC=()

if [ -n "${SMOKE_IMAGE:-}" ]; then
  # The image: the requests go to 127.0.0.1 inside the app container, so the edge proxy the
  # app trusts is 127.0.0.1 and no port is published on the host.
  PG_IMAGE="${SINGULARR_TEST_PG_IMAGE:-postgres:18-alpine}"
  ID="singularr-smoke-$$-$(head -c 3 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  PASS="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  APP="$ID-app"
  FACE="$ID-face"
  cleanup() {
    docker rm -f "$APP" "$FACE" "$ID-pg" >/dev/null 2>&1 || true
    docker network rm "$ID" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT
  docker network create "$ID" >/dev/null
  # `timeout` inside the container is the backstop: it ends the server after 30 minutes.
  docker run -d --name "$ID-pg" --network "$ID" --tmpfs /var/lib/postgresql \
    -e POSTGRES_USER=singularr -e POSTGRES_PASSWORD="$PASS" -e POSTGRES_DB=singularr \
    "$PG_IMAGE" timeout 1800 docker-entrypoint.sh postgres >/dev/null
  ready=0
  for _ in $(seq 1 120); do
    # The entrypoint starts a temporary server first; ask over TCP, which only the real one serves.
    if docker exec "$ID-pg" pg_isready -h 127.0.0.1 -U singularr -q 2>/dev/null; then ready=1; break; fi
    sleep 0.5
  done
  [ "$ready" = 1 ] || { echo "PostgreSQL did not come up in its container" >&2; exit 2; }
  docker run -d --name "$APP" --network "$ID" \
    -e DATABASE_URL="postgres://singularr:$PASS@$ID-pg:5432/singularr" \
    -e SINGULARR_OWNER_EMAILS=owner@example.com -e SINGULARR_TRUSTED_PROXY_IPS=127.0.0.1 \
    -e SINGULARR_EDGE_TOKEN="$TOKEN" -e SINGULARR_SYNC_ENABLED=false -e RUST_LOG=info \
    -e RADARR_URL=http://127.0.0.1:9 -e SONARR_URL=http://127.0.0.1:9 \
    "$SMOKE_IMAGE" >/dev/null
  EXEC=(docker exec "$APP")
  BASE="http://127.0.0.1:3000"
  alive() { [ "$(docker inspect -f '{{.State.Running}}' "$APP" 2>/dev/null)" = true ]; }
  save_log() { docker logs "$APP" >"$LOG" 2>&1 || true; }
else
  BIN="$ROOT/target/release/singularrd"
  PORT="${SMOKE_PORT:-37300}"
  BASE="http://127.0.0.1:$PORT"
  : "${DATABASE_URL:?set DATABASE_URL to a throwaway database, or SMOKE_IMAGE to an image}"
  [ -x "$BIN" ] || { echo "no $BIN: run cargo build --release -p singularr first" >&2; exit 2; }
  cd "$ROOT/target/release"
  env -i PATH="$PATH" HOME="$HOME" DATABASE_URL="$DATABASE_URL" \
    SINGULARR_LISTEN="127.0.0.1:$PORT" SINGULARR_WEB_DIR="$ROOT/web" \
    SINGULARR_POLICY="$ROOT/rules/policy.toml" SINGULARR_OWNER_EMAILS=owner@example.com \
    RADARR_URL=http://127.0.0.1:9 SONARR_URL=http://127.0.0.1:9 \
    SINGULARR_TRUSTED_PROXY_IPS=127.0.0.1 SINGULARR_EDGE_TOKEN="$TOKEN" SINGULARR_SYNC_ENABLED=false RUST_LOG=info \
    "$BIN" > "$LOG" 2>&1 &
  PID=$!
  trap 'kill "$PID" 2>/dev/null || true' EXIT
  alive() { kill -0 "$PID" 2>/dev/null; }
  save_log() { :; }
fi

# curl where the server is: here, or inside the app container.
req() { "${EXEC[@]}" curl "$@"; }

up=0
for _ in $(seq 1 60); do
  if req -fsS -o /dev/null "$BASE/healthz" 2>/dev/null; then up=1; break; fi
  alive || break
  sleep 0.5
done
if [ "$up" != 1 ]; then
  save_log
  echo "the server did not come up; its log:" >&2
  cat "$LOG" >&2
  exit 1
fi

fail=0
# The owner's session as the edge would send it.
OWNER=(-H "x-edge-token: $TOKEN" -H "x-auth-request-email: owner@example.com")
check() { # name, expected status, needle or "", curl args...
  local name="$1" want="$2" needle="$3"; shift 3
  local body status
  body="$(req -sS -o - -w '\n%{http_code}' "$@" || true)"
  status="${body##*$'\n'}"
  body="${body%$'\n'*}"
  if [ "$status" != "$want" ]; then
    echo "FAIL  $name: status $status, wanted $want"; fail=1; return
  fi
  if [ -n "$needle" ] && ! grep -qF -- "$needle" <<<"$body"; then
    echo "FAIL  $name: the answer does not contain: $needle"; fail=1; return
  fi
  echo "ok    $name"
}

check "healthz" 200 "ok" "$BASE/healthz"
check "readyz" 200 "ready" "$BASE/readyz"
check "metrics, engine gauge" 200 "singularr_engine_up" "$BASE/metrics"
check "metrics, http requests" 200 "axum_http_requests_total" "$BASE/metrics"
check "api health, no sign-in" 200 '"database":true' "$BASE/api/v1/health"
check "api titles, no sign-in" 401 "singularr.unauthorized" "$BASE/api/v1/titles"
check "identity header without the edge token" 401 "" -H "x-auth-request-email: owner@example.com" "$BASE/api/v1/whoami"
check "api whoami, owner" 200 '"kind":"owner"' "${OWNER[@]}" "$BASE/api/v1/whoami"
check "api overview" 200 '"sync_enabled":false' "${OWNER[@]}" "$BASE/api/v1/overview"
check "api openapi" 200 '"openapi":"3.1' "${OWNER[@]}" "$BASE/api/v1/openapi.json"
check "api openapi, title facts" 200 '"fanart_url"' "${OWNER[@]}" "$BASE/api/v1/openapi.json"
check "api titles, movies only" 200 '"total":0' "${OWNER[@]}" "$BASE/api/v1/titles?kind=movie"
# Radarr and Sonarr are listed without keys: two integrations, a library each, none asked.
check "api libraries" 200 '"total":2' "${OWNER[@]}" "$BASE/api/v1/libraries"
check "api integrations" 200 '"type":"radarr"' "${OWNER[@]}" "$BASE/api/v1/integrations"
check "api integrations, no sign-in" 401 "" "$BASE/api/v1/integrations"
# No Plex integration: nothing is playing and the server's music is empty, both answered.
check "api server sessions, no Plex" 200 '"configured":false' "${OWNER[@]}" "$BASE/api/v1/server/sessions"
check "api music from the server" 200 '"albums":[]' "${OWNER[@]}" "$BASE/api/v1/music"
check "api search needs words" 400 "" "${OWNER[@]}" "$BASE/api/v1/search?q="
check "api reference page" 200 "Singularr API reference" "$BASE/api/docs"
check "api parse" 200 '"quality":"Bluray-1080p"' "${OWNER[@]}" -H "content-type: application/json" \
  -d '{"name":"Movie.2004.1080p.BluRay.x265-QxR"}' "$BASE/api/v1/releases/parse"
check "event stream" 200 "event: connected" "${OWNER[@]}" --max-time 2 "$BASE/api/v1/events/stream"

# The interface: the web client's static files at clean addresses, for anyone. They hold no
# data: the API behind them asks for the sign-in.
check "client home" 200 "js/core.js" "$BASE/"
for page in discover movies shows music requests activity calendar rules lab journal settings title person collection; do
  check "client page $page" 200 "js/pages/$page.js" "$BASE/$page"
done
check "client page file moves to its clean address" 308 "" "$BASE/movies.html?sort=year"
check "client script" 200 "window.MQ" "$BASE/js/core.js"
check "client stylesheet" 200 "@layer" "$BASE/css/singularr.css"
check "client keeps its tools to itself" 404 "" "$BASE/tools/build_data.py"
check "client stays inside its folder" 404 "" --path-as-is "$BASE/../rules/policy.toml"
check "old title address moves" 302 "" "$BASE/movies/424242"
check "old lab address moves" 302 "" "$BASE/rules/lab?name=x"
check "summary" 200 '"requests_pending"' "${OWNER[@]}" "$BASE/api/v1/summary"
check "unknown page" 404 "js/pages/notfound.js" "$BASE/no-such-page"

# Singularr Requests, the same image as the public face (image mode only).
if [ -n "${SMOKE_IMAGE:-}" ]; then
  FACE_PASS="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  FACE_SECRET="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  # The role the file creates (an installation may keep an older name).
  FACE_ROLE="$(sed -n 's/.*CREATE ROLE \([a-z_]*\).*/\1/p' "$ROOT/deploy/requests-role.sql" | head -1)"
  docker exec -i "$ID-pg" psql -q -v ON_ERROR_STOP=1 -U singularr -d singularr \
    < "$ROOT/deploy/requests-role.sql" >/dev/null
  docker exec -i "$ID-pg" psql -q -v ON_ERROR_STOP=1 -U singularr -d singularr \
    -c "ALTER ROLE $FACE_ROLE PASSWORD '$FACE_PASS'" >/dev/null
  face_env=(-e SINGULARR_FACE=requests
    -e SINGULARR_REQUESTS_DATABASE_URL="postgres://$FACE_ROLE:$FACE_PASS@$ID-pg:5432/singularr"
    -e SINGULARR_REQUESTS_SESSION_SECRET="$FACE_SECRET"
    -e SINGULARR_REQUESTS_PUBLIC_URL=https://requests.example.org -e RUST_LOG=info)
  # With an engine key in its environment it must refuse to start.
  if timeout 60 docker run --rm --network "$ID" "${face_env[@]}" -e PLEXSEERR_API_KEY=a-key \
      "$SMOKE_IMAGE" >"$ROOT/target/smoke-face-refused.log" 2>&1; then
    echo "FAIL  the request face started with an engine key"; fail=1
  elif grep -qF "PLEXSEERR_API_KEY" "$ROOT/target/smoke-face-refused.log"; then
    echo "ok    the request face refuses an engine key"
  else
    echo "FAIL  the request face stopped without naming the engine key"; fail=1
  fi
  docker run -d --name "$FACE" --network "$ID" "${face_env[@]}" "$SMOKE_IMAGE" >/dev/null
  face_up=0
  for _ in $(seq 1 60); do
    if docker exec "$FACE" curl -fsS -o /dev/null http://127.0.0.1:3000/healthz 2>/dev/null; then face_up=1; break; fi
    [ "$(docker inspect -f '{{.State.Running}}' "$FACE" 2>/dev/null)" = true ] || break
    sleep 0.5
  done
  if [ "$face_up" != 1 ]; then
    echo "FAIL  the request face did not come up; its log:"; docker logs "$FACE" 2>&1 | tail -20; fail=1
  else
    face_check() { # name, expected status, needle or "", curl args...
      local name="$1" want="$2" needle="$3"; shift 3
      local body status
      body="$(docker exec "$FACE" curl -sS -o - -w '\n%{http_code}' "$@" || true)"
      status="${body##*$'\n'}"
      body="${body%$'\n'*}"
      if [ "$status" != "$want" ]; then
        echo "FAIL  face $name: status $status, wanted $want"; fail=1; return
      fi
      if [ -n "$needle" ] && ! grep -qF -- "$needle" <<<"$body"; then
        echo "FAIL  face $name: the answer does not contain: $needle"; fail=1; return
      fi
      echo "ok    face $name"
    }
    F="http://127.0.0.1:3000"
    face_check "healthz" 200 "ok" "$F/healthz"
    face_check "readyz, the views are readable" 200 "ready" "$F/readyz"
    face_check "discover, not signed in" 303 "" "$F/"
    face_check "sign-in page" 200 "Sign in with Plex" "$F/signin"
    face_check "stylesheet with the tokens" 200 "--mq-accent" "$F/assets/requests.css"
    face_check "no owner api, even as the owner" 404 "" "${OWNER[@]}" "$F/api/v1/whoami"
    face_check "no owner pages" 404 "" "${OWNER[@]}" "$F/settings"
    face_check "no metrics" 404 "" "$F/metrics"
    face_check "no api reference" 404 "" "$F/api/docs"
    face_check "no server functions" 404 "" -X POST "${OWNER[@]}" "$F/sfn/shell"
    face_check "4K is refused" 400 "HD only" -X POST -H "sec-fetch-site: same-origin" -d "is4k=true" "$F/movie/603/request"
    face_log="$(docker logs "$FACE" 2>&1 || true)"
    if grep -qF -- "$FACE_PASS" <<<"$face_log" || grep -qF -- "$FACE_SECRET" <<<"$face_log"; then
      echo "FAIL  the request face's log holds a secret"; fail=1
    else
      echo "ok    the request face's log holds no secret"
    fi
  fi
fi

# The startup printout must hold no secret: the token of this run is one.
save_log
if grep -qF -- "$TOKEN" "$LOG"; then
  echo "FAIL  the log contains the edge token"; fail=1
else
  echo "ok    the log holds no secret"
fi

if [ "$fail" != 0 ]; then
  echo "smoke: failed; the server log:"
  tail -40 "$LOG"
  exit 1
fi
echo "smoke: every check passed"
