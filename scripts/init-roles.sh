#!/bin/sh
# Postgres init-time bootstrap. Runs once per cluster (first container start
# on an empty data dir), executed by the official postgres entrypoint from
# /docker-entrypoint-initdb.d/.
#
# Creates the three application roles that participate in RLS:
#   - jitacart_admin  : table owner; runs migrations. RLS does not apply.
#   - jitacart_app    : api runtime role; NOBYPASSRLS, gated by policies.
#   - jitacart_worker : worker runtime role; BYPASSRLS for cross-tenant jobs.
#
# Passwords come from compose env (POSTGRES_PASSWORD reused for admin so the
# existing operator secret keeps working; APP_ and WORKER_DB_PASSWORD are
# new). After this script runs the database is owned by jitacart_admin, so
# the api migration runner — connecting as jitacart_admin — can CREATE/ALTER
# freely while the runtime jitacart_app cannot.

set -e

psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" <<EOSQL
CREATE ROLE jitacart_admin  LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '${POSTGRES_PASSWORD}';
CREATE ROLE jitacart_app    LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '${APP_DB_PASSWORD}';
CREATE ROLE jitacart_worker LOGIN NOSUPERUSER BYPASSRLS   PASSWORD '${WORKER_DB_PASSWORD}';
GRANT jitacart_admin TO ${POSTGRES_USER};
ALTER DATABASE ${POSTGRES_DB} OWNER TO jitacart_admin;
EOSQL
