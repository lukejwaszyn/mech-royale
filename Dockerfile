# syntax=docker/dockerfile:1
# Mech Royale bot image: a static musl binary in an empty image.
#
#   docker build -t mech-bot .
#   python3 <kit>/check/check.py --map-dir /arena -- docker run --rm -i --network none \
#       --cpus 1 --memory 512m --read-only --tmpfs /tmp:rw,size=64m --user 65534:65534 \
#       -v "$PWD/arena:/arena:ro" mech-bot

FROM rust:1.99.0-alpine3.22 AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# Only what the bot needs: the workspace manifest globs crates/*, so leaving the local
# simulator (crates/sim) out of the build context simply makes it not a member.
COPY Cargo.toml Cargo.lock ./
COPY crates/world crates/world
COPY crates/bot crates/bot
ENV CARGO_PROFILE_RELEASE_STRIP=symbols \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=4 \
    CARGO_INCREMENTAL=0
RUN cargo build --release -p bot --bin bot \
 && cp target/release/bot /bot

FROM scratch
COPY --from=build /bot /bot
# Runs in the empty image: proves the binary is static and starts.
RUN ["/bot", "--version"]
# practice | ffa  (ffa = the final: free-for-all against other teams' bots)
ENV MECH_MODE=practice
USER 65534:65534
ENTRYPOINT ["/bot"]
