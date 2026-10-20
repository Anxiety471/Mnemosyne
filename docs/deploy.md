# Ubuntu personal Drive deployment

Drawer is a private, self-hosted file-transfer app, **not** a NAS operating system.

## Prerequisites

Ubuntu Server with Docker Engine + Compose v2 and `flock` (from util-linux), available disk space, and a private way to connect (SSH forwarding, Tailscale, or an HTTPS reverse proxy). Install Docker using its official Ubuntu instructions. This compose configuration publishes Drawer on **127.0.0.1:8787**, not the public interface: `DRAWER_PUBLISH` defaults to `127.0.0.1:8787`, so nothing is reachable off-host until you change it.

## Start

```sh
git clone https://github.com/Anxiety471/drawer.git
cd drawer
cp .env.example .env
chmod 600 .env
# Edit .env and replace DRAWER_ADMIN_PASSWORD and DRAWER_TOKEN
docker compose up -d --build
docker compose ps
```

`.env` holds the admin password and the machine token, so keep it at `chmod 600` and out of version control. Compose reads it for `env_file` and for `${DRAWER_PUBLISH}` / `${DRAWER_MEM_LIMIT}` / `${DRAWER_CPU_LIMIT}` substitution; its values also appear in `docker inspect` output, which is another reason not to world-read it.

**Building the image.** `.dockerignore` keeps `.env`, `backups/`, `data/`, `*.tar.gz`, `target/`, `ui/node_modules/` and `ui/dist` out of the build context, so a local `.env` can never reach an image layer. The image is a `debian:bookworm-slim` runtime (roughly 80 MB compressed) plus the built binary and UI. The runtime installs only `tar` and `bash`: `tar` is needed by the maintenance scripts, and `bash` provides `/dev/tcp` for the healthcheck, so no HTTP client is installed.

The admin password signs into the browser at `http://127.0.0.1:8787`. The **different** `DRAWER_TOKEN` is for Hermes, CLI HTTP clients, and MCP tools; do not put this token in a shared link.

Use **Tailscale Serve** or an HTTPS reverse proxy for remote devices. Do **not** expose port 8787 directly to the public internet. To publish it anyway, set `DRAWER_PUBLISH=0.0.0.0:8787` in `.env`; prefer the reverse-proxy route. Set `DRAWER_COOKIE_SECURE=true` if clients visit the HTTPS address (the backend may still see HTTP from a local reverse proxy). For a browser visiting a Tailscale private *HTTP* address, leave the cookie flag false, because browsers otherwise refuse the cookie. HTTPS is preferable.

The container runs with a **read-only root filesystem**, `no-new-privileges`, and an `init` process. Its only writable state is the `drawer_data` volume at `/data` and the bind-mounted `./backups`; `/tmp` is a tmpfs. Resource limits default to 512 MB and 1 CPU and are raised with `DRAWER_MEM_LIMIT` / `DRAWER_CPU_LIMIT`. `docker compose stop` gives the process 30 seconds (`stop_grace_period`) to close the store before it is killed. Create `backups/` (`mkdir -p backups`) before the first run and keep it at `chmod 700`, since archives and their `.sha256` manifests land there and `docker inspect` shows the mounted path.

With `DRAWER_ADMIN_PASSWORD` configured, all REST and MCP routes require an authenticated session or bearer token. The `/auth/login` endpoint authenticates a single admin using an in-memory 24-hour, HttpOnly, SameSite=Strict cookie; it signs out with `/auth/logout`. A restart invalidates browser sessions. Shared links in `/s/<token>` are download-only capabilities and need no login, but the recipient must have network reachability. The app does not currently provide separate human users or per-user identities.

## Browser transfers

- Upload multiple files, drag/drop, or choose **New → Upload folder**.
- Transfers use 4 MiB chunks and can resume by choosing the **same file again**. The browser stores a transfer ID matched to the destination path, file size, and last-modified timestamp. If the browser has been reset or the local file changed, start a new transfer.
- **Pause** stops the current transfer without deleting its staging file. Staged uploads expire after 48 hours; the server removes expired staging files on startup and hourly.
- Each resumable transfer is verified by byte length before commit. An API caller may additionally submit a SHA-256 hash, which is verified at finalization.
- Resumable uploads support up to **4 GiB** and public/private downloads stream directly from disk rather than loading the entire file into server RAM. Private downloads accept standard single byte-range requests. The older single-request `PUT /files` path remains limited to 512 MiB.
- Select multiple items to move/trash or download file-by-file. Browser vendors may block multiple simultaneous downloads.

## Data and backups

Drawer persists a SQLite database plus content-addressed blobs on a Docker volume. All revisions are kept unless explicitly purged. Back up **both** database and blobs as one consistent point in time.

```sh
bash ops/backup.sh
# Backups and SHA-256 manifests are written under ./backups
# Copy these OFF THE SERVER to a different disk or cloud account.
```

The script stops Drawer while archiving, then restarts it via a shell trap. That is intentional: copying SQLite's WAL database during active writes and backing up blobs separately can corrupt the logical store.

To restore:

```sh
cp /path/to/drawer-YYYYMMDDTHHMMSSZ.tar.gz{,.sha256} ./backups/
bash ops/restore.sh ./backups/drawer-YYYYMMDDTHHMMSSZ.tar.gz
docker compose exec drawer drawer doctor
```

Restore verifies the selected archive against its checksum, prompts for `RESTORE`, and holds the writer stopped while creating a safety backup and restoring. Backup and restore operations are serialized with a maintenance lock. If restore fails, Drawer stays stopped; inspect the retained `pre-restore-*` directory and safety archive before starting it. **Test recovery on another machine before relying on these backups.**

## Security and limits

- This is a single-admin personal installation, not an organization-wide file access manager.
- Keep `.env` private, use unique high-entropy values, and keep the public `/s/<token>` URLs secret. Anyone who possesses a valid link can download that file until expiry or revocation.
- Browser sessions are stored in RAM and are lost on restart; admin password is supplied from the environment.
- Per-user upload quotas, a ZIP bundle download, and multi-device background sync are outside this version.
- Do not treat old cooperative permissions (with self-declared agent names) as an authentication mechanism; use admin sessions and bearer tokens for all network access.
