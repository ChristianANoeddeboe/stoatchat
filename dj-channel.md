# DJ Channel — Feature Spec v2

A new **DJ channel capability**: members explicitly join a channel, do not post
messages, and hear a shared FIFO music queue streamed to them. Anyone joined can
queue a music file; files live on object storage and are reaped after they leave
the queue to save space.

v2 rewrites v1 after an adversarial review against the actual codebase. v1 keyed
on mechanisms that do not exist (per-channel membership, range-servable audio, a
single-owner worker) and combined several rules into silent data loss. Those are
fixed here.

---

## 0. Changes from v1 (and the one locked decision that moved)

| Area | v1 | v2 |
|---|---|---|
| Channel model | new `DJ` channel_type variant | `TextChannel` + `dj: Option<DjInformation>` capability field (voice precedent) |
| Membership | "joined members", no mechanism | voice-style join/leave + TTL heartbeat (30 s / 90 s) |
| Streaming | reuse file layer, range requests | dedicated unencrypted range-capable DJ blobs + 206 handler + signed URL |
| Queue owner | a worker, undefined store | Mongo `dj_queue` (durable) + single-replica worker with Redis lease + `play_seq` |
| Delete on play | synchronous delete | mark played, `delete_after = now + 60 s`, reference-counted reaper |
| Crash recovery | clear queue + delete all blobs | **resume from durable state** (no wipe) |
| Requeue after play | allowed | not allowed in v1; re-upload to replay |
| Skip math | `votes >= ceil(members × threshold)` | `max(1, ceil(present_voters × threshold))` + threshold-0 = 1 vote |
| Settings | flat channel fields | dedicated `PATCH /dj/settings`, `ManageChannel` gated |
| Android background audio | assumed free | **foreground-only in v1**; no session service |
| Storage location | "locally on the server" | MinIO/S3 via autumn (unencrypted DJ bucket) |

**Decision that moved: crash recovery.** v1 said "clear everything on restart."
That was compensating for an ephemeral queue. With a durable Mongo queue it is
unnecessary and destroys user uploads on any worker restart. v2 resumes instead.
If you still want crash-wipe, say so and it goes back in, but it is now a single
line, not the load-bearing recovery design.

**Channel model re-confirmed:** DJ stays a `TextChannel` with a `dj` option, not
a new `channel_type`. Joining is a voice-style explicit action (see §2). This
keeps shipped Android clients working (no enum parse throw).

---

## 1. Channel model

DJ is a **capability on a text channel**, not a new `channel_type` enum value.

```rust
Channel::TextChannel {
    // existing fields
    voice: Option<VoiceInformation>,
    dj: Option<DjInformation>,     // new
}
```

`DjInformation` (per-channel config):

```rust
struct DjInformation {
    skip_threshold: f32,        // 0.0..=1.0, default 0.5
    max_file_mb: u32,           // per-upload, clamped to the uploader's tier limit
    max_track_seconds: u32,     // default 900 (15 min)
    max_tracks: u32,            // total queue depth, default 100
    max_total_bytes: u64,       // per-channel storage ceiling, default 20 GiB
    max_users: Option<u32>,     // optional join cap, like voice_info.max_users
    enabled: bool,              // channel on/off (server admins can also disable)
}
```

Why not a variant:
- `core/model/.../Channel.kt:267` (Android) throws on unknown `ChannelType`
  inside the `Ready` payload. A new variant breaks every shipped app on login.
- `Channel::server()` returns `None` for unhandled variants, reaching
  `unimplemented!()` in `permissions.rs:379` (a runtime panic).
- Voice already models "behaves differently" as an optional field on
  `TextChannel`. DJ has no child semantics (unlike forums), so it needs no
  variant.

Consequences:
- Old clients see a normal text channel. The composer is present but the server
  rejects posts with a clear `CannotSendMessageToDjChannel` error. Acceptable
  degradation; no crash.
- `channel_type` stays `TextChannel`. Clients detect DJ via `dj != null`.

### Posting guards

DJ channels are read-only for messages. Add a shared
`resolve_postable_channel(channel)` helper used by: message send/edit/delete,
reactions, pins, typing indicators, webhooks, and search indexing. Threads and
forum posts cannot be created in a DJ channel. Acks/unreads remain allowed.

### Settings

