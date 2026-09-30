# Slash commands — Option A implementation plan (backend-first)

Status: IMPLEMENTATION IN PROGRESS (fresh agent session). Runtime context:
live instance https://stoat.noddeboe.dk, self-hosted fork across 5 GitHub repos (work branches).

## Goal (Option A — message-based bot slash commands)
Bots register `/commands` server-side; clients fetch them per server and merge into the `/`
autocomplete; executing just sends `/command args` as a normal message the bot parses. NO
interaction round-trip (that's Option B, deferred — see `slash-commands-interactions-B.md`).

## Backend (cr start here — MUST compile with `cargo check`)
Add a `server_commands` model + `/commands` routes in the `stoatchat` (backend) fork,
branch `feat/forums`, repo at `~/stoat/backend`.

### Model — `Command` (mirror `channel_webhooks` model exactly)
Fields: `id` (serde `_id`), `name: String` (no leading slash), `description: String`,
`server_id: Option<String>` (None = global), `owner_id: String` (bot user id).
Files to create under `crates/core/database/src/models/server_commands/`:
- `model.rs` — `auto_derived_partial!(struct Command {...}, "PartialCommand")` + `auto_derived!(enum FieldsCommand { Name, Description })` + a `Default` impl (copy the webhooks model pattern). Add `create`/`fetch` convenience methods if needed.
- `ops.rs` — `AbstractCommands` trait: `insert_command`, `fetch_command(id)`, `fetch_commands_for_server(server_id) -> Result<Vec<Command>>`, `update_command(id, &PartialCommand, &[FieldsCommand])`, `delete_command(id)`.
- `ops/mongodb.rs` — `impl AbstractCommands for MongoDb`, `COL = "server_commands"`, using `query!(...)` + `self.col::<Command>(COL).find(doc!{"server_id": server_id})` for fetch_by_server (copy webhooks mongodb.rs).
- `ops/reference.rs` — `impl AbstractCommands for ReferenceDb` using a `self.server_commands` Mutex<HashMap> (copy webhooks reference.rs).
- `mod.rs` — declare `mod ops;` (+ re-export).
Wiring:
- `crates/core/database/src/drivers/reference.rs` (line ~21): add field
  `pub server_commands: Arc<Mutex<HashMap<String, Command>>>` (add `Command` to the `use crate::{...}`).
- `crates/core/database/src/models/mod.rs`: `mod server_commands;` (alphabetical), `pub use server_commands::*;`, and add `+ server_commands::AbstractCommands` to the `AbstractDatabase` trait bound.
- `crates/core/database/src/models/admin_migrations/ops/mongodb/init.rs` (~line 51): add
  `db.create_collection("server_commands")` for fresh installs (existing DBs auto-create on insert).

### Routes — `crates/delta/src/routes/commands/`
Files: `mod.rs`, `create_command.rs`, `fetch_commands.rs`, `delete_command.rs`.
- `mod.rs` — `pub fn routes() -> (Vec<Route>, OpenApi)` via `openapi_get_routes_spec![...]`.
- Register in `crates/delta/src/routes/mod.rs`: add `mod commands;` and `"/commands" => commands::routes(),`.
- `create_command` — `#[put("/<server_id>/<command_name>")]`, auth = **bot** (extract bot from session/bot token like webhook routes / bot routes do; only a bot/user with ManageServer or the bot owner may register). Body: `{description?, ...}`. Upsert by (server_id, name): if exists and owned by same bot → update; else insert. Generate id via `ULID` (see how other models create ids).
- `fetch_commands` — `#[get("/<server_id>")]`, any authenticated `User`; returns `Vec<Command>` for that server (for autocomplete).
- `delete_command` — `#[delete("/<server_id>/<command_name>")]`, bot-owner only.

### Verify
- `cargo check -p revolt-database` then `cargo check -p revolt-delta` (Rust toolchain pinned in `rust-toolchain.toml`). Fix all errors; do NOT leave the tree half-wired.
- `cargo fmt` the new files, commit on `feat/forums`:
  `feat(backend): bot slash command registration (model + routes)` and push.

## SDK — `stoat.js` (repo ~/stoat/sdk, branch feat/forums)
Add `Command` model type, `createCommand`/`fetchServerCommands`/`deleteCommand` methods (copy an existing API class pattern), and export.

## Web client (repo ~/stoat/web, branch feat/mobile-settings)
- `git submodule update --init --recursive` FIRST (else lingui build fails misleadingly).
- Fetch server commands via the SDK client (per current channel server) and MERGE into the existing
  slash autocomplete: `codeMirrorSlashCommands.ts` registry + `codeMirrorAutoCompleteSource.ts`
  (`case "/"`). Keep the built-in `SLASH_COMMANDS` as a fallback merged with server commands.
- Typecheck via standalone tsc (see `stoat-fork-development` skill `references/testing.md`); build image `stoat-web:...`.

## Android (repo ~/stoat/android, branch feat/self-hosted-instance)
- Merge server commands into the composer autocomplete: `MessageField.kt` (`AutocompleteSuggestion.Command` + trigger) and `internals/Autocomplete.kt` `command(query)` — make the list come from the SDK/API fetch for the current channel's server, falling back to built-ins.
- Verify `:app:compileReleaseKotlin` (SDK prereqs in `~/stoat/android` local.properties/stoatbuild.properties/real google-services.json).

## Testing (user does runtime click-through; agent may do headless-Chrome-CDP if practical)
Test creds in `~/.bashrc` (`STOAT_TEST_EMAIL`/`STOAT_TEST_PASSWORD`). Browser-CDP automation recipe + gitignored build prereqs are in the `stoat-fork-dev` / `stoat-fork-development` skills. Deploy is a separate concern — do NOT deploy; just implement + compile-verify + commit/push.

## Deploy note
Web: build `stoat-web` image, push `registry.noddeboe.dk`, update `compose.yml`, recreate `stoat-web-1` on `tower`. Desktop auto-gets web. Android: signed APK release flow (see `stoat-fork-dev`). Backend: deploy forked `api`/`events` images. (Do not perform unless asked.)
