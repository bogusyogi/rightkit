#!/bin/sh
# SellRight host CLI. Installed to /usr/local/bin/sellright by install.sh.
# Operates on the Compose appliance at $SELLRIGHT_HOME (default /opt/sellright).
set -eu

SELLRIGHT_HOME="${SELLRIGHT_HOME:-/opt/sellright}"
COSIGN_VERSION="${COSIGN_VERSION:-2.4.1}"

compose() {
  docker compose --env-file "${SELLRIGHT_HOME}/.env" -f "${SELLRIGHT_HOME}/compose.yaml" "$@"
}

log() { printf '%s\n' "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }

require_home() {
  [ -f "${SELLRIGHT_HOME}/.env" ] || die "no install found at ${SELLRIGHT_HOME} (run install.sh first, or set SELLRIGHT_HOME)"
}

# Reads one KEY=value out of deploy/.env WITHOUT sourcing/executing it as
# shell — sourcing broke on any value containing a space (e.g.
# BOOTSTRAP_STORE_NAME="Drill Store" parses as an assignment followed by a
# bareword command) and, more importantly, would execute arbitrary shell
# metacharacters in any value. Values may be bare or double-quoted.
env_get() {
  [ -f "${SELLRIGHT_HOME}/.env" ] || return 0
  sed -n "s/^$1=//p" "${SELLRIGHT_HOME}/.env" | tail -n 1 | sed -e 's/^"//' -e 's/"$//'
}

cmd_status() {
  require_home
  compose ps
}

cmd_logs() {
  require_home
  compose logs -f --tail=200 "$@"
}

cmd_backup() (
  require_home
  umask 077
  backup_root="${SELLRIGHT_HOME}/backups"
  out_dir=""
  complete=0
  status=0
  # Subshell-local traps leave update's rollback bookkeeping trap intact.
  trap 'status=$?; if [ "$complete" -eq 0 ] && [ -n "$out_dir" ]; then rm -rf -- "$out_dir"; fi; exit "$status"' 0
  trap 'exit 1' HUP INT TERM
  if [ -e "$backup_root" ] || [ -L "$backup_root" ]; then
    [ ! -L "$backup_root" ] && [ -d "$backup_root" ] || die "backup parent must be a real directory: $backup_root"
    [ "$(stat -c %u -- "$backup_root")" = "$(id -u)" ] || die "backup parent must be owned by current user: $backup_root"
    chmod 700 "$backup_root"
  else
    mkdir -m 700 "$backup_root"
  fi
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  out_dir=$(mktemp -d "${backup_root}/${stamp}.XXXXXX")
  log "Backing up database..."
  compose exec -T postgres pg_dump -U sellright -d sellright -Fc > "${out_dir}/database.dump"
  log "Backing up assets and downloads volumes..."
  compose run --rm --no-deps -T api tar czf - -C /app/var assets > "${out_dir}/assets.tar.gz"
  compose run --rm --no-deps -T api tar czf - -C /app/var downloads > "${out_dir}/downloads.tar.gz"
  api_ref="${COMPOSE_API_IMAGE:-$(compose images -q api)}"
  database_sha=$(sha256sum "${out_dir}/database.dump")
  assets_sha=$(sha256sum "${out_dir}/assets.tar.gz")
  downloads_sha=$(sha256sum "${out_dir}/downloads.tar.gz")
  {
    printf '{\n'
    printf '  "createdAt": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '  "appVersion": "%s",\n' "$(env_get SELLRIGHT_IMAGE_TAG)"
    printf '  "apiImageId": "%s",\n' "${api_ref:-unknown}"
    printf '  "parts": {\n'
    printf '    "database.dump": "sha256:%s",\n' "${database_sha%% *}"
    printf '    "assets.tar.gz": "sha256:%s",\n' "${assets_sha%% *}"
    printf '    "downloads.tar.gz": "sha256:%s"\n' "${downloads_sha%% *}"
    printf '  }\n'
    printf '}\n'
  } > "${out_dir}/manifest.json"
  complete=1
  log "Backup set written to ${out_dir}"
  if [ "${1:-}" = "--offsite" ]; then
    cmd_backup_offsite "$out_dir"
  fi
)

# Encrypt-then-MAC (authenticated encryption from plain `openssl enc` +
# `openssl dgst -hmac`, portable to any OpenSSL 1.1+ without depending on
# `enc`'s newer, version-dependent AEAD tag support). Two keys are derived
# from the recovery kit via HMAC-SHA256 with distinct labels — never reuse
# the same key for encryption and authentication.
derive_key() {
  # $1 = label ("enc" or "mac"), $2 = masterKey, $3 = kitId
  printf '%s' "sellright-backup-${1}:${3}" | openssl dgst -sha256 -hmac "$2" | awk '{print $NF}'
}

encrypt_authenticated() {
  # encrypt_authenticated <in> <out.enc> <encKeyHex> <macKeyHex>
  in_file="$1"; out_file="$2"; enc_key="$3"; mac_key="$4"
  openssl enc -aes-256-cbc -pbkdf2 -iter 200000 -salt \
    -pass "pass:${enc_key}" -in "$in_file" -out "$out_file"
  openssl dgst -sha256 -hmac "$mac_key" "$out_file" | awk '{print $NF}' > "${out_file}.hmac"
}

decrypt_authenticated() {
  # decrypt_authenticated <in.enc> <out> <encKeyHex> <macKeyHex>
  in_file="$1"; out_file="$2"; enc_key="$3"; mac_key="$4"
  [ -f "${in_file}.hmac" ] || die "missing integrity tag ${in_file}.hmac — refusing to decrypt an unauthenticated file"
  expected=$(cat "${in_file}.hmac")
  actual=$(openssl dgst -sha256 -hmac "$mac_key" "$in_file" | awk '{print $NF}')
  [ "$expected" = "$actual" ] || die "integrity check failed for $(basename "$in_file") — tampered or corrupted backup, refusing to decrypt"
  openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -salt \
    -pass "pass:${enc_key}" -in "$in_file" -out "$out_file"
}

cmd_backup_offsite() (
  umask 077
  out_dir="$1"
  remote="$(env_get SELLRIGHT_OFFSITE_REMOTE)"
  if [ -z "$remote" ]; then
    log "SELLRIGHT_OFFSITE_REMOTE not set in ${SELLRIGHT_HOME}/.env; skipping off-site copy."
    return 0
  fi
  command -v rclone >/dev/null 2>&1 || die "rclone not installed; cannot push off-site backup"
  master_key="$(env_get SELLRIGHT_MASTER_KEY)"
  kit_id="$(env_get RECOVERY_KIT_ID)"
  enc_key=$(derive_key enc "$master_key" "$kit_id")
  mac_key=$(derive_key mac "$master_key" "$kit_id")
  enc_dir=$(mktemp -d)
  trap 'rm -rf -- "$enc_dir"' 0
  trap 'exit 1' HUP INT TERM
  for f in database.dump assets.tar.gz downloads.tar.gz manifest.json; do
    encrypt_authenticated "${out_dir}/${f}" "${enc_dir}/${f}.enc" "$enc_key" "$mac_key"
  done
  rclone copy "$enc_dir" "${remote}/$(basename "$out_dir")/"
  log "Off-site (encrypted + authenticated) backup pushed to ${remote}/$(basename "$out_dir")/"
)

cmd_recovery_kit() {
  require_home
  [ -f "${SELLRIGHT_HOME}/recovery-kit.json" ] || die "no recovery kit found at ${SELLRIGHT_HOME}/recovery-kit.json"
  cat "${SELLRIGHT_HOME}/recovery-kit.json"
}

cmd_restore() (
  umask 077
  decrypted_dir=""
  status=0
  trap 'status=$?; if [ -n "$decrypted_dir" ]; then rm -rf -- "$decrypted_dir"; fi; exit "$status"' 0
  trap 'exit 1' HUP INT TERM
  kit=""
  set_file=""
  yes=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --kit) kit="$2"; shift 2 ;;
      --set) set_file="$2"; shift 2 ;;
      --yes) yes=1; shift ;;
      *) die "unknown restore option: $1" ;;
    esac
  done
  usage_msg="usage: sellright restore --kit <recovery-kit.json> --set <backup-dir> [--yes]"
  [ -n "$kit" ] || die "$usage_msg"
  [ -n "$set_file" ] || die "$usage_msg"
  [ -f "$kit" ] || die "recovery kit not found: $kit"
  [ -e "$set_file" ] || die "backup set not found: $set_file"
  require_home

  work_dir="$set_file"
  if [ -f "${set_file}/database.dump.enc" ]; then
    log "Backup set is encrypted; verifying integrity and decrypting with the recovery kit..."
    kit_key=$(sed -n 's/.*"masterKey": *"\([^"]*\)".*/\1/p' "$kit")
    kit_id=$(sed -n 's/.*"kitId": *"\([^"]*\)".*/\1/p' "$kit")
    [ -n "$kit_key" ] && [ -n "$kit_id" ] || die "could not read masterKey/kitId from $kit"
    enc_key=$(derive_key enc "$kit_key" "$kit_id")
    mac_key=$(derive_key mac "$kit_key" "$kit_id")
    decrypted_dir=$(mktemp -d)
    work_dir="$decrypted_dir"
    for f in "${set_file}"/*.enc; do
      base=$(basename "$f" .enc)
      decrypt_authenticated "$f" "${work_dir}/${base}" "$enc_key" "$mac_key"
    done
  fi

  for part in database.dump assets.tar.gz downloads.tar.gz; do
    [ -f "${work_dir}/${part}" ] || die "backup set is missing ${part} — refusing a partial restore"
  done

  # This is destructive by design: it replaces the live database, assets, and
  # downloads. Requires an explicit --yes outside a real terminal (CI, scripts)
  # rather than silently assuming consent.
  if [ "$yes" -ne 1 ]; then
    if [ -t 0 ]; then
      printf 'This REPLACES the live database, assets, and downloads with the restored backup.\nA safety backup of the current live data is taken first, but this is still destructive.\nType "yes" to continue: ' >&2
      read -r confirm
      [ "$confirm" = "yes" ] || die "aborted (confirmation not given)"
    else
      die "refusing to promote a restore without confirmation in a non-interactive session; pass --yes"
    fi
  fi

  log "Taking a pre-restore safety backup of the current live stack..."
  cmd_backup >&2

  log "Restoring database from ${work_dir}/database.dump into disposable database 'sellright_restore'..."
  compose exec -T postgres dropdb --if-exists -U sellright sellright_restore
  compose exec -T postgres createdb -U sellright sellright_restore
  compose exec -T postgres pg_restore -U sellright -d sellright_restore --no-owner --no-privileges < "${work_dir}/database.dump"

  log "Promoting: stopping api, terminating live connections, swapping database..."
  compose stop api
  compose exec -T postgres psql -U sellright -d postgres -c \
    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname IN ('sellright','sellright_restore') AND pid <> pg_backend_pid();"
  prev_db="sellright_prev_$(date -u +%Y%m%dT%H%M%SZ)"
  compose exec -T postgres psql -U sellright -d postgres -c "ALTER DATABASE sellright RENAME TO ${prev_db};"
  compose exec -T postgres psql -U sellright -d postgres -c "ALTER DATABASE sellright_restore RENAME TO sellright;"

  log "Promoting: replacing live assets and downloads volumes..."
  compose run --rm --no-deps -T api sh -c '
    set -e
    rm -rf /app/var/assets/* /app/var/assets/.[!.]* 2>/dev/null || true
    tar xzf - -C /app/var/assets --strip-components=1
  ' < "${work_dir}/assets.tar.gz"
  compose run --rm --no-deps -T api sh -c '
    set -e
    rm -rf /app/var/downloads/* /app/var/downloads/.[!.]* 2>/dev/null || true
    tar xzf - -C /app/var/downloads --strip-components=1
  ' < "${work_dir}/downloads.tar.gz"

  compose up -d api
  log "Restore promoted. Previous database preserved as '${prev_db}' — drop it manually once verified:"
  log "  sellright status  # confirm api is healthy, then:"
  log "  docker compose ... exec postgres dropdb -U sellright ${prev_db}"
)

cmd_setup_link() {
  require_home
  # dist/scripts/setup-link.js prints ONLY the raw token on stdout (its own
  # progress/errors go to stderr, which passes through uncaptured here). It
  # exits non-zero — with an explanatory stderr line — once an installation
  # admin already exists; there is deliberately no way to re-claim an
  # installed instance through this path (use reset-admin instead).
  token=$(compose exec -T api node dist/scripts/setup-link.js) \
    || die "setup-link failed (see above). If this installation was already claimed, use: sellright reset-admin <email>"
  [ -n "$token" ] || die "setup-link produced no token"
  domain="$(env_get SELLRIGHT_DOMAIN)"
  case "$domain" in
    :*|'')
      log "Claim link (valid 7 days, single use). SELLRIGHT_DOMAIN isn't set to a real hostname yet, so this uses localhost — swap in your server's address if you're browsing remotely:"
      log "  http://localhost${domain}/setup?token=${token}"
      ;;
    *)
      log "Claim link (valid 7 days, single use):"
      log "  https://${domain}/setup?token=${token}"
      ;;
  esac
}

cmd_reset_admin() {
  require_home
  email="${1:-}"
  [ -n "$email" ] || die "usage: sellright reset-admin <email>"
  password="$(gen_secret_local 24)"
  compose exec -T -e ADMIN_PASSWORD="$password" api node dist/scripts/seed-admin.js "$email"
  log "Admin password reset for ${email}: ${password}"
  log "(shown once — this terminal only; not logged or stored anywhere else)"
}

gen_secret_local() {
  openssl rand -hex "$1" 2>/dev/null || head -c "$1" /dev/urandom | od -An -tx1 | tr -d ' \n'
}

cmd_maintenance() {
  require_home
  action="${1:-}"
  case "$action" in
    on|off|status) ;;
    *) die "usage: sellright maintenance <on|off|status>" ;;
  esac
  compose exec -T api node dist/scripts/maintenance-cli.js "$action"
}

cmd_functional_check() {
  require_home
  compose exec -T api node dist/scripts/functional-check.js
}

# Records each running service's current image digest (repo@sha256:...), one
# `service digest` pair per line. Used by cmd_update to remember what to
# restart if the new images fail their functional checks.
capture_digests() {
  for svc in api admin storefront; do
    ref=$(compose images -q "$svc" 2>/dev/null || true)
    [ -n "$ref" ] || continue
    digest=$(docker inspect --format '{{index .RepoDigests 0}}' "$ref" 2>/dev/null || true)
    [ -n "$digest" ] && printf '%s %s\n' "$svc" "$digest"
  done
}

# Re-pins each service to the exact digest captured by capture_digests, via a
# throwaway Compose override file, then recreates just those containers. A
# digest (not a floating tag like "latest") is used deliberately — this is
# the rollback path, so it must not depend on the registry still serving
# whatever the tag currently resolves to.
restart_digests() {
  digests_file="$1"
  [ -s "$digests_file" ] || { log "no previous digests recorded; cannot roll back automatically"; return 1; }
  override_file=$(mktemp)
  {
    printf 'services:\n'
    while read -r svc digest; do
      [ -n "$svc" ] || continue
      printf '  %s:\n    image: %s\n' "$svc" "$digest"
    done < "$digests_file"
  } > "$override_file"
  log "rollback plan:"
  cat "$digests_file" >&2
  # Through compose() (not a bare `docker compose ...`) so this honors any
  # project-name/host override compose() carries — in production both resolve
  # to the same `name: sellright` project either way, but a test harness that
  # patches compose() to add `-p <isolated-project>` (see the CI drill in
  # .github/workflows/ci.yml's verify-update job) must not have the rollback
  # path silently fall back to the default project and operate on the wrong
  # containers.
  compose -f "$override_file" up -d --no-deps api admin storefront
  rm -f "$override_file"
}

cmd_update() {
  require_home
  from_source=0
  for arg in "$@"; do
    case "$arg" in
      --from-source) from_source=1 ;;
      *) die "unknown argument to 'update': $arg" ;;
    esac
  done

  digests_file=$(mktemp)
  trap 'rm -f "$digests_file"' EXIT

  log "1/7 entering maintenance mode..."
  cmd_maintenance on

  log "2/7 taking a backup..."
  cmd_backup

  log "recording current image digests (for rollback)..."
  capture_digests > "$digests_file"

  if [ "$from_source" -eq 1 ]; then
    # Owner-operated private forks: no registry to pull signed images from,
    # so this path builds api/admin/storefront LOCALLY from the checked-out
    # repo at SELLRIGHT_SOURCE (compose.build.yaml, the same override file
    # local dev and CI's verify-appliance job already use) and skips the
    # cosign gate entirely — there is nothing to verify a signature against
    # for an image that was never published. This is intentionally a
    # separate, explicit flag: plain `sellright update` (no flag) must always
    # stay on the cosign-verified registry path; a host is never silently
    # downgraded to unverified images.
    : "${SELLRIGHT_SOURCE:?--from-source requires SELLRIGHT_SOURCE=/path/to/checked-out/sellright}"
    [ -f "${SELLRIGHT_SOURCE}/deploy/compose.build.yaml" ] || die "SELLRIGHT_SOURCE (${SELLRIGHT_SOURCE}) has no deploy/compose.build.yaml — not a sellright checkout?"
    log "3/7 building images from source (${SELLRIGHT_SOURCE}), no registry pull, no cosign verify..."
    if ! compose -f "${SELLRIGHT_SOURCE}/deploy/compose.build.yaml" build; then
      log "source build failed; leaving maintenance on, no changes made"
      exit 1
    fi
  else
    log "3/7 pulling + verifying signed images..."
    if ! compose pull; then
      log "pull failed; leaving maintenance on, no changes made"
      exit 1
    fi
    # WS-D's install.sh performs the authoritative cosign keyless-verify (image
    # digest against the release workflow's OIDC identity) before ever writing
    # an image ref into compose's env; `compose pull` here re-resolves the exact
    # digest install.sh already verified for the configured tag. A same-tag
    # cosign re-check is repeated here defensively so `sellright update` alone
    # (without re-running install.sh) still never runs an image whose signature
    # can't be verified.
    cosign_verify_images || { log "cosign verification failed; leaving maintenance on, no changes made"; exit 1; }
  fi

  log "4/7 migrating..."
  # shellcheck disable=SC2016 # single-quoted deliberately: $DATABASE_URL_MIGRATE
  # must expand inside the container's sh, not here on the host.
  if ! compose run --rm --no-deps api sh -c 'DATABASE_URL="$DATABASE_URL_MIGRATE" node dist/scripts/migrate.js'; then
    log "migration failed; leaving maintenance on. No backup restore was performed — restore this backup set manually only if you determine the migration left the schema inconsistent: see sellright backup output above"
    exit 1
  fi

  log "5/7 starting updated services..."
  compose up -d api admin storefront

  log "6/7 running functional checks..."
  if ! cmd_functional_check; then
    log "functional checks FAILED after update. Rolling back to the previous images BEFORE reopening (maintenance stays on). No backup restore performed."
    restart_digests "$digests_file"
    log "rollback complete. Re-running functional checks against the restored images..."
    if cmd_functional_check; then
      log "previous images restored and healthy. Maintenance remains ON — clear it manually once you've investigated: sellright maintenance off"
    else
      log "CRITICAL: functional checks still failing after rollback. Maintenance remains ON. Manual intervention required — see docs/runbooks/migrations.md."
    fi
    exit 1
  fi

  log "7/7 leaving maintenance mode..."
  cmd_maintenance off
  log "update complete."
}

# Fail-closed: `sellright update` must never run an image it couldn't verify.
# Installs cosign itself (same download install.sh's install_cosign performs)
# rather than skipping — a host missing cosign is a setup gap to fix, not a
# reason to relax the update path's own signature gate.
ensure_cosign() {
  command -v cosign >/dev/null 2>&1 && return 0
  log "cosign not found; installing cosign ${COSIGN_VERSION}..."
  arch=$(uname -m)
  case "$arch" in
    x86_64) cosign_arch=amd64 ;;
    aarch64) cosign_arch=arm64 ;;
    *) die "unsupported architecture for cosign: $arch" ;;
  esac
  if ! curl -fsSL -o /usr/local/bin/cosign \
      "https://github.com/sigstore/cosign/releases/download/v${COSIGN_VERSION}/cosign-linux-${cosign_arch}"; then
    die "failed to download cosign — cannot verify image signatures, refusing to update"
  fi
  chmod +x /usr/local/bin/cosign
  command -v cosign >/dev/null 2>&1 || die "cosign install did not produce a usable binary"
}

# Keyless-signature check for the three images `compose pull` just fetched —
# i.e. the images `compose up -d` is about to SWITCH TO, not whatever is
# currently running. Fail-closed: missing cosign, an unresolvable image ref,
# or any failed verification all abort the update (non-zero return) —
# cmd_update treats that as "leave maintenance on, make no changes", never as
# "proceed anyway". This is the same identity/issuer install.sh's own
# verify_and_pull checks, re-applied here so `sellright update` never trusts
# a pull it hasn't independently verified itself.
#
# Bug history — read before touching image resolution here again:
#   1. `compose images -q "$svc"` resolves the image of the service's
#      CURRENTLY RUNNING container, not the tag `compose pull` just
#      repointed to (containers aren't recreated until later in cmd_update,
#      at "5/7 starting updated services"). An attacker or a broken registry
#      could serve an unsigned/wrong image at the pulled tag and the OLD,
#      already-verified image would get verified again while the new one
#      still gets switched to.
#   2. `compose config --images "$svc"` (a same-CI-run replacement attempt)
#      does NOT reliably filter to just that service — observed in CI
#      returning the FULL unfiltered image list regardless of the service
#      arg, so `| head -n 1` silently verified an unrelated service's image.
#   3. Zipping `compose config --services` with `compose config --images` by
#      LINE NUMBER (a second same-CI-run attempt) assumed the two separate
#      invocations enumerate services in the same order. They don't have to,
#      and in CI they didn't — this produced a different wrong image per run
#      (caddy, then postgres, both reported as "api").
# Fix: `compose config --format json` renders the FULLY RESOLVED config as
# one document; jq indexes `.services.<name>.image` directly by key — no
# filtering flag, no cross-invocation ordering assumption, no ambiguity.
cosign_verify_images() {
  ensure_cosign || return 1
  # jq resolves the per-service image out of the compose config JSON below.
  # Not installed by install.sh today (a gap worth fixing there separately);
  # fail closed with an actionable message rather than falling back to a
  # less reliable parsing method for a security-critical read.
  command -v jq >/dev/null 2>&1 || { log "jq not found — required to resolve target image refs; install it (e.g. 'apt-get install -y jq') and retry. Refusing to update without it."; return 1; }
  config_json=$(compose config --format json 2>/dev/null) || { log "could not read compose config; refusing to update"; return 1; }
  verified_any=0
  for svc in api admin storefront; do
    ref=$(printf '%s' "$config_json" | jq -r --arg svc "$svc" '.services[$svc].image // empty')
    if [ -z "$ref" ]; then
      log "could not resolve the target image for service '${svc}'; refusing to update without verifying it"
      return 1
    fi
    digest=$(docker image inspect --format '{{index .RepoDigests 0}}' "$ref" 2>/dev/null || true)
    if [ -z "$digest" ]; then
      log "could not resolve a content digest for service '${svc}' (ref: ${ref}); refusing to update without verifying it"
      return 1
    fi
    if ! cosign verify \
        --certificate-identity-regexp '^https://github.com/Orthic-Labs/sellright/' \
        --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
        "$digest" >/dev/null 2>&1; then
      log "cosign verify FAILED for ${svc} (${digest}); refusing to update"
      return 1
    fi
    verified_any=$((verified_any + 1))
  done
  [ "$verified_any" -eq 3 ] || { log "expected to verify 3 images (api, admin, storefront), verified ${verified_any}"; return 1; }
  return 0
}

usage() {
  cat <<'EOF'
Usage: sellright <command> [args]

Commands:
  status              Show container status
  logs [service]      Tail logs (all services, or one)
  backup [--offsite]  Take a database + assets + downloads backup set
  recovery-kit        Print the recovery kit (master key + backup location)
  restore --kit <f> --set <dir> [--yes]
                      Restore + PROMOTE database, assets, and downloads from
                      a backup set (encrypted or plain). Destructive: takes a
                      pre-restore safety backup first and prompts for
                      confirmation unless --yes is given.
  setup-link          Print how to claim/reset the installation admin
  reset-admin <email> Reset the admin account directly
  maintenance <on|off|status>
                      Toggle or check maintenance mode directly
  functional-check    Run the post-update health checks directly
  update [--from-source]
                      Maintenance on -> backup -> pull+verify -> migrate ->
                      start -> functional checks -> maintenance off.
                      Rolls back to the previous images automatically if a
                      functional check fails before reopening.
                      --from-source builds api/admin/storefront locally from
                      SELLRIGHT_SOURCE (a checked-out repo, via
                      compose.build.yaml) instead of pulling+cosign-verifying
                      registry images — for owners running private forks with
                      no image registry. Same maintenance/backup/migrate/
                      functional-check/rollback flow either way.
EOF
}

main() {
  cmd="${1:-}"
  [ -n "$cmd" ] || { usage; exit 1; }
  shift
  case "$cmd" in
    status) cmd_status "$@" ;;
    logs) cmd_logs "$@" ;;
    backup) cmd_backup "$@" ;;
    recovery-kit) cmd_recovery_kit "$@" ;;
    restore) cmd_restore "$@" ;;
    setup-link) cmd_setup_link "$@" ;;
    reset-admin) cmd_reset_admin "$@" ;;
    maintenance) cmd_maintenance "$@" ;;
    functional-check) cmd_functional_check "$@" ;;
    update) cmd_update "$@" ;;
    -h|--help|help) usage ;;
    *) log "unknown command: $cmd"; usage; exit 1 ;;
  esac
}

main "$@"
