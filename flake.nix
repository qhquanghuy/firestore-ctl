{
  description = "firectl: query Firestore from the command line";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    # The ShipIt CLI (`shipit check`, `verify`, `pre-push`) in the dev shell. Own nixpkgs: its
    # toolchain stays as ShipIt tested it.
    shipit.url = "github:qhquanghuy/shipit";
  };

  outputs =
    {
      nixpkgs,
      crane,
      flake-utils,
      shipit,
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

        firectl = craneLib.buildPackage (releaseArgs // { doCheck = false; });

        # Portable release binary: statically linked against musl, so it runs on any
        # x86_64 Linux. The default package links against /nix/store and only runs where
        # that store exists.
        muslCraneLib = crane.mkLib pkgs.pkgsStatic;
        muslArgs = commonArgs // { doCheck = false; };
        firectlStatic = muslCraneLib.buildPackage (
          muslArgs // { cargoArtifacts = muslCraneLib.buildDepsOnly muslArgs; }
        );
      in
      {
        packages = {
          default = firectl;
        }
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") { static = firectlStatic; };

        checks = {
          default = firectl;

          clippy = craneLib.cargoClippy (
            checkArgs
            // {
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          doc = craneLib.cargoDoc (
            checkArgs
            // {
              cargoDocExtraArgs = "--no-deps --document-private-items";
              RUSTDOCFLAGS = "-D warnings";
            }
          );
          fmt = craneLib.cargoFmt { inherit src; };
          nextest = craneLib.cargoNextest checkArgs;
        };

        devShells.default = pkgs.mkShell {
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

          # nixpkgs rustc ships without llvm-tools; take the LLVM rustc itself was built with.
          LLVM_COV = "${pkgs.rustc.llvmPackages.llvm}/bin/llvm-cov";
          LLVM_PROFDATA = "${pkgs.rustc.llvmPackages.llvm}/bin/llvm-profdata";

          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            cargo-nextest
            cargo-llvm-cov
            pkg-config
            firebase-tools
            jdk21_headless
            shipit.packages.${system}.default
          ];
        };
      }
    );
}
