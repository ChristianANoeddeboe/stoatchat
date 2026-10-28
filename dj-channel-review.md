# DJ Channel — adversarial design review

Reviewed: `backend/dj-channel.md` (v1, commit a1b76437) against `feat/forums` backend, `android/`, `web/`, `sdk/`, `desktop/`.

## Overall verdict

**Do not implement as written.** v1 is a feature sketch with three of its five "locked decisions" resting on
mechanisms that do not exist in this codebase (per-channel membership, range-servable audio, a single-owner
playback worker). The two genuinely hard problems — clock/position sync and the file lifecycle — are asserted
rather than designed, and several rules combine into silent data destruction (delete-all on empty, delete on
play, crash ⇒ delete everything, dangling-file sweeper) with no undo.

The good parts: MP3-only transcode removes the codec matrix, FIFO-only removes ordering debates, one-shot
ephemeral queue removes history/pagination. Keep those.

Effort estimate is wrong by a wide margin. Backend + SDK + web + desktop + Android "in one pass" is
realistically 4–6× the sketch, mostly because the web client has *no* audio playback code today and the
Android client has no background-playback infrastructure.

---

# BLOCKERS

## B1. "Joined member" does not exist, and the whole feature keys off it

**Problem.** §6/§5/§3 all key off "joined members" and §8 says "Member count: existing channel-membership
pathway". There is no such pathway for server channels. `crates/delta/src/routes/channels/mod.rs` registers
`join_call` (voice) and `thread_members` only; `crates/core/models/src/v0/channels.rs:14` has no membership
field for `TextChannel`/`ForumChannel`. Server channels are visible to every server member; there is nothing
to join.

**Why it matters.** The vote denominator, the 3-track cap, `DJ.MemberJoined/Left`, and the destructive
"zero members ⇒ delete every queued file" rule all depend on a quantity that has no definition, no storage,
and no lifecycle. Implementations will each invent one and drift.

**Fix.** Define membership as an explicit lease, mirroring voice but without its bug: Redis set
`dj_members:{channel_id}` (cf. `crates/core/database/src/voice/mod.rs:341`) written by
`POST /channels/{id}/dj/join` and deleted by `POST .../leave`, with a **TTL'd heartbeat** (30 s beat,
90 s expiry) because the voice set has no expiry and goes stale when webhooks are missed. Never derive
membership from "channel is open in a tab" — see B3/H8.

## B2. Range requests cannot be served by the existing file layer

**Problem.** Files are encrypted with AES-256-GCM over the *whole* object
(`crates/core/files/src/implementation/encryption_impl.rs:43-56`). The read path downloads the entire object
and decrypts it into a `Vec<u8>` (`s3_impl.rs:60-90`, `crates/services/autumn/src/api.rs:85-93`), cached in a
2 GiB, 5-minute moka cache (`api.rs:68-80`). `fetch_file` sets `Content-Disposition: attachment` and returns
the full body with no `Accept-Ranges`/`Content-Range` (`api.rs:464-505`). There is no range support anywhere
in the stack.

**Why it matters.** `GET /dj/{channelId}/stream` "range-request-capable" is not an endpoint you can add on
top of this. A seek = full download + full decrypt + full re-encrypt-free copy per request; a 50 MB track per
listener per seek; 2 GiB of decrypted audio resident. AEAD makes partial decryption impossible by design.
Both ExoPlayer and browser media elements issue Range probes and rely on 206 for seeking; without it the
`at`-timestamp seek in §4 and the mid-song join in §7 are dead.

**Fix.** DJ blobs go to object storage **unencrypted** (dedicated bucket or `dj_track` tag with encryption
skipped) and are served by a real byte-range handler: `get_object().range("bytes=a-b")`, correct
`Content-Length: end-start+1`, `Content-Range`, `Accept-Ranges: bytes`, `206`. Do not proxy through autumn's
decrypt+cache path. If encryption is non-negotiable, chunk the object into fixed-size CBC/CTR blocks — which
is a storage-format change, not a route change. Decide now; it changes the data path.

## B3. "Delete after play" is unsafe by construction

**Problem.** §3: `TrackEnded` ⇒ broadcast ⇒ delete file ⇒ advance. §7 repeats it for skip and clear.
Clients buffer ahead by seconds to tens of seconds; the skip vote can fire while a listener is 15 s behind.
Additionally: content-hash dedupe (`autumn/src/api.rs:251-282`) means two queue rows can share one blob;
`file_deletion.rs` only deletes the object when `count_file_hash_references(hash) <= 1`
(`crates/daemons/crond/src/tasks/file_deletion.rs`). And a mid-song joiner streams a track that is already
past the halfway point of its file — there is nothing left to read if it was deleted a minute later.

