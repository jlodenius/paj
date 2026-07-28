{
  description = "Local session discovery, messaging, and editor bridge for Pi coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {
    self,
    nixpkgs,
  }: let
    forAllSystems = nixpkgs.lib.genAttrs [
      "aarch64-linux"
      "x86_64-linux"
    ];
  in {
    packages = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "paj";
        version = "0.1.0";
        src = self;

        cargoLock.lockFile = ./Cargo.lock;

        meta = {
          description = "Local session discovery, messaging, and editor bridge for Pi coding agents";
          homepage = "https://github.com/jlodenius/paj";
          mainProgram = "paj";
          platforms = pkgs.lib.platforms.linux;
        };
      };
    });
  };
}
