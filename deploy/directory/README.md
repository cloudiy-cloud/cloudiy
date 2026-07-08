# Hosted Cloudiy Directory Node

A directory node is the bootstrap discovery point: providers (`cloudiy share`) and
consumers announce/find each other through it. Running one public, always-on
directory node gives the network a stable entry point.

The node runs the unified `cloudiy` binary in `directory` mode:

```
cloudiy directory
```

On boot it prints a stable **EndpointId** — the address clients dial with `--via`.
The EndpointId is derived from the node's key, so persisting the key (via a volume or
a fixed home directory) keeps the EndpointId stable across restarts.

## Option A — Docker Compose

Requires `ghcr.io/cloudiy/cloudiy:latest` to exist. **Building and publishing that
image is a HUMAN step** — the ops session does not build the Rust binary. Once it's
published:

```bash
cd deploy/directory
docker compose up -d
docker compose logs -f directory   # copy the printed EndpointId
```

The `cloudiy-node-key` volume persists the key at `/root/.config/cloudiy`.

## Option B — systemd on a VPS / bare metal

1. Build the `cloudiy` binary (HUMAN step) and copy it to the host:
   ```bash
   sudo install -m 0755 cloudiy /usr/local/bin/cloudiy
   ```
2. Create a service user and install the unit:
   ```bash
   sudo useradd --system --create-home --home-dir /var/lib/cloudiy cloudiy
   sudo cp cloudiy-directory.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now cloudiy-directory
   ```
3. Read the EndpointId from the logs:
   ```bash
   sudo journalctl -u cloudiy-directory -f
   ```

## Pointing clients at your directory

Once you have a stable `<ENDPOINT_ID>`, clients can reach it in two ways:

- **Ad hoc**: pass `--via <ENDPOINT_ID>` on the command line.
- **As a default**: bake it in as the client-side default.

> **NOTE (human edits — do NOT let the ops scaffolding make these):**
> - The client-side default is a **one-line change** in
>   `crates/cloudiy/src/main.rs`: give the `--via` argument a default value of your
>   published `<ENDPOINT_ID>`.
> - The web marketplace picks up its default directory from the browser's
>   `localStorage` key **`cloudiy_nodes`** — set it there so hosted users get your
>   node without configuration.
>
> These edits touch `crates/` and `web/` and must be made by a human. This README
> only documents them.