- `PATCH /channels/{id}/dj/settings`, gated on `ManageChannel`.
- Do **not** widen `PartialChannel`/`DataEditChannel` with DJ fields; that would
  make them PATCH-able on every channel type and leak into audit-log before/after
  diffs. Validate per type if any field must appear there.

---

## 2. Membership ("joined", voice-style)

DJ is a **TextChannel with a `dj` option** (`channel_type` stays `TextChannel`),
but it is *joined* the way a voice channel is: the sidebar shows a joinable
channel, joining is an explicit action, and the joined set is visible.

- `POST /channels/{id}/dj/join` mirrors `join_call` semantics: requires the
  `Connect` channel permission, enforces `dj.max_users` if set, adds the user to
  the live set, and returns the full DJ state.
- `POST /channels/{id}/dj/leave` removes the user and stops playback for them.
- Membership lives in Redis `dj_members:{channel_id}`, **with a TTL**. Voice's
  `vc_members` set has no expiry and goes stale; do not copy that bug.
- Client heartbeats every **30 s** (piggyback on the existing websocket ping or
  `POST .../dj/heartbeat`); entries expire after **90 s**.
- A backgrounded tab that stops heartbeating expires. "Joined" is explicit and
  expiring, never "channel is visible".
- `dj_states: Option<Vec<DjChannelState>>` in the `Ready` payload (mirrors
  `voice_states`), so reconnects know which DJ channels the user is in.
- Count = cardinality of the live set. Never derive it from `ViewChannel` or
  open tabs.

Events: `DJ.MemberJoined {user, count}`, `DJ.MemberLeft {user, count}`.

**Sidebar/UX:** a TextChannel with `dj != null` renders as a joinable DJ channel
(headphones badge, member count), not a normal text channel. Opening it without
joining shows the queue read-only with a Join button; joining unlocks
upload/skip.

---

## 3. Queue model (durable, single-writer)

Mongo collection `dj_queue`:

```text
_id            ulid
channel        channel id
seq            monotonic per channel (from a dj_channel_state counter doc)
file           file id (unencrypted dj blob)
title          string
duration_ms    int (authoritative, probed from the transcoded output)
uploader       user id
state          queued | playing | played | deleted
started_at     timestamp | null
position_ms    int (persisted every 5 s while playing)
delete_after   timestamp | null
created_at     timestamp
```

`dj_channel_state`:

```text
channel, epoch, play_seq, empty_since, worker_lease_holder
```

**Writer rules**
- API is the only writer of uploads/queue inserts.
- The worker is the only writer of `state`, `started_at`, `position_ms`,
  `delete_after`, and `play_seq`.
- **Single owner:** the worker holds a Redis lease `dj_worker:{channel}:lease`
  (10 s TTL, renewed every 3 s). Two replicas can never both advance.
- If the lease is lost the worker stops advancing. It does not clear state.

**Enqueue (atomic)**
1. Check `count(queued|playing for channel, uploader) < 3`.
2. Check `max_tracks`, `max_total_bytes`, per-server quota, and rate limit.
3. Insert with `seq = inc(dj_channel_state.counter)`.
All in one transaction.

A track may be queued multiple times before it plays (each row references the
same hash-deduped blob; the existing reference count protects it). A track that
has already played cannot be requeued; upload again to replay.

---

## 4. Audio pipeline

### Upload → transcode → enqueue

1. `POST /channels/{id}/dj/queue` (multipart, `IdempotencyKey`).
2. Write to a temp object; do **not** bind it as an attachment.
3. Worker transcodes with ffmpeg: `-vn -t <max_track_seconds>`, MP3, bounded
   concurrency (N = 2-4), hard timeout per job.
4. Probe the **output** MP3 with ffprobe for the authoritative duration; reject
   if the probe fails, the output is empty, or duration exceeds the cap.
5. Store the MP3 as an **unencrypted** DJ blob under the `dj_track` tag.
6. Bind `FileUsedForType::DjTrack` in the same atomic update that inserts the
   queue row.
7. Delete the source temp blob on success or failure.
8. Notify the uploader on failure (`DJ.UploadFailed {reason}`).

Policy the v1 spec omitted and v2 fixes:
- ffmpeg lives only in autumn/january today; add it to the worker image and do
  an `ffmpeg -version` startup check that fails loudly.
- New `Tag::dj_track` + per-tier size limits for **every** tier (fail closed; the
  current `.expect("size limit")` panics).
- `prune_dangling_files` must exclude `dj_track`; otherwise queued-but-unplayed
  tracks are deleted 60 minutes after upload.
