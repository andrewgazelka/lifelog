{
  description = "Continuous local activity log for macOS: app sampler + Screen Time ingest + phone event API, into one queryable SQLite db";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # The sampler talks to AppKit/CoreGraphics, so darwin only.
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        lifelog = pkgs.rustPlatform.buildRustPackage {
          pname = "lifelog";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = nixpkgs.lib.cleanSource self;
          cargoLock.lockFile = ./Cargo.lock;
          meta.mainProgram = "lifelog";
        };
        default = lifelog;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            sqlite
          ];
        };
      });
    };
}
