# DJ Channel — Feature Spec v3 (simplest implementation)

A **DJ-enabled text channel**: members join explicitly (voice-style), do not post
messages, and hear a shared FIFO music queue. Anyone joined can upload a track;
files live on local disk and are deleted after they play.

v3 keeps every correctness fix from the v2 review but takes the simplest
implementation path. It assumes a **single API/delta instance** and drops crash
recovery entirely.

---

## 0. What v3 simplifies vs v2

| Area | v2 (robust) | v3 (simplest) |
|---|---|---|
| Queue state | Mongo `dj_queue` + transactions | Redis list per channel |
| Playback owner | separate daemon + Redis lease | one `tokio` task per active channel inside delta |
| Storage | unencrypted MinIO bucket + custom range handler | local disk + Rocket `NamedFile` (built-in 206) |
| Crash recovery | resume from durable state | **none**: restart clears queues and files |
| Position persistence | `position_ms` every 5 s | in-memory only |
| Event ordering | `play_seq` fencing | single publisher, order guaranteed by the bus |
| Membership | TTL heartbeat (30 s / 90 s) | join/leave + socket-close removal |
| Delete on play | reference-counted reaper | delete local file 60 s after it ends |

Everything else (channel model, join semantics, skip math, posting guards,
autoplay unlock, foreground-only Android) is unchanged from v2.

**Assumption:** one delta instance, or at least one instance serving DJ channels.
The in-process engine is not safe across replicas. That is accepted.

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

```rust
struct DjInformation {
    skip_threshold: f32,        // 0.0..=1.0, default 0.5
    max_file_mb: u32,           // per-upload, clamped to the uploader's tier limit
    max_track_seconds: u32,     // default 900 (15 min)
    max_tracks: u32,            // total queue depth, default 100
    max_total_bytes: u64,       // per-channel disk ceiling, default 20 GiB
    max_users: Option<u32>,     // optional join cap, like voice_info.max_users
    enabled: bool,              // channel on/off
}
```

Why not a variant:
- `core/model/.../Channel.kt:267` (Android) throws on unknown `ChannelType` in
  the `Ready` payload. A new variant breaks every shipped app on login.
- `Channel::server()` returns `None` for unhandled variants, reaching
  `unimplemented!()` in `permissions.rs:379`.
- Voice already models "behaves differently" as an optional field.

Consequences: `channel_type` stays `TextChannel`; clients detect DJ via
`dj != null`. Old clients show a normal text channel and get a clear
`CannotSendMessageToDjChannel` error if they try to post.

### Posting guards

Read-only for messages. Add `resolve_postable_channel(channel)` used by message
send/edit/delete, reactions, pins, typing, webhooks, and search indexing. Threads
and forum posts cannot be created. Acks/unreads stay allowed.

### Settings

`PATCH /channels/{id}/dj/settings`, gated on `ManageChannel`. Do not widen
`PartialChannel`/`DataEditChannel`; that would make DJ fields PATCH-able on every
channel type and leak into audit-log diffs.

---

## 2. Membership (voice-style)

- `POST /channels/{id}/dj/join` mirrors `join_call`: requires `Connect`, enforces
  `dj.max_users` if set, adds the user, returns full state.
- `POST /channels/{id}/dj/leave` removes the user.
- Membership is the Redis set `dj_members:{channel}`.
- A user is also removed when their websocket session closes.
- No heartbeat in v3. Trade-off: a client killed without a clean close lingers
  until a lightweight reconciliation sweep drops members with no live session
  (run once a minute; cheap).
- `dj_states` in `Ready` mirrors `voice_states`, so a reconnect knows which DJ
  channels the user is in.
- Count = `SCARD dj_members:{channel}`. Never derive it from `ViewChannel`.

**Sidebar/UX:** a TextChannel with `dj != null` renders as joinable (headphones
badge, member count). Opening without joining shows the queue read-only with a
Join button; joining unlocks upload/skip.

---

## 3. State model (Redis only)

