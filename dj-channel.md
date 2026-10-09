# DJ Channel — Feature Spec v1

Add a new channel type `DJ`: people join, don't talk to each other, but hear
music streamed. Anyone can queue a music file; a shared queue plays to every
joined member. Files live locally on the server and are deleted once played to
save space.

## Locked decisions

1. **Full feature** — backend + sdk + web + desktop + Android in one pass.
2. **Transcode** → MP3 via ffmpeg on upload; all clients stream the transcode.
3. **Empty = joined members** — the engine keys off the channel's member count
   (no listener heartbeat).
4. **Crash recovery** → clear queue + delete all DJ blobs on worker restart.
5. **Per-member queue cap** = 3 pending tracks.

## 1. Channel model

- New `channel_type = "DJ"` alongside existing types.
- Same membership/join model as a normal channel, but **no message posting** —
  the composer is replaced by an upload/queue control.
- Per-channel admin settings:
  - `skip_threshold` (float 0–1, default `0.5`) — fraction of joined members
    needed to skip the current track.
  - `max_file_mb` (default e.g. `50`) — per-upload cap.
  - `skip_timeout` — how long before a track auto-advances (natural end wins
    first).

## 2. Audio pipeline

- **Upload** → standard file upload, tagged `kind: "dj_track"`, with `track_meta`
  (title, duration via ffprobe, uploaded_by, added_at) stored with the queue row.
- **Transcode** → on upload, ffmpeg transcodes to MP3 so every client plays the
  same stream regardless of source format. Adds per-upload CPU cost (accepted).
- **Streaming** → range-request-capable endpoint `GET /dj/{channelId}/stream`
  serving the current track so clients can seek/re-buffer. Same track playing
  again is served from a cache keyed by file id.

## 3. Playback engine

A new dedicated Rust worker (fits the existing crond/pushd pattern). It owns:

- active track + position
- the FIFO queue
- current member count (from channel membership)
- broadcast ticks to clients

Rules:
- Track ends → broadcast `TrackEnded`, **delete file from disk**, advance FIFO
  head, `TrackStarted` (stream URL + duration + start time).
- Queue is strictly FIFO. Requeues allowed; a track that just played can't be
  re-added until it leaves the buffer.
- No joined members → `QueueCleared`, stop playback, **delete every queued file**.

## 4. Events (websocket, to all joined clients)

`DJ.MemberJoined/Left`, `DJ.TrackQueued {meta}`, `DJ.TrackStarted {track,url,duration,at}`,
`DJ.TrackEnded`, `DJ.NowPlaying` (on every new joiner — mid-song joins land at
current position), `DJ.Vote {for,total,needed}`, `DJ.Skipped`, `DJ.QueueCleared`.

`NowPlaying`-on-join is load-bearing: cross-client clock sync, all clients
derive position from server `at` timestamp.

## 5. Skip voting

- Any joined member casts one vote per active track via
  `POST /channels/{id}/dj/skip`.
- Server-side tally. On `votes >= ceil(members × threshold)` → broadcast
  `DJ.Skipped`, advance queue, delete the file. Votes are per-track (reset on
  advance).
- Threshold admin-set 0 (anyone skips) to 1 (effectively never), default 0.5.
- Votes are cumulative for the whole track lifetime; track auto-advances on
  natural end OR threshold, whichever first.

## 6. Empty-channel semantics

A channel with **zero joined members** clears the queue, stops playback, and
deletes all queued files. Keyed off membership count, not live connections.

## 7. File lifecycle & GC

- Deleted on: played to completion, skipped via vote, queue cleared (empty
  channel), crash restart (all), orphan sweep.
- **Crash recovery:** on worker restart, clear current-track state and delete
  every DJ blob; the queue was ephemeral by design.
- **Orphan sweep:** periodic job deletes `dj_track` blobs with no queue row
  (abandoned/aborted uploads).

## 8. Endpoints

- `POST /channels/{id}/dj/queue` (multipart upload, tagged `dj_track`)
- `GET /dj/{channelId}/stream` (range requests)
- `POST /channels/{id}/dj/skip`
- Member count: existing channel-membership pathway.

## Android — playback spike (RESOLVED)

**Result: no new playback library needed.** androidx.media3 (ExoPlayer) **1.9.2**
is already the app's media engine: `composables/media/AudioPlayer.kt` (live,
used for audio attachments via `MessageAttachment.kt`) and
`activities/media/VideoViewActivity.kt` both build `ExoPlayer` from a
`MediaItem.fromUri(url)`. HLS + okhttp datasource deps are already in
`gradle/libs.versions.toml`. So:

- MP3 streams play natively (Android codec via MediaCodec).
- Range-request seeking works (media3 HTTP data source; okhttp datasource
  available if needed).
- ExoPlayer runs its own media session + audio renderer, so it **keeps playing
  when the app is backgrounded/minimised** — the feared line item.

What is genuinely new on Android (moderate, not risky):
- A **channel-scoped continuous player** (singleton per DJ channel) that
  advances on `DJ.TrackStarted` events and seeks on mid-song join, instead of
  the per-attachment `AudioPlayer`. Mid-song join = `prepare()` then
  `seekTo(positionMs)` from the event's `at` timestamp.
- Queue UI: upload control replacing the composer in the DJ channel view.

No playback-library research/decision remains open.

## Open (post-v1) considerations
- None locked. Possible future: skip-timeout votes, playlists, DJ admin
  overrides.