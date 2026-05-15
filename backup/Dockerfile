# syntax=docker/dockerfile:1.7
#
# Nightly Postgres → age → S3-compatible bucket backup runner.
#
# Based on the same postgres:16-alpine image as the database server: this
# guarantees pg_dump / pg_restore are byte-for-byte the server's major
# version, and the base image is on a maintained release train rather than
# a pinned Alpine point release that silently goes EOL.
#
# - rclone handles any S3-compatible target (B2, R2, Wasabi, AWS, MinIO).
# - age provides at-rest encryption with a single recipient public key.
#
# Configure via env vars from the parent compose .env. Missing required
# vars cause backup.sh to log a warning and exit 0 — the cron loop
# stays alive so an operator can fix config and restart.

FROM postgres:16-alpine@sha256:4e6e670bb069649261c9c18031f0aded7bb249a5b6664ddec29c013a89310d50

RUN apk add --no-cache \
        rclone \
        age \
        bash \
        coreutils \
        tini \
        tzdata

# Run unprivileged. The postgres base image ships its own `postgres`
# user, but this image overrides the base entrypoint, so nothing drops
# privileges unless we do it here. uid 10001 matches the api/worker/
# frontend images. pg_dump/pg_restore connect over TCP, age and rclone
# stream, and the .pgpass file goes to a mktemp under /tmp — none of it
# needs root or the `postgres` uid.
RUN addgroup -S jitacart && adduser -S -G jitacart -u 10001 -h /home/jitacart jitacart

COPY backup.sh /usr/local/bin/backup.sh
COPY restore.sh /usr/local/bin/restore.sh
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/backup.sh /usr/local/bin/restore.sh /usr/local/bin/entrypoint.sh

ENV TZ=UTC
ENV HOME=/home/jitacart
USER jitacart

ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/entrypoint.sh"]
