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
    # ossc upstream, tag 1.10 (PostgreSQL 18 support).
    pg-store-plans-ossc = {
      url = "github:ossc-db/pg_store_plans/e802804d4d2763ad3b32fc8fcf9f218c61cc475f";
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
      pg-store-plans-ossc,
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

        # PGXS build of a pg_store_plans fork against one major. Both forks
        # install identically named files, so one cluster carries one fork.
        mkStorePlans =
          pg: forkName: forkVersion: forkSrc:
          pg.pkgs.callPackage (
            { postgresqlBuildExtension }:
            postgresqlBuildExtension {
              pname = "pg_store_plans-${forkName}";
              version = forkVersion;
              src = forkSrc;
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
            }
          ) { };

        # vadv on PG17/18, ossc upstream on PG15/16: both collector paths get
        # live BDD coverage.
        postgresql_15_plans = pkgs.postgresql_15.withPackages (_: [
          (mkStorePlans pkgs.postgresql_15 "ossc" "1.10" pg-store-plans-ossc)
        ]);
        postgresql_16_plans = pkgs.postgresql_16.withPackages (_: [
          (mkStorePlans pkgs.postgresql_16 "ossc" "1.10" pg-store-plans-ossc)
        ]);
        postgresql_17_plans = pkgs.postgresql_17.withPackages (_: [
          (mkStorePlans pkgs.postgresql_17 "vadv" "2.1" pg-store-plans-vadv)
        ]);
        postgresql_18_plans = pkgs.postgresql_18.withPackages (_: [
          (mkStorePlans pkgs.postgresql_18 "vadv" "2.1" pg-store-plans-vadv)
        ]);

        commonArgs = {
          # Cargo-source filtering keeps only .rs files and manifests, which
          # drops the web UI assets rust-embed compiles into the binary; the
          # bench directory is pinned too so a filter change cannot break the
          # manifest's declared [[bench]] target.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            # maybeMissing: the BDD builder context carries only manifests and
            # dummy sources, so these directories may be absent there.
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              (pkgs.lib.fileset.maybeMissing ./crates/kronika-reader/benches)
              (pkgs.lib.fileset.maybeMissing ./bins/pg_kronika-web/benches)
              (pkgs.lib.fileset.maybeMissing ./bins/pg_kronika-web/static)
            ];
          };
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkgsMusl.stdenv.cc ];
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER =
            "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";
          CC_x86_64_unknown_linux_musl = "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";
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

        compilerWrapper = pkgs.writeShellScriptBin "pgkronika-sccache" ''
          IFS= read -r SCCACHE_LOCAL_RW_MODE < /var/cache/pgkronika-sccache/.mode
          case "$SCCACHE_LOCAL_RW_MODE" in
            READ_ONLY|READ_WRITE) ;;
            *) exit 2 ;;
          esac
          export SCCACHE_LOCAL_RW_MODE
          export SCCACHE_DIR=/var/cache/pgkronika-sccache
          export SCCACHE_CACHE_SIZE=2G
          export SCCACHE_BASEDIRS=/tmp:/build:/nix/store:/nix/var/nix/builds
          export SCCACHE_IDLE_TIMEOUT=0
          exec ${pkgs.sccache}/bin/sccache "$@"
        '';

        compilerTools = pkgs.symlinkJoin {
          name = "pgkronika-bdd-compiler-tools";
          paths = [ pkgs.sccache compilerWrapper ];
        };

        # One store path lets the runner spawn the collector.
        bins = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "pgkronika-bins";
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.sccache compilerWrapper ];
            RUSTC_WRAPPER = "${compilerWrapper}/bin/pgkronika-sccache";
          }
        );

        # Feature files are read through KRONIKA_FEATURES.
        features = ./crates/kronika-bdd/features;

        bddPgMatrix = pkgs.linkFarm "pgkronika-bdd-pg-matrix" [
          {
            name = "postgresql-15";
            path = postgresql_15_plans;
          }
          {
            name = "postgresql-16";
            path = postgresql_16_plans;
          }
          {
            name = "postgresql-17";
            path = postgresql_17_plans;
          }
          {
            name = "postgresql-18";
            path = postgresql_18_plans;
          }
          {
            name = "fake-nss";
            path = pkgs.dockerTools.fakeNss;
          }
          {
            name = "bin-sh";
            path = pkgs.dockerTools.binSh;
          }
        ];

        bddPgClosureManifest = pkgs.writeText "pgkronika-bdd-pg-closure.json" (
          builtins.toJSON {
            schema = 2;
            postgresql = {
              "15" = "${postgresql_15_plans}";
              "16" = "${postgresql_16_plans}";
              "17" = "${postgresql_17_plans}";
              "18" = "${postgresql_18_plans}";
            };
            helpers = {
              fakeNss = "${pkgs.dockerTools.fakeNss}";
              binSh = "${pkgs.dockerTools.binSh}";
            };
          }
        );

        bddCargoClosureManifest = pkgs.writeText "pgkronika-bdd-cargo-closure.json" (
          builtins.toJSON {
            schema = 2;
            target = "x86_64-unknown-linux-musl";
            features = "default";
            cargoArtifacts = "${cargoArtifacts}";
            compilerTools = "${compilerTools}";
          }
        );

        bddAppRoot = pkgs.runCommand "pgkronika-bdd-app-root" { } ''
          mkdir -p $out/opt/pgkronika/bin $out/opt/pgkronika/features
          install -m 0755 ${bins}/bin/kronika-bdd $out/opt/pgkronika/bin/kronika-bdd
          install -m 0755 ${bins}/bin/pg_kronika-collector \
            $out/opt/pgkronika/bin/pg_kronika-collector
          cp -R ${features}/. $out/opt/pgkronika/features/
        '';

        bddAppLayer = pkgs.runCommand "pgkronika-bdd-app-layer.tar" {
          nativeBuildInputs = [ pkgs.gnutar ];
        } ''
          tar \
            --sort=name \
            --mtime=@1 \
            --owner=0 \
            --group=0 \
            --numeric-owner \
            -C ${bddAppRoot} \
            -cf $out \
            .
        '';

        # Scratch image for the BDD suite.
        image = pkgs.dockerTools.streamLayeredImage {
          name = "pgkronika-bdd";
          tag = "latest";
          maxLayers = 120;
          contents = [
            bins
            postgresql_15_plans
            postgresql_16_plans
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
              "KRONIKA_PG_MATRIX=15=${postgresql_15_plans}/bin;16=${postgresql_16_plans}/bin;17=${postgresql_17_plans}/bin;18=${postgresql_18_plans}/bin"
            ];
          };
        };
      in
      {
        packages = {
          default = bins;
          inherit
            bins
            cargoArtifacts
            image
            bddPgMatrix
            bddPgClosureManifest
            bddCargoClosureManifest
            bddAppLayer
            compilerTools
            ;
          bddCargoArtifacts = cargoArtifacts;
          bddCompilerTools = compilerTools;
        } // pgMatrix;

        devShells.default = craneLib.devShell {
          packages = builtins.attrValues pgMatrix;
        };
      }
    );
}
