# JitaCart — Backup & Restore Runbook

Canonical operator reference for the `backup` service in
`docker-compose.yml`. Nightly Postgres dump → age-encrypted →
S3-compatible bucket via rclone.

The pipeline streams `pg_dump --format=custom --compress=9` through
`age` (public-key encryption) into `rclone rcat`; nothing is ever
written to local disk in plaintext. Restore is the same pipeline in
reverse.

## Setup (first time)

1. **Generate an age key** on a trusted machine — *not* the prod host.
   The private key only needs to exist where you'd restore from.

   ```sh
   age-keygen -o jitacart-backup.age
   ```

   The `# public key:` line goes into `BACKUP_AGE_RECIPIENT` in `.env`.
   Stash the full file (with the secret line) somewhere durable: a
   password manager, paper printout in a safe, etc.

2. **Pick a bucket**. B2, R2, Wasabi, AWS S3, MinIO — anything `rclone`
   speaks. Copy `backup/rclone.conf.example` to `backup/rclone.conf`
   and fill in.

3. **Set the four required env vars** in `.env`:

   ```sh
   BACKUP_AGE_RECIPIENT=age1...      # public key from step 1
   BACKUP_RCLONE_REMOTE=b2:bucket/jc # remote + path
   BACKUP_RETAIN_DAILY=30            # daily prune horizon
   BACKUP_HOUR_UTC=3                 # 03:00 UTC fire time
   ```

4. **Bring the stack up** (or restart `backup` if the stack is
   already running):

   ```sh
   docker compose up -d backup
   docker compose logs backup        # expect: "next backup at ..."
   ```

5. **Smoke-test now**, without waiting for the cron fire:

   ```sh
   docker compose run --rm -e BACKUP_RUN_ON_START=true backup
   ```

   Verify the new object appears in your bucket. Then unset the env
   override so the next container restart doesn't double-back-up.

If `BACKUP_AGE_RECIPIENT` or `BACKUP_RCLONE_REMOTE` is unset, the
container stays `Up` with a single WARN log line — visible
misconfiguration, no crash loop.

## Restore

```sh
# Mount the private age key into a one-off run of the backup container.
# BACKUP_RESTORE_CONFIRM must literally equal the target DB name — this
# is the only thing between you and an accidental prod overwrite.

docker compose run --rm \
    -v "$HOME/.age/jitacart-backup.age:/run/age.key:ro" \
    -e BACKUP_AGE_IDENTITY=/run/age.key \
    -e BACKUP_RESTORE_CONFIRM=jitacart_restore \
    -e BACKUP_RESTORE_TARGET_DB=jitacart_restore \
    backup restore 2026-05-10
```

Use `restore latest` instead of a date to grab the most recent dump.

By default the restore lands in `jitacart_restore` (the side DB),
not the live `jitacart` DB — so a forgotten `BACKUP_RESTORE_TARGET_DB`
can't silently overwrite prod just because the confirm token happens
to match the live DB name. To overwrite prod you must set
`BACKUP_RESTORE_TARGET_DB=jitacart` explicitly *and* match it with
`BACKUP_RESTORE_CONFIRM=jitacart`. The normal flow — validate the
dump in `jitacart_restore` first, then swap `DATABASE_URL` and bounce
api + worker — is what the defaults bias you toward.

## Quarterly restore drill

A backup you've never restored is hope, not a backup. Run this every
quarter, log the date and outcome somewhere durable.

1. **Pick a recent dump**: pick yesterday's date — exercises the full
   pipeline, doesn't depend on prune behavior.
2. **Restore into a side DB**:

   ```sh
   docker compose exec -T postgres \
       psql -U jitacart_bootstrap -c "CREATE DATABASE jitacart_restore;"
   docker compose run --rm \
       -v "$HOME/.age/jitacart-backup.age:/run/age.key:ro" \
       -e BACKUP_AGE_IDENTITY=/run/age.key \
       -e BACKUP_RESTORE_CONFIRM=jitacart_restore \
       -e BACKUP_RESTORE_TARGET_DB=jitacart_restore \
       backup restore latest
   ```

3. **Sanity-check row counts** against the live DB:

   ```sh
   docker compose exec -T postgres psql -U jitacart_bootstrap -d jitacart \
       -c "SELECT count(*) FROM users;"
   docker compose exec -T postgres psql -U jitacart_bootstrap -d jitacart_restore \
       -c "SELECT count(*) FROM users;"
   ```

   Repeat for `groups`, `lists`, `claims`. Diff should be tiny (rows
   created since the dump was taken).

4. **Tenant isolation check**: run the api's tenant-isolation
   integration test against the restored DB:

   ```sh
   DATABASE_URL=postgres://jitacart_app:$APP_DB_PASSWORD@postgres:5432/jitacart_restore \
       cargo test -p jitacart-api --test tenant_isolation
   ```

5. **Tear down**:

   ```sh
   docker compose exec -T postgres \
       psql -U jitacart_bootstrap -c "DROP DATABASE jitacart_restore;"
   ```

Outcomes worth alerting on:

- Restore failed → bucket / age key / pipeline broken; fix before next
  scheduled fire.
- Row counts wildly off → backup truncation; check `backup.sh` logs
  from the dump date.
- Tenant isolation test fails on restored data → schema drift between
  what was dumped and what the binary expects; reconcile migrations.

## Threats

- **Bucket compromise alone**: ciphertext only; useless without the
  age key.
- **Age key compromise alone**: useless without the ciphertext.
- **Both lost**: restoration impossible. Print the age key.
- **Bucket lifecycle misconfiguration**: rclone-side prune is bounded
  by `BACKUP_RETAIN_DAILY` (default 30). If your bucket *also* has a
  shorter object-lifetime policy, that wins. Configure the bucket
  with at least 35 days retention and let the backup script's prune
  be the floor.
