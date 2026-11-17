# Bullbar's container image: built in one stage, run in another with nothing but the binary.
# The run stage has no shell or package manager and runs as a non-root user (concept design:
# containers run non-root, with read-only filesystems). Images are pinned by digest, so a base image
# changes only with a commit here.

FROM docker.io/library/rust:1.100-slim-trixie@sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a AS build
WORKDIR /src
# A C++ compiler for tokenizers' native parts (the scrubber's entity model, ADR 0020). Build stage
# only: nothing from it reaches the run stage but the binary.
RUN apt-get update && apt-get install -y --no-install-recommends g++ && rm -rf /var/lib/apt/lists/*
# Only what the build needs: no config, keys or state ever enter the image.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release --locked -p bullbar-cli

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:cb87acc574abe719b6e5c547384defa2f7b1f24af9605d3f618cf852495d8be2
COPY --from=build /src/target/release/bullbar /usr/local/bin/bullbar
USER 65532:65532
# The state folder (tokens, shutter) is $XDG_STATE_HOME/bullbar: mounted from the host.
ENV XDG_STATE_HOME=/state
ENTRYPOINT ["/usr/local/bin/bullbar"]
# Inside the container Bullbar listens on all addresses; compose publishes the ports to the host's
# loopback only. Every chat request needs the chat token (ADR 0016).
CMD ["serve", "--config", "/config", "--listen", "0.0.0.0:8480", "--control", "0.0.0.0:8481"]