```
dj_members:{channel}          SET    user ids
dj_queue:{channel}            LIST   JSON entries, FIFO
dj_now:{channel}              HASH   current track + started_at
dj_votes:{channel}:{track}    SET    user ids for the current track
dj_seq:{channel}              STRING counter for track ids
dj_empty_since:{channel}      STRING timestamp (empty-grace marker)
```

Queue entry:

```json
{"id":"<seq>-<ulid>","file":"/dj/<channel>/<id>.mp3","title":"...",
 "duration_ms":183000,"uploader":"<user id>"}
```

`dj_now` = the same entry plus `started_at` (unix ms).

There is no durable store. On process start, every `dj_*` key and every file
under `DJ_DIR` is deleted (no crash recovery).

---

## 4. Adding music to the queue

Client: the composer is replaced by an upload control on a joined DJ channel.

1. `POST /channels/{id}/dj/queue` (multipart, `IdempotencyKey`).
   - Require an active membership; otherwise `NotInDjChannel`.
2. Validate size (`max_file_mb`, capped by the uploader's tier) and reject
   non-audio input.
3. Transcode synchronously in the request handler: ffmpeg `-vn`, MP3,
   `-t max_track_seconds`, hard timeout, bounded by a semaphore (2 concurrent).
   ffmpeg is added to the delta image; a startup `ffmpeg -version` check fails
   loudly if missing.
4. `ffprobe` the **output** for the authoritative duration. Reject probe
   failures, empty output, or duration over the cap; delete the temp file and
   emit `DJ.UploadFailed`.
5. Under a per-channel async mutex, check the caps: 3 queued/playing per member,
   `max_tracks`, `max_total_bytes`. If any fails, delete the file and reject.
6. Move the MP3 to `DJ_DIR/{channel}/{id}.mp3` (unencrypted), `RPUSH` the queue
   entry, and `INCR dj_seq`.
7. Publish `DJ.TrackQueued {track, position}` to joined members. If the engine
   task is idle, wake it.

There is no `dj_track` tag, no autumn binding, and no `prune_dangling_files`
interaction, because DJ files never enter the file service. A small periodic
sweep deletes files in `DJ_DIR` with no matching queue/now entry.

---

## 5. Playback engine

One `tokio` task per channel that has members. Started on the first join,
cancelled when the channel has been empty past the grace.

```
loop {
    if no current track:
        LPOP queue
        if none: await notify (new upload or member join)
        else:    set dj_now, started_at = now, publish TrackStarted
    select {
        sleep until started_at + duration_ms + 5 s
        skip notify  -> publish Skipped
        shutdown     -> exit
    }
    publish TrackEnded (or Skipped)
    schedule delete of the file after 60 s
    clear dj_now
}
```

- `DJ.NowPlaying` heartbeat every 15 s: `position_ms = now - started_at`.
- Track id is the queue entry id; votes are scoped to it.
- On end or skip the file is deleted 60 s later, leaving a buffer for slow
  clients. Deleting is a local `remove_file`.
- No position persistence, no resume, no lease.

### Empty channel
- On the last leave, set `dj_empty_since`.
- If still empty after **30 s**, cancel the task, delete all files for the
  channel, clear the `dj_*` keys, publish `DJ.QueueCleared`.
- A rejoin within 30 s cancels the grace and resumes.
- Process restart clears everything immediately (see §3).

---

## 6. Events

Single in-process publisher, so events are ordered; no `play_seq`.

```
DJ.MemberJoined  {channel, user, count}
DJ.MemberLeft    {channel, user, count}
DJ.TrackQueued   {channel, track, position}
DJ.TrackStarted  {channel, track, position_ms, server_time, signed_url}
DJ.NowPlaying    {channel, track, position_ms, server_time, signed_url}
DJ.Vote          {channel, votes, needed, voters}
DJ.Skipped       {channel, track}
DJ.TrackEnded    {channel, track}
DJ.QueueCleared  {channel}
DJ.UploadFailed  {channel, reason}
```

- `DJ.Vote` uses `votes`, not `for` (Rust keyword).
- **Targeting:** publish to each joined member's private user topic, not the
  channel topic. The channel topic reaches every `ViewChannel` member and would
  leak queue contents and signed URLs.
- Optionally publish a URL-free `DJ.TrackQueued`-lite to the channel topic.

### State snapshot
- `dj_states` in `Ready`.
- `GET /channels/{id}/dj`:
  `{current, position_ms, server_time, queue, votes, voters, members, settings}`.
- `POST .../dj/join` returns the same body.
- Reconnects resync from the 15 s `NowPlaying` heartbeat.

### Clock sync
- `position_ms` at emit time + `server_time`.
- Client measures RTT on the existing websocket ping and advances with a
  monotonic clock from receipt.
- Seek only when `|drift| > 500 ms`; clamp to `[0, duration]`.

---

## 7. Skip voting

- `POST /channels/{id}/dj/skip` is idempotent; `SADD dj_votes:{channel}:{track}`.
- `needed = max(1, ceil(present_voters × threshold))`.
  - `threshold = 0` means one vote skips (special case; never let `votes >= 0`
    fire on arrival).
  - `threshold = 1` means unanimous.
- Re-evaluate on vote cast **and** on member join/leave.
- Votes from users no longer in the member set are dropped at evaluation time.
- `present_voters` = current `SCARD dj_members:{channel}`.

---

## 8. Endpoints

```
POST   /channels/{id}/dj/join            -> state
POST   /channels/{id}/dj/leave
GET    /channels/{id}/dj                 -> full state
POST   /channels/{id}/dj/queue           -> multipart upload (idempotent)
POST   /channels/{id}/dj/skip
PATCH  /channels/{id}/dj/settings        -> ManageChannel
GET    /dj/{channel}/stream?token=...    -> 206 range-capable audio
```

### Streaming

- Files are served from local disk with Rocket `NamedFile`, which handles
  `Range`/`206`/`Accept-Ranges` natively (Rocket 0.5; enable its `fs` feature
  if not already on).
- Signed query token: HMAC over `(channel, track_id, expiry)`;
  TTL = `max_track_seconds + 60 s`.
- The token is required because `<audio>` cannot send an `Authorization` header.
- Never send the URL to a non-joined client.

---

## 9. Clients

### Web (and desktop)
- No audio code exists today; this is net-new.
- **Autoplay:** unlock the audio element synchronously in the join click, then
  `seekTo(position_ms)` on the unlocked element. Keep a "tap to enable sound"
  state for `NotAllowedError`. Never start sound from an event handler first.
- Desktop is the same web build; set Electron `autoplayPolicy` explicitly.
- Queue UI replaces the composer.

### Android
- androidx.media3 ExoPlayer 1.9.2 is already the media engine.
- **Foreground-only:** no `media3-session`, no `MediaSessionService`, no
  `mediaPlayback` service. Backgrounding stops playback; background audio is out
  of scope.
- Channel-scoped player: advances on `DJ.TrackStarted`, seeks on mid-song join.
- Handle leaving the channel, two DJ channels at once, audio focus/ducking, and
  upload-picker constraints.

### SDK
- Channel `dj` field, DJ events, state fetch, join/leave, queue, skip.

---

## 10. Security and limits

- Signed short-TTL stream URLs, sent only to joined users.
- Per-channel `max_tracks`, `max_total_bytes`, max queue duration (4 h).
- Rate-limit uploads, joins, skips.
- Duration cap (default 15 min) prevents one member parking a long mix.
- Delete-after-play governs the server copy only; do not imply DRM.
- Files are local and unencrypted, so disk permissions and the signed token are
  the only protection. Acceptable for a self-hosted instance.

---

## 11. Rollout

1. Backend model: `DjInformation` + posting guards.
2. Join/leave + `dj_states` in `Ready` + events.
3. Upload/transcode/queue + local storage + `NamedFile` streaming.
4. In-process playback engine + skip voting + empty-channel clear.
5. SDK.
6. Web player (autoplay unlock) + queue UI.
7. Android foreground player.
8. Desktop config flag.

---

## 12. Out of scope
- Crash recovery / queue durability.
- Multi-replica playback.
- Android background audio.
- Requeue after play (upload again).
- Playlists, history, replay, federation, DRM.

## 13. Open decisions
1. Confirm single-instance delta is the deployment target (the in-process engine
   requires it).
2. Nothing else outstanding.
