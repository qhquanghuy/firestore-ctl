{
  description = "fsq: query Firestore from the command line";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      crane,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        craneLib = crane.mkLib pkgs;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        releaseArgs = commonArgs // { inherit cargoArtifacts; };

        # Checks run in the dev profile (debug assertions, overflow checks, no LTO).
        devArgs = commonArgs // {
          CARGO_PROFILE = "dev";
        };
        devArtifacts = craneLib.buildDepsOnly devArgs;
        checkArgs = devArgs // { cargoArtifacts = devArtifacts; };

        fsq = craneLib.buildPackage (releaseArgs // { doCheck = false; });
      in
      {
        packages.default = fsq;

        checks = {
          default = fsq;

          clippy = craneLib.cargoClippy (
            checkArgs
            // {
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          fmt = craneLib.cargoFmt { inherit src; };
          nextest = craneLib.cargoNextest checkArgs;
        };

        devShells.default = pkgs.mkShell {
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            cargo-nextest
            pkg-config
            firebase-tools
            jdk21_headless
          ];
        };
      }
    );
}
