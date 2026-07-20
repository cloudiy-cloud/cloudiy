# E2E: encrypted snapshot volume mode (RFC-0009 prototype)

Manual end-to-end for `CLOUDIY_VOLUME_MODE=snapshot`. It needs Docker and an
rclone-configured remote, so it is a documented procedure rather than a CI
script (the pure parts — key derivation, manifest parsing, the restic argv — are
unit-tested in `crates/cloudiy/src/{volume,vm}.rs`, run by `cargo test`).

The default `rclone copy` path needs none of this and is unchanged; this only
exercises the opt-in engine.

## Prerequisites

1. **Docker** running.
2. **An rclone remote** the operator already uses for `CLOUDIY_VOLUME_REMOTE`
   (e.g. an S3/R2 bucket, or a `local:` remote for a pure-local test), and its
   `rclone.conf`.
3. **A restic+rclone image.** restic's `rclone:` backend shells out to `rclone`,
   so the container needs both binaries. Build a tiny one:

   ```dockerfile
   # Dockerfile.restic-rclone
   FROM restic/restic:latest
   RUN apk add --no-cache rclone
   ```
   ```bash
   docker build -t cloudiy/restic-rclone -f Dockerfile.restic-rclone .
   ```
   (Override the name with `CLOUDIY_RESTIC_IMAGE` if you tag it differently.)

## Local-only remote (no cloud account needed)

```bash
# an rclone "local" remote pointing at a host dir that stands in for the store
mkdir -p /tmp/cloudiy-store
cat > /tmp/rclone.conf <<'EOF'
[store]
type = local
EOF
export CLOUDIY_RCLONE_CONFIG=/tmp/rclone.conf
export CLOUDIY_VOLUME_REMOTE=store:/tmp/cloudiy-store
export CLOUDIY_VOLUME_MODE=snapshot
```

## The key handoff (Architecture A interim, RFC-0009 §3.2)

The consumer produces the wallet signature; the provider consumes it. On the
**consumer**:

```bash
cloudiy volume sig            # prints CLOUDIY_VOLUME_KEY_SIG=<128 hex>
```

Export that value in the **provider's** environment (this is the interim
exposure the RFC is explicit about — the provider can reconstruct the key):

```bash
export CLOUDIY_VOLUME_KEY_SIG=<the hex from above>
```

## Run it

```bash
# 1. provider with snapshot mode on
cloudiy share --no-http &

# 2. provision a VM, write some state
cloudiy vm up --to <node-id>
cloudiy shell --to <node-id>       # then: echo hi > /root/canary.txt ; exit

# 3. stop → this triggers a restic *backup* to the store (encrypted)
cloudiy vm down --to <node-id>

# 4. confirm the store holds a restic repo (ciphertext, not your files)
ls /tmp/cloudiy-store/<owner-id>/   # => config, data/, index/, keys/, snapshots/
#    grep -r canary /tmp/cloudiy-store  => nothing: it's encrypted

# 5. provision again → restic *restore* brings /root back
cloudiy vm up --to <node-id>
cloudiy shell --to <node-id>       # then: cat /root/canary.txt  => "hi"
```

## What to verify

- **Confidentiality at rest**: `grep -r canary /tmp/cloudiy-store` finds nothing;
  the store contains a restic repo, not plaintext files.
- **Incrementality**: a second `vm down` after a small change adds a snapshot
  (`restic snapshots`) without re-uploading the whole home.
- **Wrong key fails closed**: point `CLOUDIY_VOLUME_KEY_SIG` at a different
  wallet's signature and the restore fails rather than returning garbage — the
  repo is keyed.
- **Default untouched**: unset `CLOUDIY_VOLUME_MODE` and the same flow uses
  `rclone copy` exactly as before.

## Known limits (see RFC-0009 §3.2)

This interim is **Architecture A**: the provider can reconstruct the key at
snapshot time, so it protects state **at rest on the store**, not against the
provider itself. The provider-blind variant (Architecture B — snapshot runs
consumer-side over the tunnel, key never leaves the client) is the target and is
a follow-up gated on the RFC's decision points.
