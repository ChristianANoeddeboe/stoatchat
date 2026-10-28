# Build Stage
FROM ghcr.io/stoatchat/base:latest AS builder
FROM debian:12 AS debian

# Bundle Stage
#
# Debian slim rather than distroless: DJ channels transcode uploads with
# ffmpeg, whose shared library graph cannot be reliably copied into a
# distroless rootfs.
FROM debian:12-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /home/rust/src/target/release/revolt-delta ./
COPY --from=debian /usr/bin/uname /usr/bin/uname

# Fail the build loudly if ffmpeg is missing from the image
RUN ffmpeg -version > /dev/null && ffprobe -version > /dev/null

# Storage for DJ audio uploads (unencrypted, deleted after playback)
ENV DJ_DIR=/var/lib/revolt/dj
RUN mkdir -p "$DJ_DIR" && chown -R 65532:65532 "$DJ_DIR"

EXPOSE 14702
ENV ROCKET_ADDRESS=0.0.0.0
USER 65532:65532
CMD ["./revolt-delta"]
