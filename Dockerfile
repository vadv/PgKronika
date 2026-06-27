# The BDD matrix is built and run entirely inside Docker; the only host
# dependency is Docker. Nix (from the base image) supplies the Rust toolchain,
# the harness binary, and every PostgreSQL version. The harness then boots them
# all in parallel and runs the cucumber suite (see crates/kronika-bdd).
FROM nixos/nix:latest

ENV NIX_CONFIG="experimental-features = nix-command flakes"

WORKDIR /src
COPY . .

# Build the harness and realise each PostgreSQL version's `out` into the store.
RUN nix build .#kronika-bdd        --out-link /artifacts/bdd  \
 && nix build '.#postgresql_15^out' --out-link /artifacts/pg15 \
 && nix build '.#postgresql_16^out' --out-link /artifacts/pg16 \
 && nix build '.#postgresql_17^out' --out-link /artifacts/pg17

# postgres refuses to run as root, so the suite runs as an unprivileged user.
RUN mkdir -p /home/bdd /tmp \
 && printf 'bdd:x:1000:1000::/home/bdd:/bin/sh\n' >> /etc/passwd \
 && printf 'bdd:x:1000:\n' >> /etc/group \
 && chown 1000:1000 /home/bdd \
 && chmod 1777 /tmp

ENV HOME=/home/bdd \
    TMPDIR=/tmp \
    LC_ALL=C \
    LANG=C \
    KRONIKA_PG_MATRIX="15=/artifacts/pg15/bin;16=/artifacts/pg16/bin;17=/artifacts/pg17/bin"

USER 1000:1000
# cucumber loads ./features relative to the working directory.
WORKDIR /src/crates/kronika-bdd
ENTRYPOINT ["/artifacts/bdd/bin/kronika-bdd"]
