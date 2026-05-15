#!/bin/sh
# Postgres init-time bootstrap. Runs once per cluster (first container start
# on an empty data dir), executed by the official postgres entrypoint from
# /docker-entrypoint-initdb.d/.
#
# The cluster's bootstrap superuser is `jitacart_bootstrap` (set via
# POSTGRES_USER in compose). It is deliberately NOT named `jitacart` and is
# never used in a runtime DATABASE_URL — a superuser bypasses all RLS, so it
# must not be reachable by application code. Only the four roles below ever
# appear in a connection string:
#   - jitacart_admin   : table owner; runs migrations. RLS does not apply.
#                        Has CREATEROLE + BYPASSRLS because the init migration's
#                        idempotent `CREATE ROLE … EXCEPTION WHEN
#                        duplicate_object` is checked for privilege BEFORE
#                        existence — a non-CREATEROLE admin gets 42501 even
#                        when the role already exists. CREATEROLE covers
#                        jitacart_app (NOBYPASSRLS) but PG 16 separately
#                        requires the creator to hold BYPASSRLS to create a
#                        BYPASSRLS role (jitacart_worker, jitacart_backup), so
#                        admin needs both. BYPASSRLS on admin itself is a
#                        functional no-op — admin is the table owner and is
#                        already exempt from RLS unless we add FORCE RLS,
#                        which we deliberately don't.
#   - jitacart_app     : api runtime role; NOBYPASSRLS, gated by policies.
#   - jitacart_worker  : worker runtime role; BYPASSRLS for cross-tenant jobs.
#   - jitacart_backup  : backup runtime role; SELECT-only + BYPASSRLS so
#                        pg_dump can read every tenant's rows but the backup
#                        container can never mutate data.
#
# Passwords come from compose env (POSTGRES_PASSWORD reused for admin so the
# existing operator secret keeps working; APP_/WORKER_/BACKUP_DB_PASSWORD are
# new). After this script runs the database is owned by jitacart_admin, so
# the api migration runner — connecting as jitacart_admin — can CREATE/ALTER
# freely while the runtime jitacart_app cannot.

set -e

psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" <<EOSQL
CREATE ROLE jitacart_admin  LOGIN NOSUPERUSER BYPASSRLS CREATEROLE PASSWORD '${POSTGRES_PASSWORD}';
CREATE ROLE jitacart_app    LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '${APP_DB_PASSWORD}';
CREATE ROLE jitacart_worker LOGIN NOSUPERUSER BYPASSRLS   PASSWORD '${WORKER_DB_PASSWORD}';
CREATE ROLE jitacart_backup LOGIN NOSUPERUSER BYPASSRLS   PASSWORD '${BACKUP_DB_PASSWORD}';
GRANT jitacart_admin TO ${POSTGRES_USER};
ALTER DATABASE ${POSTGRES_DB} OWNER TO jitacart_admin;
EOSQL
