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
          # Build only the harness; the rest of the workspace is irrelevant.
          # (-p overrides crane's default --locked, so pass it back explicitly.)
          cargoExtraArgs = "--locked -p kronika-bdd";
          doCheck = false;
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        kronika-bdd = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "kronika-bdd";
          }
        );
      in
      {
        packages = {
          default = kronika-bdd;
          inherit kronika-bdd;
        } // pgMatrix;

        devShells.default = craneLib.devShell {
          packages = builtins.attrValues pgMatrix;
        };
      }
    );
}
