# Slash Command Interactions (Option B) — deferred design note

Status: **NOT built.** Deferred by Christian (2026-09-30) — revisit later. Built "Option A"
(message-based bot slash commands) instead. This note documents the full Discord-style
interaction protocol we chose not to build yet.

## What A covers (already shipped/being built)
- Bots register commands; clients show them in `/` autocomplete; sending posts them as a normal
  message the bot parses. No structured interaction round-trip.

## What B would ADD (the gap)
1. **Typed, validated arguments.** Commands declare an option schema: name, type (string/int/bool/
   channel/user/choice), required/optional, min/max, choices, per-option autocomplete. The client
   renders argument UI and validates before send; the bot receives a structured payload
   `{command, options:{key:value}}` instead of parsing free text.
2. **Interaction round-trip & responses.** Backend delivers a structured invocation event to the
   bot (via its websocket or a callback URL) and relays/acks the response. Supports:
   - **Ephemeral responses** — only the invoking user sees the result.
   - **Rich responses** — embeds, follow-ups that can be edited later.
   - **Deferred/async** — bot can ack, do slow work, then reply.
3. **Platform-enforced metadata.** Command name/description/schema are server-side and consistent
   across web/desktop/Android; availability can be permission-gated per server.
4. **Foundation for rich components** (buttons, selects, modals) — which A structurally cannot support.

## Sketch (backend, if/when built)
- **Registration:** `POST/PUT /commands` or bot-management endpoint; body = command name, description,
  option schema, scope (global|server), permissions. Auth = bot token. Stored in Mongo.
- **Types:** DB model `Command` (app/server id, name, description, options[], owner bot id).
- **Invocation:** on client send where content matches a registered command prefix, client posts
  `POST /interactions {command, options, channel, nonce}` (instead of a plain message). Backend
  validates against the schema, emits a `CommandInvoke` event to the owning bot (ws frame), and
  stores a pending interaction keyed by nonce.
- **Response:** bot calls `POST /interactions/{id}/response` with `{type: content|embed|ephemeral}`
  (+ optional later edits). Backend relays to the invoker's channel / as ephemeral to the user.
- **Ephemeral:** client renders it as a locally-scoped message not persisted to channel history.

## Open decisions when revisiting
- Delivery to bots: websocket event frame vs HTTP callback URL (webhooks already exist in Revolt).
- Whether interaction messages are a new message type or a soft overlay on existing messages.
- Permission model (who may invoke, who may register).
- Reuse for Hermes bot as first consumer (dogfood).

## Clients
- Web/desktop: argument picker + validation in the composer; render ephemeral/embed responses.
- Android: argument picker in `MessageField.kt` autocomplete; render responses.
- SDK (`stoat.js`): `Command` type, `registerCommand`, `POST /interactions`, response endpoints.
