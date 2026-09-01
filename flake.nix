{
  description = "nzbkodi — Elementum-style on-demand Usenet for Kodi (engine)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      localSystem:
      let
        pkgs = import nixpkgs {
          inherit localSystem;
          overlays = [ rust-overlay.overlays.default ];
        };

        # Toolchain pinned via rust-overlay, shared with TurboNZB.
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        # `turbonzb-core` is a `git+file://` dependency on the local
        # TurboNZB checkout, so no cross-tree source layout is needed
        # here: crane vendors it from the Cargo.lock like any other dep.
        commonArgs = {
          src = craneLib.path ./.;
          strictDeps = true;

          nativeBuildInputs = with pkgs; [
            pkg-config
          ];

          # sqlx bundles sqlite (libsqlite3-sys) and ring needs cc; both
          # ship with the stdenv toolchain, so no extra buildInputs.
          buildInputs = [ ];

          # Panics in release still produce backtraces for bug reports.
          CARGO_BUILD_RUSTFLAGS = "-C force-unwind-tables=yes";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        nzbkodi-engine = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            meta = with pkgs.lib; {
              description = "On-demand Usenet download engine for the nzbkodi Kodi addon";
              homepage = "https://github.com/IGI-111/nzbkodi";
              license = licenses.mit;
              mainProgram = "nzbkodi-engine";
              platforms = platforms.linux;
            };
          }
        );

        # The Kodi python addon, packaged for `kodi.withPackages`.
        nzbkodi-addon = pkgs.stdenv.mkDerivation {
          pname = "nzbkodi-addon";
          version = "0.1.0";
          src = ./addon/plugin.video.nzbkodi;

          # stdlib-only python: nothing to build, just install files.
          dontBuild = true;

          installPhase = ''
            runHook preInstall
            mkdir -p $out/share/kodi/addons/plugin.video.nzbkodi
            cp -r ./. $out/share/kodi/addons/plugin.video.nzbkodi/
            # Tests and caches are not part of the addon.
            rm -rf $out/share/kodi/addons/plugin.video.nzbkodi/tests
            find $out -name __pycache__ -type d -exec rm -rf {} +
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "nzbkodi Kodi addon (plugin.video.nzbkodi)";
            homepage = "https://github.com/IGI-111/nzbkodi";
            license = licenses.mit;
            platforms = platforms.linux;
          };
        };
      in
      {
        packages = {
          default = nzbkodi-engine;
          inherit nzbkodi-engine nzbkodi-addon;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = nzbkodi-engine;
          name = "nzbkodi-engine";
        };

        # `nix develop`: toolchain + a few extras. cargo resolves the
        # TurboNZB git dep from the local repo (no network needed).
        devShells.default = craneLib.devShell (
          commonArgs
          // {
            packages = with pkgs; [
              cargo-nextest
              nixpkgs-fmt
            ];
          }
        );

        checks = {
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          fmt = craneLib.cargoFmt commonArgs;
          test = craneLib.cargoTest (
            commonArgs
            // {
              inherit cargoArtifacts;
            }
          );
        };
      }
    );
}