- Reject non-audio input, video files, and absurd durations before transcoding.
- Rate-limit the queue endpoint and enforce per-channel/per-server quotas.

### Storage

DJ blobs are **unencrypted** and served from object storage (MinIO/S3 via
autumn). Whole-object AES-256-GCM cannot be range-decrypted, so the normal file
path is unusable for streaming. Use a dedicated bucket/prefix and a dedicated
handler.

### Streaming

`GET /dj/{channelId}/stream?token=<signed>`

- Real byte-range handler: `Accept-Ranges: bytes`, `Content-Range`,
  `status 206`.
- Short-TTL HMAC token: `(channel, play_seq, expiry)`; TTL =
  `max_track_seconds + 60 s`.
- Signed URL because `<audio>` cannot send an `Authorization` header; ExoPlayer
  can (media3-okhttp is already a dependency) but one mechanism for all clients.
- Never send the URL to a non-joined client.

---

## 5. Playback engine

A dedicated single-replica worker (new daemon, not `crond`: crond is a stateless
60 s loop with no lease and would double-play with two replicas).

Loop per channel with a live queue:
1. Acquire/renew lease.
2. Promote head: atomic `findOneAndUpdate({state: queued}, {state: playing,
   started_at: now, position_ms: 0}, sort: seq)`.
3. Persist `position_ms` every 5 s.
4. Wake on `started_at + duration_ms`, on a skip, or on membership change.
5. On end/skip: `state = played`, `delete_after = now + 60 s`, increment
   `play_seq`, broadcast, then promote the next track.

### Empty channel
- Member count hits 0 -> set `empty_since`, emit `DJ.Paused` after a **120 s**
  grace (not immediately).
- Rejoin within grace -> resume.
- Still empty after **1 h** -> `QueueCleared`, delete all DJ blobs for the
  channel. This is the **single destructive path**.
- Do not use `skip_timeout`: end time is `started_at + duration_ms`, with a +5 s
  safety margin. One clock.

### Crash / restart
- Resume from durable state, no wipe.
- If `now > started_at + duration_ms` during downtime, treat the track as ended
  and advance.
- Otherwise resume at the persisted `position_ms`.
- A stale lease expires after 10 s; the new holder continues.

### Reaper
- Deletes rows with `state = played AND delete_after <= now` via the existing
  reference-counted deletion path (`count_file_hash_references(hash) <= 1`), so
  shared blobs survive.
- Orphan sweep deletes `dj_track` objects with no queue row and no upload in
  progress, using an upload epoch so in-flight enqueues are not reaped.

---

## 6. Events

All DJ events carry `play_seq`; clients drop any event with `seq < current`.

```
DJ.MemberJoined   {channel, user, count, play_seq}
DJ.MemberLeft     {channel, user, count, play_seq}
DJ.TrackQueued    {channel, track{id,title,duration_ms,uploader}, position, play_seq}
DJ.TrackStarted   {channel, track, position_ms, server_time, signed_url, play_seq}
DJ.NowPlaying     {channel, track, position_ms, server_time, signed_url, play_seq}
DJ.Vote           {channel, votes, needed, voters, play_seq}
DJ.Skipped        {channel, track, play_seq}
DJ.TrackEnded     {channel, track, play_seq}
DJ.Paused         {channel, play_seq}
DJ.QueueCleared   {channel, play_seq}
DJ.UploadFailed   {channel, reason}
```

Notes:
- `DJ.Vote` uses `votes`, not `for` (Rust keyword).
- `DJ.NowPlaying` is a heartbeat every **15 s**, not only on join.
- **Targeting:** DJ events go to private user topics for each joined member
  (`EventV1::private(user_id)`), not the channel topic. The channel topic fans
  out to every member with `ViewChannel`, which would leak queue contents and
  signed URLs to lurkers.
- Optionally publish a URL-free `DJ.TrackQueued`-lite to the channel topic so
  non-joined members see activity.

### State snapshot
- `dj_states: Option<Vec<DjChannelState>>` in the `Ready` payload (voice
  precedent).
- `GET /channels/{id}/dj` returns:
  `{current, position_ms, server_time, play_seq, queue, votes, voters, members, settings}`.
- `POST .../dj/join` returns the same body.
- Reconnecting clients resync from `NowPlaying` heartbeats.

### Clock sync
- Every start/heartbeat includes `position_ms` **at emit time** and
  `server_time`.
