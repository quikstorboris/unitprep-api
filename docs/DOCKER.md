# UnitPrep Docker Commands

Run everything in a WSL terminal. API commands use `~/Development/unitprep-api`, UI commands use `~/Development/unitprep-ui`. Use a separate terminal for anything marked "stays open".

Everything is behind the `dev` compose profile, so a bare `docker compose up` does nothing by accident.

## Start everything

**1. API containers** (`test-db` and `api-dev`):

```bash
cd ~/Development/unitprep-api && docker compose --profile dev up -d
```

**2. API server** (stays open: the server runs in this terminal):

```bash
cd ~/Development/unitprep-api && docker compose exec api-dev cargo run
```

**3. UI:**

```bash
cd ~/Development/unitprep-ui && docker compose --profile dev up -d
```

- UI: http://localhost:3001 (not 3000)
- API: http://localhost:8080, once step 2 is running
- The UI's first start runs `npm ci` and Next's first compile, so allow a minute.

## Restart the API after a code change

`cargo run` does not reload on save. In the terminal running step 2, press Ctrl-C, then run step 2 again.

If it says port 8080 is already in use, an old `cargo run` is still alive inside the container:

```bash
cd ~/Development/unitprep-api && docker compose up -d --force-recreate api-dev
```

Then run step 2 again (recreating wipes anything started by hand). The image has no `pkill`.

## Stop everything

```bash
cd ~/Development/unitprep-ui && docker compose --profile dev down
```

```bash
cd ~/Development/unitprep-api && docker compose --profile dev down
```

`down` removes the containers but keeps the caches (node_modules, .next, cargo registry, build output), so the next start is fast. Use `stop` instead of `down` to pause without removing.

A WSL or Docker restart stops everything on its own.

## Live logs

UI: Next.js compiling on every save:

```bash
cd ~/Development/unitprep-ui && docker compose logs -f ui-dev
```

API: the test-watch loop, which reruns the whole test suite on every Rust save:

```bash
cd ~/Development/unitprep-api && docker compose logs -f api-dev
```

- Ctrl-C stops watching, not the container.
- Add `--tail 50` to start from the last 50 lines.
- The API server's own request log prints in the terminal running step 2, not here.

## Status and extras

| Need | Command |
|---|---|
| What's running | `docker ps` (empty = nothing; add `-a` for stopped too) |
| Shell in the API container | `cd ~/Development/unitprep-api && docker compose exec api-dev bash` |
| Shell in the UI container | `cd ~/Development/unitprep-ui && docker compose exec ui-dev bash` |
| Real-DB `#[ignore]`d test against the throwaway DB | `docker compose exec api-dev cargo test -- --ignored <name>` (run `./scripts/bootstrap_test_db.sh` first on a fresh test-db) |
| Stop one container by name, from anywhere | `docker stop unitprep-ui-ui-dev-1` |

## Gotchas

- **Which database does `cargo run` use?** The compose file only sets `TEST_DATABASE_URL` (the throwaway test-db). The server probably reads `.env.local` through the bind mount and talks to your real Neon dev branch. Not verified.
- **Docker is a bit slower than native.** Port forwarding, Next file watching and debug builds all add a little. Expected.
- **PowerShell commands don't run in the WSL shell**, and the other way round.
