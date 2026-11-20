{
  description = "fsq: query Firestore from the command line";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
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
        withArtifacts = commonArgs // { inherit cargoArtifacts; };
      in
      {
        packages.default = craneLib.buildPackage (withArtifacts // { doCheck = false; });

        checks = {
          clippy = craneLib.cargoClippy (
            withArtifacts
            // {
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          fmt = craneLib.cargoFmt { inherit src; };
          nextest = craneLib.cargoNextest (
            withArtifacts
            // {
              cargoNextestExtraArgs = "--no-tests=pass";
            }
          );
        };

        devShells.default = pkgs.mkShell {
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
