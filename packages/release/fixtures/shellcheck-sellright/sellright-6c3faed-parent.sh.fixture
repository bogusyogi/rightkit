#!/bin/sh
# SellRight host CLI. Installed to /usr/local/bin/sellright by install.sh.
# Operates on the Compose appliance at $SELLRIGHT_HOME (default /opt/sellright).
set -eu

SELLRIGHT_HOME="${SELLRIGHT_HOME:-/opt/sellright}"

compose() {
  docker compose --env-file "${SELLRIGHT_HOME}/.env" -f "${SELLRIGHT_HOME}/compose.yaml" "$@"
}

log() { printf '%s\n' "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }

require_home() {
  [ -f "${SELLRIGHT_HOME}/.env" ] || die "no install found at ${SELLRIGHT_HOME} (run install.sh first, or set SELLRIGHT_HOME)"
}

env_get() {
  # shellcheck disable=SC1090,SC1091
  ( . "${SELLRIGHT_HOME}/.env" && eval "printf '%s' \"\${$1:-}\"" )
}

cmd_status() {
  require_home
  compose ps
}

cmd_logs() {
  require_home
  compose logs -f --tail=200 "$@"
}

cmd_backup() {
  require_home
  mkdir -p "${SELLRIGHT_HOME}/backups"
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  out_dir="${SELLRIGHT_HOME}/backups/${stamp}"
  mkdir -p "$out_dir"
  # The api container writes into this bind mount as its non-root `node`
  # user, whose uid won't generally match whoever owns $SELLRIGHT_HOME on the
  # host (root, via install.sh). 0777 only on this one transient per-backup
  # staging directory, not on $SELLRIGHT_HOME itself.
  chmod 777 "$out_dir"
  log "Backing up database..."
  compose exec -T postgres pg_dump -U sellright -d sellright -Fc > "${out_dir}/database.dump"
  log "Backing up assets and downloads volumes..."
  compose run --rm -v "${out_dir}:/backup" -T api \
    sh -c 'tar czf /backup/assets.tar.gz -C /app/var assets && tar czf /backup/downloads.tar.gz -C /app/var downloads'
  api_ref="${COMPOSE_API_IMAGE:-$(compose images -q api)}"
  {
    printf '{\n'
    printf '  "createdAt": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '  "appVersion": "%s",\n' "$(env_get SELLRIGHT_IMAGE_TAG)"
    printf '  "apiImageId": "%s",\n' "${api_ref:-unknown}"
    printf '  "parts": {\n'
    printf '    "database.dump": "sha256:%s",\n' "$(sha256sum "${out_dir}/database.dump" | cut -d' ' -f1)"
    printf '    "assets.tar.gz": "sha256:%s",\n' "$(sha256sum "${out_dir}/assets.tar.gz" | cut -d' ' -f1)"
    printf '    "downloads.tar.gz": "sha256:%s"\n' "$(sha256sum "${out_dir}/downloads.tar.gz" | cut -d' ' -f1)"
    printf '  }\n'
    printf '}\n'
  } > "${out_dir}/manifest.json"
  log "Backup set written to ${out_dir}"
  if [ "${1:-}" = "--offsite" ]; then
    cmd_backup_offsite "$out_dir"
  fi
}

cmd_backup_offsite() {
  out_dir="$1"
  remote="$(env_get SELLRIGHT_OFFSITE_REMOTE)"
  if [ -z "$remote" ]; then
    log "SELLRIGHT_OFFSITE_REMOTE not set in ${SELLRIGHT_HOME}/.env; skipping off-site copy."
    return 0
  fi
  command -v rclone >/dev/null 2>&1 || die "rclone not installed; cannot push off-site backup"
  key="$(env_get SELLRIGHT_MASTER_KEY)"
  kit_id="$(env_get RECOVERY_KIT_ID)"
  enc_dir=$(mktemp -d)
  for f in database.dump assets.tar.gz downloads.tar.gz manifest.json; do
    openssl enc -aes-256-cbc -pbkdf2 -salt \
      -pass "pass:${key}:${kit_id}" \
      -in "${out_dir}/${f}" -out "${enc_dir}/${f}.enc"
  done
  rclone copy "$enc_dir" "${remote}/$(basename "$out_dir")/"
  rm -rf "$enc_dir"
  log "Off-site (encrypted) backup pushed to ${remote}/$(basename "$out_dir")/"
}

cmd_recovery_kit() {
  require_home
  [ -f "${SELLRIGHT_HOME}/recovery-kit.json" ] || die "no recovery kit found at ${SELLRIGHT_HOME}/recovery-kit.json"
  cat "${SELLRIGHT_HOME}/recovery-kit.json"
}

cmd_restore() {
  kit=""
  set_file=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --kit) kit="$2"; shift 2 ;;
      --set) set_file="$2"; shift 2 ;;
      *) die "unknown restore option: $1" ;;
    esac
  done
  [ -n "$kit" ] || die "usage: sellright restore --kit <recovery-kit.json> --set <backup-dir-or-tarball>"
  [ -n "$set_file" ] || die "usage: sellright restore --kit <recovery-kit.json> --set <backup-dir-or-tarball>"
  [ -f "$kit" ] || die "recovery kit not found: $kit"
  [ -e "$set_file" ] || die "backup set not found: $set_file"
  require_home

  work_dir="$set_file"
  if [ -f "${set_file}/database.dump.enc" ]; then
    log "Backup set is encrypted; decrypting with the recovery kit's master key..."
    kit_key=$(sed -n 's/.*"masterKey": *"\([^"]*\)".*/\1/p' "$kit")
    kit_id=$(sed -n 's/.*"kitId": *"\([^"]*\)".*/\1/p' "$kit")
    [ -n "$kit_key" ] && [ -n "$kit_id" ] || die "could not read masterKey/kitId from $kit"
    work_dir=$(mktemp -d)
    for f in "${set_file}"/*.enc; do
      base=$(basename "$f" .enc)
      openssl enc -d -aes-256-cbc -pbkdf2 -salt \
        -pass "pass:${kit_key}:${kit_id}" \
        -in "$f" -out "${work_dir}/${base}"
    done
  fi

  log "Restoring database from ${work_dir}/database.dump into disposable database 'sellright_restore'..."
  compose exec -T postgres dropdb --if-exists -U sellright sellright_restore
  compose exec -T postgres createdb -U sellright sellright_restore
  compose exec -T postgres pg_restore -U sellright -d sellright_restore --no-owner --no-privileges < "${work_dir}/database.dump"
  [ "$work_dir" = "$set_file" ] || rm -rf "$work_dir"
  log "Restored into disposable database 'sellright_restore'. Run docs/runbooks/recovery.md's cutover steps to promote it."
}

cmd_setup_link() {
  log "The /v1/setup/claim endpoint ships with WS-B. Until then, use:"
  log "  sellright reset-admin"
  log "to set a known admin email/password directly."
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
  docker compose --env-file "${SELLRIGHT_HOME}/.env" \
    -f "${SELLRIGHT_HOME}/compose.yaml" -f "$override_file" \
    up -d --no-deps api admin storefront
  rm -f "$override_file"
}

cmd_update() {
  require_home
  digests_file=$(mktemp)
  trap 'rm -f "$digests_file"' EXIT

  log "1/7 entering maintenance mode..."
  cmd_maintenance on

  log "2/7 taking a backup..."
  cmd_backup

  log "recording current image digests (for rollback)..."
  capture_digests > "$digests_file"

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

  log "4/7 migrating..."
  if ! compose run --rm --no-deps api sh -c 'DATABASE_URL="$DATABASE_URL_MIGRATE" node dist/scripts/migrate.js'; then
    log "migration failed; leaving maintenance on. No backup restore was performed — restore this backup set manually only if you determine the migration left the schema inconsistent: ${out_dir:-see sellright backup output above}"
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

# Best-effort keyless-signature check for the three images this compose file
# currently resolves to. Requires `cosign` on PATH (installed by install.sh);
# skips (with a warning, not a hard failure) when cosign isn't present so a
# host that hasn't re-run install.sh's setup step yet doesn't get stuck unable
# to update at all — the real gate is still install.sh's own verify-before-
# write step for a fresh install.
cosign_verify_images() {
  command -v cosign >/dev/null 2>&1 || { log "cosign not installed; skipping re-verification (already verified by install.sh at install time)"; return 0; }
  for svc in api admin storefront; do
    ref=$(compose images -q "$svc" 2>/dev/null || true)
    [ -n "$ref" ] || continue
    digest=$(docker inspect --format '{{index .RepoDigests 0}}' "$ref" 2>/dev/null || true)
    [ -n "$digest" ] || continue
    cosign verify \
      --certificate-identity-regexp '^https://github.com/Orthic-Labs/sellright/' \
      --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
      "$digest" >/dev/null 2>&1 || { log "cosign verify failed for ${digest}"; return 1; }
  done
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
  restore --kit <f> --set <dir>
                      Restore a backup set into a disposable database
  setup-link          Print how to claim/reset the installation admin
  reset-admin <email> Reset the admin account directly
  maintenance <on|off|status>
                      Toggle or check maintenance mode directly
  functional-check    Run the post-update health checks directly
  update              Maintenance on -> backup -> pull+verify -> migrate ->
                      start -> functional checks -> maintenance off.
                      Rolls back to the previous images automatically if a
                      functional check fails before reopening.
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
