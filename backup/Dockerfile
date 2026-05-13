# syntax=docker/dockerfile:1.7
#
# Nightly Postgres → age → S3-compatible bucket backup runner.
#
# - postgresql-client matches the server major version (16) for pg_dump
#   compatibility.
# - rclone handles any S3-compatible target (B2, R2, Wasabi, AWS, MinIO).
# - age provides at-rest encryption with a single recipient public key.
#
# Configure via env vars from the parent compose .env. Missing required
# vars cause backup.sh to log a warning and exit 0 — the cron loop
# stays alive so an operator can fix config and restart.

FROM alpine:3.19@sha256:6baf43584bcb78f2e5847d1de515f23499913ac9f12bdf834811a3145eb11ca1

RUN apk add --no-cache \
        postgresql16-client \
        rclone \
        age \
        bash \
        coreutils \
        tini \
        tzdata

COPY backup.sh /usr/local/bin/backup.sh
COPY restore.sh /usr/local/bin/restore.sh
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/backup.sh /usr/local/bin/restore.sh /usr/local/bin/entrypoint.sh

ENV TZ=UTC

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/entrypoint.sh"]
