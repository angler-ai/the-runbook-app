#!/bin/sh
# First start of the bundled PostgreSQL only (an empty volume): create the
# unprivileged role the server connects as. It is not a superuser and
# cannot create databases or roles; in this one database it may connect and
# create schemas (the server's first migration runs CREATE SCHEMA IF NOT
# EXISTS runbook, which PostgreSQL authorizes against the database even
# when the schema exists), so it owns the `runbook` schema and everything
# the migrations put in it. POSTGRES_USER stays the administrator for
# backups and maintenance. Values reach SQL as psql variables, never
# spliced into it.
set -eu
: "${RUNBOOK_DB_USER:?set RUNBOOK_DB_USER}"
: "${RUNBOOK_DB_PASSWORD:?set RUNBOOK_DB_PASSWORD}"
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  -v app_user="$RUNBOOK_DB_USER" -v app_password="$RUNBOOK_DB_PASSWORD" -v db="$POSTGRES_DB" <<'SQL'
CREATE ROLE :"app_user" LOGIN PASSWORD :'app_password'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
REVOKE ALL ON DATABASE :"db" FROM PUBLIC;
GRANT CONNECT, TEMPORARY, CREATE ON DATABASE :"db" TO :"app_user";
REVOKE ALL ON SCHEMA public FROM PUBLIC;
CREATE SCHEMA runbook AUTHORIZATION :"app_user";
SQL
