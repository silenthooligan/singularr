#!/usr/bin/env bash
# Start Singularr for a few seconds and fetch its pages and routes.
#
# It proves what the tests cannot: that the built binary starts, applies the migrations,
# answers searches from Meilisearch and from the built-in search when Meilisearch stops (with an
# image),
# hands out the web client at its clean addresses, and answers the API. The sync is switched off:
# no engine is asked. It runs `singularrd health`. With an image it waits for Docker to report
# the container healthy. With an image it also starts the
# same image as Singularr Requests (SINGULARR_FACE=requests) on the role of
# deploy/requests-role.sql. The full process creates that role at its start. Its password reaches
# the database as a SCRAM verifier: no statement the database logs holds it. The check then sees
# that the portal serves its pages, knows no owner route and
# refuses to start with an engine key. Everything it starts is stopped and removed when the
# script ends.
#
#   SMOKE_IMAGE=<image> tools/smoke.sh
#       the image as built: a throwaway PostgreSQL container and the image on a network of
#       their own, the requests sent from a curl container in the app container's network
#       (the image holds no curl of its own). The image job of CI runs this.
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
  # The image: the requests go to 127.0.0.1 inside the app container's network, so the edge
  # proxy the app trusts is 127.0.0.1 and no port is published on the host. curl runs in a
  # container of its own that shares that network: the image needs no curl.
  PG_IMAGE="${SINGULARR_TEST_PG_IMAGE:-postgres:18-alpine}"
  CURL_IMAGE="${SMOKE_CURL_IMAGE:-curlimages/curl:8.23.0@sha256:3fb7e13e5d179a845c97d78204f3f1064d1907037f26a3ebf91710571151fd93}"
  # The Meilisearch of the public compose file, the default search.
  MEILI_IMAGE="${SMOKE_MEILI_IMAGE:-getmeili/meilisearch:v1.58.0@sha256:cee0d2e31191594ff8e1651eb144aabd95b389a0bc3532327ecfb9ed91dffe7e}"
  ID="singularr-smoke-$$-$(head -c 3 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  PASS="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  MEILI_KEY="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  # The portal role's password: the full process creates the role with it (requests::role).
  FACE_PASS="$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  APP="$ID-app"
  FACE="$ID-face"
  cleanup() {
    docker rm -f "$ID-curl" "$ID-face-curl" "$APP" "$FACE" "$ID-pg" "$ID-meili" >/dev/null 2>&1 || true
    docker network rm "$ID" >/dev/null 2>&1 || true
  }
  # A curl beside a container, in its network: `docker exec <name> curl ...` reaches its 127.0.0.1.
  curl_beside() { # name of the curl container, the container whose network it shares
    local said
    if ! said="$(docker run -d --name "$1" --network "container:$2" --entrypoint sleep \
      "$CURL_IMAGE" 1800 2>&1)"; then
      echo "the curl container $1 did not start: $said" >&2
      exit 2
    fi
  }
  trap cleanup EXIT
  docker network create "$ID" >/dev/null
  # `timeout` inside the container is the backstop: it ends the server after 30 minutes. The
  # server logs every DDL statement: the portal role's password must not be in one.
  docker run -d --name "$ID-pg" --network "$ID" --tmpfs /var/lib/postgresql \
    -e POSTGRES_USER=singularr -e POSTGRES_PASSWORD="$PASS" -e POSTGRES_DB=singularr \
    "$PG_IMAGE" timeout 1800 docker-entrypoint.sh postgres -c log_statement=ddl >/dev/null
  ready=0
  for _ in $(seq 1 120); do
    # The entrypoint starts a temporary server first; ask over TCP, which only the real one serves.
    if docker exec "$ID-pg" pg_isready -h 127.0.0.1 -U singularr -q 2>/dev/null; then ready=1; break; fi
    sleep 0.5
  done
  [ "$ready" = 1 ] || { echo "PostgreSQL did not come up in its container" >&2; exit 2; }
  docker run -d --name "$ID-meili" --network "$ID" --tmpfs /meili_data \
    -e MEILI_MASTER_KEY="$MEILI_KEY" -e MEILI_ENV=production -e MEILI_NO_ANALYTICS=true \
    "$MEILI_IMAGE" >/dev/null
  ready=0
  for _ in $(seq 1 120); do
    if docker exec "$ID-meili" curl -fsS -o /dev/null http://127.0.0.1:7700/health 2>/dev/null; then ready=1; break; fi
    sleep 0.5
  done
  [ "$ready" = 1 ] || { echo "Meilisearch did not come up in its container" >&2; exit 2; }
  # The image's health check runs every 2 s here. Docker then reports the state within the smoke
  # run.
  docker run -d --name "$APP" --network "$ID" --health-interval=2s \
    -e DATABASE_URL="postgres://singularr:$PASS@$ID-pg:5432/singularr" \
    -e SINGULARR_REQUESTS_DB_PASSWORD="$FACE_PASS" \
    -e SINGULARR_OWNER_EMAILS=owner@example.com -e SINGULARR_TRUSTED_PROXY_IPS=127.0.0.1 \
    -e SINGULARR_EDGE_TOKEN="$TOKEN" -e SINGULARR_SYNC_ENABLED=false -e RUST_LOG=info \
    -e RADARR_URL=http://127.0.0.1:9 -e SONARR_URL=http://127.0.0.1:9 \
    -e SINGULARR_SEARCH_URL="http://$ID-meili:7700" -e SINGULARR_SEARCH_KEY="$MEILI_KEY" \
    "$SMOKE_IMAGE" >/dev/null
  curl_beside "$ID-curl" "$APP"
  EXEC=(docker exec "$ID-curl")
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