- Client measures RTT on the existing websocket ping, applies an offset, and
  advances with a monotonic clock from receipt.
- Re-sync on each heartbeat; seek only when `|drift| > 500 ms`; clamp to
  `[0, duration]`.

---

## 7. Skip voting

- One vote per `(play_seq, user)`; Redis set `dj_votes:{channel}:{play_seq}`,
  deleted on advance.
- `POST /channels/{id}/dj/skip` is idempotent.
- `needed = max(1, ceil(present_voters × threshold))`.
  - `threshold = 0` means **one vote skips** (special-cased; do not let
    `votes >= 0` fire on arrival).
  - `threshold = 1` means unanimous; document it as such, not "never".
- Re-evaluate on vote cast **and** on member join/leave.
- Votes from users no longer in the member set are dropped at evaluation time.
- `present_voters` is the current live member count, never a cached count.

---

## 8. Endpoints

```
POST   /channels/{id}/dj/join            -> state, starts lease
POST   /channels/{id}/dj/leave
POST   /channels/{id}/dj/heartbeat
GET    /channels/{id}/dj                 -> full state
POST   /channels/{id}/dj/queue           -> multipart upload (idempotent)
POST   /channels/{id}/dj/skip
PATCH  /channels/{id}/dj/settings        -> ManageChannel
GET    /dj/{channelId}/stream?token=...  -> 206 range-capable audio
```

Authz: join/queue/skip require the same channel view permission; settings
require `ManageChannel`; server admins can set `enabled = false`.

---

## 9. Clients

### Web (and desktop)
- No audio playback code exists today; this is net-new.
- **Autoplay:** browsers block `play()` without a gesture. Unlock the audio
  element synchronously in the join click, then `seekTo(position_ms)` on the
  unlocked element. Keep a "tap to enable sound" state for `NotAllowedError`.
  Never start sound from an event handler as the first sound.
- Desktop is a thin Electron shell over the web build; set `autoplayPolicy`
  explicitly and count it as "web plus one config flag", not a separate client.
- Queue UI replaces the composer.

### Android
- androidx.media3 ExoPlayer 1.9.2 is already the media engine.
- **V1 is foreground-only.** No `media3-session`, no `MediaSessionService`, no
  `mediaPlayback` foreground service. Playback runs while the DJ channel screen
  is in the foreground; backgrounding the app stops/pauses audio (the OS may
  kill the process). Background playback is explicitly out of scope for v1.
- Channel-scoped player: advances on `DJ.TrackStarted`, seeks on mid-song join
  (`prepare()` then `seekTo(position_ms)` from `server_time` + RTT).
- Handle: leaving the channel (stop), two DJ channels at once (one active), audio
  focus/ducking, and upload-picker constraints.

### SDK
- Add channel `dj` field, DJ event types, state fetch, join/leave/heartbeat,
  queue, skip.

---

## 10. Security, abuse, and moderation

- Signed short-TTL stream URLs; URLs only to joined users.
- ClamAV scan before transcode (autumn already has the scanner).
- Keep file report/takedown and a per-server "disable DJ" toggle.
- Per-channel ceilings: `max_tracks`, `max_total_bytes`, max queue duration
  (4 h). Per-server quota. Rate-limit uploads, joins, skips.
- Duration cap (default 15 min) prevents one member parking a 40-minute mix.
- "Delete after play" governs the server copy only; a user can still record
  audio. Document this, do not imply DRM.

---

## 11. Rollout

1. Backend models + capability field + posting guards.
2. Voice-style membership join/leave + TTL heartbeat + `dj_states` in Ready + events.
3. Storage: unencrypted DJ blob path + range handler + signed URL.
4. Transcode worker + queue store + playback engine.
5. SDK.
6. Web player (autoplay unlock) + queue UI.
7. Android foreground channel player (no session service).
8. Desktop config flag.

No `channel_type` enum value is added, so shipped Android clients keep parsing
`Ready` normally; DJ is detected via `dj != null`.

---

## 12. Explicitly out of scope for v1
- Requeue after play (re-upload instead).
- Playlists, history, replay.
- Cross-server federation of DJ queues.
- Downloading tracks to disk on clients.
- DRM.

## 13. Open decisions
1. Confirm crash recovery = resume (v1 said wipe). Default: resume.
2. ~~Android background playback~~ **Resolved: foreground-only in v1.**
