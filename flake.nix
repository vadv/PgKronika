{
  description = "PgKronika BDD test infrastructure: a Nix-built cucumber harness and a matrix of PostgreSQL versions, assembled and run inside Docker so the only host dependency is Docker itself";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Match the workspace MSRV pin so the harness is built with the exact
        # toolchain the rest of the project uses (rust-toolchain.toml).
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Major versions whose system-catalog drift the collector must track
        # (e.g. pg_stat_bgwriter -> pg_stat_checkpointer in 17). Adding a
        # version is one line here; minor versions do not change catalogs.
        pgMatrix = {
          postgresql_15 = pkgs.postgresql_15;
          postgresql_16 = pkgs.postgresql_16;
          postgresql_17 = pkgs.postgresql_17;
        };

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          # Build only the two binaries used by the Docker BDD suite.
          # -p overrides crane's default --locked, so pass it back explicitly.
          cargoExtraArgs = "--locked -p kronika-bdd -p pg_kronika-collector";
          doCheck = false;
        };

        cargoArtifacts = craneLib.buildDepsOnly (
          commonArgs
          // {
            pname = "pgkronika-bdd-deps";
          }
        );

        # Keep both binaries in one store path so the BDD runner can spawn the collector.
        bins = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "pgkronika-bins";
          }
        );

        # The cucumber feature files; the harness reads this path from
        # KRONIKA_FEATURES inside the image.
        features = ./crates/kronika-bdd/features;

        # FROM-scratch image for the BDD suite. PostgreSQL runs as nobody; fakeNss
        # makes that uid resolvable for initdb, and /tmp holds throwaway data dirs.
        # Code changes rebuild only the thin app layer.
        image = pkgs.dockerTools.streamLayeredImage {
          name = "pgkronika-bdd";
          tag = "latest";
          maxLayers = 120;
          contents = [
            bins
            pkgs.postgresql_15
            pkgs.postgresql_16
            pkgs.postgresql_17
            pkgs.dockerTools.fakeNss
            # initdb shells out via popen, so the scratch image needs /bin/sh.
            pkgs.dockerTools.binSh
          ];
          extraCommands = "mkdir -m 1777 tmp";
          config = {
            Entrypoint = [ "${bins}/bin/kronika-bdd" ];
            User = "65534:65534";
            Env = [
              "HOME=/tmp"
              "TMPDIR=/tmp"
              "LC_ALL=C"
              "LANG=C"
              "KRONIKA_FEATURES=${features}"
              "KRONIKA_COLLECTOR_BIN=${bins}/bin/pg_kronika-collector"
              "KRONIKA_PG_MATRIX=15=${pkgs.postgresql_15}/bin;16=${pkgs.postgresql_16}/bin;17=${pkgs.postgresql_17}/bin"
            ];
          };
        };
      in
      {
        packages = {
          default = bins;
          inherit bins cargoArtifacts image;
        } // pgMatrix;

        devShells.default = craneLib.devShell {
          packages = builtins.attrValues pgMatrix;
        };
      }
    );
}
