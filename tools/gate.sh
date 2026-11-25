#!/usr/bin/env bash
# The gate: formatting, clippy over the whole workspace with warnings as errors, and every test
# (the database-backed ones against a throwaway PostgreSQL). The `check` stage of the Dockerfile runs this script on the build runner;
# tools/ci-check.sh starts that from a PC.
#
#   docker buildx build --target check --output type=cacheonly .     the gate, in the image
#   tools/gate.sh                                                     on a Linux box with cargo
#
# PostgreSQL: when SINGULARR_TEST_DATABASE_URL is not set and the server binaries are found
# (SINGULARR_GATE_PG_BIN, default /usr/lib/postgresql/18/bin, which the check stage installs),
# a server is started for the length of the gate: its data in SINGULARR_GATE_PG_DIR (a tmpfs
# in the check stage), bound to 127.0.0.1 only, fsync off. Over its socket in that folder no
# password is asked. Over TCP every login needs a SCRAM-SHA-256 password, as on a real server:
# the portal role's tests sign in there. It is stopped when the script ends, however it ends.
# Without either, the database-backed tests skip themselves and say so.
set -euo pipefail
cd "$(dirname "$0")/.."

PG_BIN="${SINGULARR_GATE_PG_BIN:-/usr/lib/postgresql/18/bin}"
PG_DIR="${SINGULARR_GATE_PG_DIR:-/run/singularr-pg}"
PG_PORT="${SINGULARR_GATE_PG_PORT:-55432}"
PG_STARTED=0

as_pg() { # Run a command as the postgres user when this script runs as root.
  if [ "$(id -u)" = 0 ]; then runuser -u postgres -- "$@"; else "$@"; fi
}

stop_pg() {
  if [ "$PG_STARTED" = 1 ]; then
    as_pg "$PG_BIN/pg_ctl" -D "$PG_DIR/data" -m fast -w stop >/dev/null 2>&1 || true
  fi
}
trap stop_pg EXIT

START=$(date +%s)
step() { # name, command...
  local name="$1" t0 rc
  shift
  echo "== $name"
  t0=$(date +%s)
  if "$@"; then rc=0; else rc=$?; fi
  echo "== $name: $( [ "$rc" = 0 ] && echo ok || echo "FAILED (exit $rc)" ) in $(( $(date +%s) - t0 )) s"
  return "$rc"
}

start_pg() {
  mkdir -p "$PG_DIR"
  if [ "$(id -u)" = 0 ]; then chown postgres:postgres "$PG_DIR"; fi
  as_pg "$PG_BIN/initdb" -D "$PG_DIR/data" -U singularr --auth-local=trust \
    --auth-host=scram-sha-256 -E UTF8 >"$PG_DIR/initdb.log" 2>&1 \
    || { cat "$PG_DIR/initdb.log"; return 1; }
  as_pg "$PG_BIN/pg_ctl" -D "$PG_DIR/data" -l "$PG_DIR/server.log" -w -t 60 \
    -o "-c listen_addresses=127.0.0.1 -p $PG_PORT -k $PG_DIR -c max_connections=200 -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c log_statement=ddl" \
    start >/dev/null || { cat "$PG_DIR/server.log"; return 1; }
  PG_STARTED=1
  # The superuser over the socket: its folder as the host, each / written %2F.
  export SINGULARR_TEST_DATABASE_URL="postgres://singularr@$(printf '%s' "$PG_DIR" | sed 's#/#%2F#g'):$PG_PORT/postgres"
  # The password the database tests give the portal's role: a new one for each run. The server
  # logs every DDL statement. The check after the tests reads that log for it.
  SINGULARR_TEST_PORTAL_PASSWORD="portal-$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  export SINGULARR_TEST_PORTAL_PASSWORD
  "$PG_BIN/postgres" --version
}

if [ -n "${SINGULARR_TEST_DATABASE_URL:-}" ]; then
  echo "== the database-backed tests use the server in SINGULARR_TEST_DATABASE_URL"
elif [ -x "$PG_BIN/initdb" ]; then
  step "PostgreSQL for the database-backed tests" start_pg
else
  echo "== no PostgreSQL in $PG_BIN: the database-backed tests skip themselves"
fi

rustc --version
cargo --version
step "fmt" cargo fmt --all --check
step "clippy (ssr, workspace, all targets)" \
  cargo clippy --locked --workspace --all-targets --no-default-features --features ssr -- -D warnings
step "tests (ssr, workspace)" \
  cargo test --locked --workspace --no-default-features --features ssr
if [ "$PG_STARTED" = 1 ]; then
  # Each database-backed test creates a database of its own (log_statement=ddl logs it). None
  # means they skipped themselves although a server was up: that is a failure, not a pass.
  made="$(grep -c 'CREATE DATABASE' "$PG_DIR/server.log" || true)"
  echo "== database-backed tests: ${made:-0} test databases created"
  if [ "${made:-0}" -eq 0 ]; then
    echo "== the database-backed tests did not reach PostgreSQL" >&2
    exit 1
  fi
  # The portal role's password is part of no statement (requests::role). The role's ALTER ROLE
  # is in the log with the verifier.
  if grep -qF -- "$SINGULARR_TEST_PORTAL_PASSWORD" "$PG_DIR/server.log"; then
    echo "== the server's log holds the portal role's password" >&2
    exit 1
  fi
  if ! grep -qF "PASSWORD 'SCRAM-SHA-256\$" "$PG_DIR/server.log"; then
    echo "== the server's log holds no ALTER ROLE with a verifier: the check above saw nothing" >&2
    exit 1
  fi
  echo "== the portal role's password: in no logged statement"
fi
echo "GATE_OK in $(( $(date +%s) - START )) s"