# curl where the server is: here, or beside the app container in its network.
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

# The media tools (image mode): the doctor finds ffmpeg and ffprobe at /usr/local/bin. Its other
# checks fail here (no engine answers). Only these two lines count.
if [ -n "${SMOKE_IMAGE:-}" ]; then
  doctor_out="$(docker exec "$APP" /app/singularrd doctor 2>&1 || true)"
  for tool in ffmpeg ffprobe; do
    line="$(grep -E "^OK +$tool: " <<<"$doctor_out" || true)"
    if [ -n "$line" ]; then
      echo "ok    doctor, $tool: ${line#*: }"
    else
      echo "FAIL  doctor, $tool: $(grep -E "^[A-Z]+ +$tool: " <<<"$doctor_out" || echo "no line")"; fail=1
    fi
  done
fi

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

# The health check: `singularrd health` asks /readyz at the listen address. It says ready. It
# fails where nothing listens. A proxy in its environment does not carry the call. With an image,
# Docker runs it and reports the container healthy.
health_of() { # container: Docker's health status, or nothing
  docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$1" 2>/dev/null || true
}
wait_healthy() { # container: wait up to 60 s for Docker to report it healthy
  local status=""
  for _ in $(seq 1 60); do
    status="$(health_of "$1")"
    [ "$status" = healthy ] && break
    sleep 1
  done
  printf '%s' "$status"
}
# The proxy is an address where nothing listens: a call it carried would fail.
PROXY_ENV=(HTTP_PROXY=http://127.0.0.1:9 http_proxy=http://127.0.0.1:9 ALL_PROXY=http://127.0.0.1:9)
if [ -n "${SMOKE_IMAGE:-}" ]; then
  health_run=(docker exec "$APP" /app/singularrd health)
  proxied_run=(docker exec "${PROXY_ENV[@]/#/--env=}" "$APP" /app/singularrd health)
  nowhere_run=(docker exec -e SINGULARR_LISTEN=127.0.0.1:9 "$APP" /app/singularrd health)
else
  health_run=(env SINGULARR_LISTEN="127.0.0.1:$PORT" "$BIN" health)
  proxied_run=(env "${PROXY_ENV[@]}" SINGULARR_LISTEN="127.0.0.1:$PORT" "$BIN" health)
  nowhere_run=(env SINGULARR_LISTEN=127.0.0.1:9 "$BIN" health)
fi
if said="$("${health_run[@]}" 2>&1)" && [ "$said" = ready ]; then
  echo "ok    singularrd health"
else
  echo "FAIL  singularrd health: $said"; fail=1
fi
if said="$("${proxied_run[@]}" 2>&1)" && [ "$said" = ready ]; then
  echo "ok    singularrd health with a proxy in its environment"
else
  echo "FAIL  singularrd health with a proxy in its environment: $said"; fail=1
fi
if "${nowhere_run[@]}" >/dev/null 2>&1; then
  echo "FAIL  singularrd health passed with nothing listening"; fail=1
else
  echo "ok    singularrd health fails with nothing listening"
fi
if [ -n "${SMOKE_IMAGE:-}" ]; then
  state="$(wait_healthy "$APP")"
  if [ "$state" = healthy ]; then
    echo "ok    Docker reports the container healthy"
  else
    echo "FAIL  Docker reports the container's health as ${state:-none}"; fail=1
  fi
fi

# Search (image mode): Meilisearch answers once the first index pass has written it, a few seconds
# after the start. Stopped, the next search falls back to the built-in one. The status then
# reports provider postgresql, state warn.
if [ -n "${SMOKE_IMAGE:-}" ]; then
  search_status() { req -sS "${OWNER[@]}" "$BASE/api/v1/search/status" 2>/dev/null || true; }
  is_status() { # status text, provider, state
    grep -qF "\"provider\":\"$2\"" <<<"$1" && grep -qF "\"state\":\"$3\"" <<<"$1"
  }
  st=""
  for _ in $(seq 1 60); do
    st="$(search_status)"
    is_status "$st" meilisearch ok && break
    sleep 1
  done
  if is_status "$st" meilisearch ok; then
    echo "ok    search status with Meilisearch: meilisearch, ok"
  else
    echo "FAIL  search status with Meilisearch: $st"; fail=1
  fi
  docker stop "$ID-meili" >/dev/null
  check "a search with Meilisearch stopped" 200 '"query":"matrix"' "${OWNER[@]}" "$BASE/api/v1/search?q=matrix&kind=album"
  st="$(search_status)"
  if is_status "$st" postgresql warn; then
    echo "ok    search status with Meilisearch stopped: postgresql, warn"
  else
    echo "FAIL  search status with Meilisearch stopped: $st"; fail=1
  fi
fi

# Singularr Requests, the same image as the public face (image mode only).
if [ -n "${SMOKE_IMAGE:-}" ]; then
  FACE_SECRET="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  # The role the file creates (an installation may keep an older name). The full process
  # created it at its start with FACE_PASS. The face signs in with that password over the
  # network, where the server asks for SCRAM-SHA-256.
  FACE_ROLE="$(sed -n 's/.*CREATE ROLE \([a-z_]*\).*/\1/p' "$ROOT/deploy/requests-role.sql" | head -1)"
  app_log="$(docker logs "$APP" 2>&1 || true)"
  if grep -qF -- "$FACE_PASS" <<<"$app_log"; then
    echo "FAIL  the full process's log holds the portal role's password"; fail=1
  elif grep -qF "the portal role was created" <<<"$app_log"; then
    echo "ok    the full process created the portal role"
  else
    echo "FAIL  the full process did not create the portal role"; fail=1
  fi
  stored="$(docker exec "$ID-pg" psql -tAq -U singularr -d singularr \
    -c "SELECT rolpassword LIKE 'SCRAM-SHA-256\$4096:%' FROM pg_authid WHERE rolname = '$FACE_ROLE'" 2>&1 || true)"
  if [ "$stored" = t ]; then
    echo "ok    the portal role's password is a SCRAM-SHA-256 verifier"
  else
    echo "FAIL  the portal role's password is no SCRAM-SHA-256 verifier: $stored"; fail=1
  fi
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
  docker run -d --name "$FACE" --network "$ID" --health-interval=2s "${face_env[@]}" "$SMOKE_IMAGE" >/dev/null
  curl_beside "$ID-face-curl" "$FACE"
  face_up=0
  for _ in $(seq 1 60); do
    if docker exec "$ID-face-curl" curl -fsS -o /dev/null http://127.0.0.1:3000/healthz 2>/dev/null; then face_up=1; break; fi
    [ "$(docker inspect -f '{{.State.Running}}' "$FACE" 2>/dev/null)" = true ] || break
    sleep 0.5
  done
  if [ "$face_up" != 1 ]; then
    echo "FAIL  the request face did not come up; its log:"; docker logs "$FACE" 2>&1 | tail -20; fail=1
  else
    face_check() { # name, expected status, needle or "", curl args...
      local name="$1" want="$2" needle="$3"; shift 3
      local body status
      body="$(docker exec "$ID-face-curl" curl -sS -o - -w '\n%{http_code}' "$@" || true)"
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
    face_state="$(wait_healthy "$FACE")"
    if [ "$face_state" = healthy ]; then
      echo "ok    face Docker reports the container healthy"
    else
      echo "FAIL  face Docker reports the container's health as ${face_state:-none}"; fail=1
    fi
    face_log="$(docker logs "$FACE" 2>&1 || true)"
    if grep -qF -- "$FACE_PASS" <<<"$face_log" || grep -qF -- "$FACE_SECRET" <<<"$face_log"; then
      echo "FAIL  the request face's log holds a secret"; fail=1
    else
      echo "ok    the request face's log holds no secret"
    fi
  fi
  # The database's log holds every DDL statement. The role's ALTER ROLE is there with the
  # verifier. The password is in none.
  pg_log="$(docker logs "$ID-pg" 2>&1 || true)"
  if grep -qF -- "$FACE_PASS" <<<"$pg_log"; then
    echo "FAIL  the database's log holds the portal role's password"; fail=1
  elif ! grep -qF "PASSWORD 'SCRAM-SHA-256\$" <<<"$pg_log"; then
    echo "FAIL  the database's log holds no ALTER ROLE with a verifier"; fail=1
  else
    echo "ok    the portal role's password is in no logged statement"
  fi
fi

# The startup printout must hold no secret: the token of this run is one.
save_log
if grep -qF -- "$TOKEN" "$LOG"; then
  echo "FAIL  the log contains the edge token"; fail=1
elif [ -n "${MEILI_KEY:-}" ] && grep -qF -- "$MEILI_KEY" "$LOG"; then
  echo "FAIL  the log contains the search key"; fail=1
else
  echo "ok    the log holds no secret"
fi

if [ "$fail" != 0 ]; then
  echo "smoke: failed; the server log:"
  tail -40 "$LOG"
  exit 1
fi
echo "smoke: every check passed"
