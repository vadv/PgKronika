{
  description = "PgKronika BDD image for PostgreSQL 15-18";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    # vadv fork of pg_store_plans. The rev is pinned because PostgreSQL 18
    # support is newer than the last release tag; update it with
    # `nix flake update pg-store-plans-vadv`.
    pg-store-plans-vadv = {
      url = "github:vadv/pg_store_plans/1ac02d9e8f84d012b8a2527a41ecd8f2d3ce4493";
      flake = false;
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      pg-store-plans-vadv,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Use the workspace Rust toolchain.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # PostgreSQL majors covered by the collector BDD suite.
        pgMatrix = {
          postgresql_15 = pkgs.postgresql_15;
          postgresql_16 = pkgs.postgresql_16;
          postgresql_17 = pkgs.postgresql_17;
          postgresql_18 = pkgs.postgresql_18;
        };

        # PGXS build of the vadv pg_store_plans fork against one major.
        mkStorePlansVadv =
          pg:
          pg.pkgs.postgresqlBuildExtension (_: {
            pname = "pg_store_plans-vadv";
            version = "2.1";
            src = pg-store-plans-vadv;
            makeFlags = [
              "USE_PGXS=1"
              "PG_CONFIG=${pg.pg_config}/bin/pg_config"
            ];
            enableUpdateScript = false;
            installPhase = ''
              runHook preInstall
              install -D -t $out/lib pg_store_plans.so
              install -D -t $out/share/postgresql/extension \
                pg_store_plans.control pg_store_plans--*.sql
              runHook postInstall
            '';
          });

        # The image includes the vadv fork on PG17/18. PG15/16 omit
        # pg_store_plans because both forks install the same file names.
        postgresql_17_plans = pkgs.postgresql_17.withPackages (_: [
          (mkStorePlansVadv pkgs.postgresql_17)
        ]);
        postgresql_18_plans = pkgs.postgresql_18.withPackages (_: [
          (mkStorePlansVadv pkgs.postgresql_18)
        ]);

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          # Limit the image build to the BDD runner and the collector.
          # `-p` replaces crane's default flags, so keep `--locked` here.
          cargoExtraArgs = "--locked -p kronika-bdd -p pg_kronika-collector";
          doCheck = false;
        };

        cargoArtifacts = craneLib.buildDepsOnly (
          commonArgs
          // {
            pname = "pgkronika-bdd-deps";
          }
        );

        # One store path lets the runner spawn the collector.
        bins = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "pgkronika-bins";
          }
        );

        # Feature files are read through KRONIKA_FEATURES.
        features = ./crates/kronika-bdd/features;

        # Scratch image for the BDD suite.
        image = pkgs.dockerTools.streamLayeredImage {
          name = "pgkronika-bdd";
          tag = "latest";
          maxLayers = 120;
          contents = [
            bins
            pkgs.postgresql_15
            pkgs.postgresql_16
            postgresql_17_plans
            postgresql_18_plans
            pkgs.dockerTools.fakeNss
            # initdb uses popen, so the scratch image needs /bin/sh.
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
              "KRONIKA_PG_MATRIX=15=${pkgs.postgresql_15}/bin;16=${pkgs.postgresql_16}/bin;17=${postgresql_17_plans}/bin;18=${postgresql_18_plans}/bin"
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