**Why it matters.** Every delete is a race with an in-flight reader, and the failure mode is a truncated
stream or 404 mid-song for the *slowest* client — i.e. the one on mobile data. Combined with B2 (whole-file
fetch), a delete during a fetch can kill a listener who was already buffering.

**Fix.** Never delete synchronously in the advance path. On advance: mark the queue row played, set a
`delete_after = now + 60s` (or `max(buffer, 30s)` after the track's last known end), and let a reaper handle
it. When deleting, use the existing reference-counted path so shared blobs survive. Also decide the intended
semantics for the contradiction below.

**Contradiction to resolve:** §3 says "Requeues allowed; a track that just played can't be re-added until it
leaves the buffer", but §7 deletes the file after play. After deletion, re-adding the same track requires a
fresh upload — and if the user re-uploads, hash dedupe will resurrect the *deleted* hash row. Either the blob
lives until the track leaves the queue (and "delete after play" means "after it leaves the queue"), or drop
requeue from v1.

## B4. A new `channel_type` variant breaks already-installed Android clients

**Problem.** `android/core/model/.../Channel.kt:258-268` deserializes `ChannelType` with an exhaustive `when`
that ends in `else -> throw IllegalArgumentException("ChannelType could not parse: $value")`. `Channel` is
part of the `Ready` payload. A single `DJ` channel in any server the user belongs to therefore fails
deserialization of the whole `Ready` frame — the released app cannot complete login / loses its state.

Second, `Channel::server()` in `crates/core/models/src/v0/channels.rs:731-740` matches with `_ => None`.
A `DJ` variant added there but not to that arm returns `None`, which reaches
`DatabasePermissionQuery::set_server_from_channel` → `unimplemented!()`
(`crates/core/database/src/util/permissions.rs:379`). That is a panic, not a compile error.

**Why it matters.** You cannot roll this out to an already-shipped app without a hard cutover. And the
codebase's own precedent for "a channel that behaves differently" is *not* a new variant: voice is
`Channel::TextChannel { voice: Option<VoiceInformation> }` (`channels.rs:113-115`) — Android's
`ChannelType.VoiceChannel` does not even exist in the backend enum. Forums got a variant because they carry
different child semantics (posts/threads/tags). DJ has no children.

**Fix.** Model DJ as `TextChannel` + `dj: Option<DjInformation>` capability field (`skip_threshold`,
`max_file_mb`, `max_track_seconds`), exactly like `voice`. Zero client-enum churn, no shipped-client breakage,
permissions and `server()` keep working, and the composer can be suppressed by the same field. If a variant is
insisted on: ship an `Unknown` fallback in Android first, in a released build, and wait a release cycle.

## B5. No owner, no store, no notification path for the queue

**Problem.** §3 says a worker "owns" the FIFO queue, but §8 has the API accept uploads. Two writers, one
undefined store. There is no `dj_queue` collection, no `DjState`, no model. Events fan out only through Redis
topics (`EventV1::p/p_user/private/server/global`, `crates/core/database/src/events/client.rs:409-447`);
RabbitMQ is used for push notifications only. And the cited skeleton is wrong: `crates/daemons/crond/src/main.rs`
runs *stateless infinite loops with 60 s sleeps* and has **no singleton guarantee** — two replicas double-play
every track and double-delete every blob. `pushd` is event-driven but has no persistent state.

**Why it matters.** "Worker owns active track + position + queue" is a distributed-systems design, not a
cron job. Without a single-writer rule, concurrent enqueue vs. advance is unsolvable; without a durable store,
API and worker desync on every restart — and the spec's answer to desync is "delete everything", which is why
B3/H8 hurt so much.

**Fix.** Mongo collection `dj_queue` as the single source of truth (`_id` = ulid, `channel`, `position` seq,
`file`, `title`, `duration_ms`, `uploader`, `state`), with atomic `findOneAndUpdate` for head advance and for
the cap check. Worker = single-replica daemon holding a Redis lease (`dj_worker:{channel}:lease`, 10 s renew)
so a stale instance can never act; it polls the collection (or subscribes to a durable RabbitMQ queue) and
publishes events. Per-channel monotonic `play_seq` incremented on every transition. API is the only writer of
uploads; the worker is the only writer of `state`/`position`. If the lease is lost, the worker stops advancing
— it does not "recover" by clearing the queue.

---

# HIGH

## H1. Events go to everyone who can view the channel, not to "joined" clients

`crates/bonfire/src/events/impl.rs:344-346` subscribes every session, at Ready, to a Redis topic per
*viewable* channel id. Anything published to `{channel_id}` therefore reaches every server member with
`ViewChannel`, not the joined set. DJ events would leak queue contents and stream URLs to lurkers and give
non-joined clients a phantom player.
**Fix.** Publish per-track events to private user topics (`EventV1::private(user_id)`, `client.rs:435`) for
each joined member, from an explicit member set the worker maintains; publish only `DJ.TrackQueued`-lite
(meta, no URL) to the channel topic if you want non-joined members to see activity.

## H2. No state snapshot on connect, join, or reconnect

There is no `GET /dj/{id}/state` in §8 and no `dj_states` in `Ready`. A joiner gets `DJ.NowPlaying` but not
the queue, the votes, the member list, or its own vote. A reconnecting client that missed `TrackStarted` sits
silent indefinitely; a client whose socket stalled for 30 s has no resync path.
**Fix.** Copy the voice precedent: `voice_states` is in the Ready payload
(`EventV1::Ready`, `client.rs:84-86`) and hydrated client-side (`sdk/src/events/v1.ts:329-348`). Add
`dj_states: Option<Vec<DjChannelState>>` to Ready, plus `GET /channels/{id}/dj` returning
`{current, position_ms, at, play_seq, queue, votes, voters, members, settings}`. Make `POST .../dj/join`
return the same body. Send `DJ.NowPlaying` as a heartbeat every 15 s, not only on join.

## H3. `at` alone cannot sync clocks

§4: "all clients derive position from server `at` timestamp". Clients would compute `now_local - at`, which
uses the *client's* wall clock. A device 3 minutes fast seeks 3 minutes into the track; a suspended laptop
resumes with a frozen timeline. No RTT handling, no drift correction, no monotonic reference.
**Fix.** Emit `position_ms` **at emit time** (not just `at`), plus `server_time`; the client measures RTT on
the websocket ping it already has, then advances with a monotonic clock from the moment of receipt. Re-sync
on every `NowPlaying` heartbeat; seek only when `|drift| > 500 ms`; clamp to `[0, duration]`.

## H4. Skip-vote arithmetic is wrong at both edges

`votes >= ceil(members × threshold)`:
- `threshold = 0` ⇒ `votes >= 0` is true immediately ⇒ every track skips on arrival, the queue drains, and
  every file is deleted. `members = 0` produces the same `needed = 0`.
- `threshold = 1` is described as "effectively never" but a unanimous vote still skips.
- The denominator moves. If the tally is only re-evaluated on vote-cast, a member leaving can push a stale
  count over a now-lower threshold (skipping a track nobody new voted against), while a member joining
  mid-track can un-trigger a skip that already fired.
- Departed voters keep their votes.
**Fix.** `needed = max(1, ceil(present_voters × threshold))`; special-case `threshold == 0` as
"1 vote skips"; evaluate on vote-cast **and** on member join/leave; drop votes belonging to users no longer
in the member set at evaluation time. Rename the `1` case to "unanimous" in the spec.

## H5. Vote identity and double-vote prevention are undefined

"one vote per active track" needs a `(track_id, user_id)` key. Events carry only counts
(`DJ.Vote {for,total,needed}`), so a client that reconnects or joins mid-track cannot know whether it already
voted, and cannot render the vote state at all.
**Fix.** Redis set `dj_votes:{track_id}` (deleted on advance); include `voters` in the state snapshot;
POST skip is idempotent per `(track, user)`; the tallied `needed` in every `DJ.Vote` is computed from the
current member set, never cached.

## H6. The upload path as specified does not exist

- `Tag` (`autumn/src/api.rs:126-133`) has no `dj_track`; size limits are a per-tag `HashMap` lookup ending in
  `.expect("size limit")` (`api.rs:206-210`) ⇒ **panic** for any user tier missing the new key.
- Non-image uploads are rejected for every tag except `attachments` (`api.rs:246-249`).
- A file with no `used_for` 404s on fetch (`api.rs:475-478`) and is marked deleted by
  `prune_dangling_files` 60 minutes after upload (`crates/daemons/crond/src/tasks/prune_dangling_files.rs`,
  `files/ops/mongodb.rs:51-65`).

**Why it matters.** That last point is a data-loss bug waiting to happen: a track queued behind a long queue
gets reaped before it plays, and the "orphan sweep" in §7 will actively delete queued-but-unplayed blobs.
`find_and_use_attachment` (`files/ops/mongodb.rs:80-120`) is also a non-atomic read-then-write with no
uploader check — do not copy it for the queue cap.
**Fix.** New tag + config limits for every tier (fail closed, no `expect`), a `FileUsedForType::DjTrack`
variant bound at enqueue time in one atomic update, and `fetch_dangling_files` must exclude `dj_track`.
Reuse the existing `IdempotencyKey` for `POST .../dj/queue` (note: it is a process-local LRU of 1000 —
`crates/core/database/src/util/idempotency.rs` — so the real dedupe must be a DB unique index on
`(channel, uploader, upload_nonce)`).

## H7. ffmpeg is in the wrong containers, and there is no transcode policy

`ffmpeg`/`ffprobe` are installed only in autumn and january (`crates/services/autumn/Dockerfile:8-9`);
`crates/delta/Dockerfile` is distroless with no binaries, and the shared `files` crate only shells out to
ffprobe for *video* dimensions (`media_impl.rs:189`). Spec silences: concurrency, timeouts, CPU limits,
probe failure, non-audio input, video files, absurd durations, output duration mismatch, and "ffmpeg missing".
**Fix.** Transcode in the worker (not the API), bounded to N=2–4 concurrent jobs with a hard timeout and
`-vn -t <cap>`; probe the *output* MP3 for the authoritative duration; reject when probe fails or duration
exceeds the cap; delete the source blob on any transcode failure; add an `ffmpeg -version` startup check that
fails the daemon loudly rather than accepting uploads it can't process.

## H8. "Zero members ⇒ delete everything" is a permanent-data-loss footgun

A websocket blip, app crash, laptop sleep, or 20 seconds where everyone closes the tab permanently destroys
every upload in the queue — files the users spent bandwidth to transcode. There is no undo and no grace.
**Fix.** Grace period ≥ 120 s before the count is treated as zero; on empty, *pause* and emit `DJ.Paused`,
then only `QueueCleared` + deletion after a longer TTL (e.g. 1 h) or explicit admin action. Given §7 already
makes crash ⇒ wipe, do not have two independent wipe paths.

## H9. Web autoplay will break the core "join and hear music" flow

There is **no** audio playback in the web client today: the only occurrence of "audio" in
`packages/client/src` is a mock device name in `Development.tsx`; voice audio comes entirely from
`packages/solid-livekit-components`. Browsers block `HTMLMediaElement.play()` without a user gesture
(Chrome gates on interaction/MEI, Safari is stricter and per-element). A `DJ.TrackStarted` handler that calls
`play()` will reject with `NotAllowedError`.
**Fix.** Unlock during the join click: create/resume the element synchronously in the gesture handler, then
`seekTo(position)` on the already-unlocked element; keep a "tap to enable sound" state for the rejected case;
never start playback from an event handler as the first sound. On desktop (Electron Forge shell over the same
web build, `desktop/forge.config.ts`) set `autoplayPolicy` explicitly so behavior is known rather than
inherited. Spec must own this, not discover it.

---

# MEDIUM

**M1. "No message posting" is not a property of the channel type.** `message_send.rs:41-44` special-cases
only `ForumChannel`. Reactions, pins, typing, webhooks, acks/unreads, message search and invites would all
happily accept a DJ channel. Add explicit guards (or a shared `resolve_postable_channel()` helper) or DJ
channels will silently accumulate messages no client renders.

**M2. Per-channel settings leak across types.** `PartialChannel`/`DataEditChannel` are single flat structs
shared by every channel type (`channels.rs:332-389`, `:406+`). Adding `skip_threshold`/`max_file_mb` there
makes them PATCH-able on a text channel and included in audit-log `before/after` diffs
(`audit_logs.rs:57-58`). Validate per type in the handler, and gate on `ManageChannel`.

**M3. No global storage or duration ceiling.** Cap is per-member: 3 × `max_file_mb` × members. A 500-member
server = 75 GB of MinIO for tracks that may never play; N throwaway accounts trivially fill the disk. Deletion
only happens after playback, so a never-played tail occupies disk indefinitely. Add per-channel max tracks and
max bytes, a per-server quota, a max total queue duration (e.g. 4 h), and rate-limit the queue endpoint with
`crates/core/ratelimits`.

**M4. No fairness.** Strict FIFO + 3/member lets one member park three 40-minute mixes ahead of everyone, and
there is no duration cap, so the only escape is the skip vote. Cap track duration (e.g. 15 min) and/or
interleave per uploader.

**M5. Stream auth is unspecified and the clients can't send headers.** Delta routes require the `User` guard;
`<audio src>` and `ExoPlayer(MediaItem.fromUri)` send no `Authorization` header — the existing
`AudioPlayer.kt:68` builds a default-data-source ExoPlayer (media3-okhttp is available at
`gradle/libs.versions.toml:88` but unused). So the stream is either public (anyone with the channel id can
download and keep every track; "delete after play" only governs your copy) or needs a signed, short-TTL,
per-track token in the query string. Decide, and make token leakage impossible by not sending URLs to
non-joined clients (see H1).

**M6. Events have no ordering or idempotency.** Redis pubsub per channel, no sequence numbers; concurrent
publishers can interleave `TrackStarted`/`Skipped`/`TrackEnded` and clients will apply stale state (a client
that gets `TrackStarted` for track N+1 before `TrackEnded` for N shows the wrong track). Carry `play_seq` in
every DJ event and drop events with `seq < current`; make advance idempotent via `findOneAndUpdate` on the
head with an expected seq.

**M7. Crash recovery enumeration is racy.** "On restart, delete every DJ blob" requires enumerating blobs
(Mongo by tag, or bucket listing) while the API keeps accepting uploads. Clear too eagerly and you delete
files for enqueues that arrived during restart; too lazily and you leak orphans. Use a generation counter:
worker startup deletes only rows with `epoch < current_epoch` and bumps the epoch atomically.

**M8. `skip_timeout` contradicts "natural end wins".** With a correct duration these are the same clock;
with a bad duration one of them cuts tracks short or hangs forever. Keep exactly one clock: end =
`started_at + duration_ms`, with the worker as the only authority and a +5 s safety margin. Delete
`skip_timeout` from v1.

---

# LOW

- **L1.** `DJ.Vote {for,total,needed}` — `for` is a Rust keyword; needs `r#for`/`#[serde(rename)]`. Schema
  wasn't checked against `EventV1`'s `#[serde(tag = "type")]` conventions.
- **L2.** `max_file_mb` duplicates `config.file_upload_size_limit` (a per-tag HashMap). One source of truth;
  per-channel value must be clamped ≤ tier limit.
- **L3.** §"Files stored locally on the server" is wrong for this stack — everything is MinIO/S3 via autumn
  (`compose.yml` minio, `files/src/implementation/s3_impl.rs`). Anyone implementing GC from this spec will
  look for files on the wrong disk.
- **L4.** Moderation/licensing hooks already exist and are unused: ClamAV scanning (`autumn/src/clamav.rs`),
  file report/takedown, `reported` flag. DJ blobs should be scanned before transcode and reportable;
  a per-server "disable DJ" toggle is cheap insurance.
- **L5.** Android: "singleton per DJ channel" ignores leaving the channel (stop or keep playing?), joining
  two DJ channels, audio focus/ducking, and upload picker constraints.
- **L6.** Desktop is a thin Electron shell over the web build — count it as "web plus one config flag",
  not a separate client workstream.

---

# Must decide before implementation (top 5)

1. **Channel modelling: capability field vs. new enum variant.** A `DJ` variant hard-breaks every shipped
   Android client (`Channel.kt:267` throws) and needs a staged rollout; `TextChannel + dj: Option<..>`
   matches the existing voice precedent and costs nothing. Decide before any client work starts.
2. **What "joined" means and how it expires.** Explicit lease with heartbeat + 120 s grace, or the destructive
   empty-channel rule will fire on network blips and delete users' uploads.
3. **Streaming storage design.** Unencrypted, range-capable objects + real 206 + signed per-track URL (or
   header auth with media3 okhttp). AEAD-whole-file storage and seeking cannot coexist.
4. **Queue/engine ownership.** Durable Mongo queue, atomic head advance, single-worker lease, `play_seq` on
   every event, per-user event targeting. "A cron loop that owns state" is not implementable here.
5. **Transcode placement and limits.** Bounded ffmpeg worker with duration/size caps, probe-failure policy,
   and startup availability check — plus the delete-after-play delay (60 s) replacing synchronous deletion.

**Also fix before v1 ships:** the skip-math edges (`threshold=0` ⇒ instant skip loop, `members=0` ⇒
`needed=0`), and the `prune_dangling_files` interaction that would delete queued-but-unplayed tracks.
