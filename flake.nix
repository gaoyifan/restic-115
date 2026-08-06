{
  description = "Restic REST backend for 115 Open Platform storage";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = {
    self,
    nixpkgs,
    ...
  }: let
    systems = [
      "aarch64-linux"
      "x86_64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    packages = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "restic-115";
        version = "0.2.6";
        src = ./.;

        cargoHash = "sha256-NmDlAH35tghCyTIOLBekfNYidvFJD5bZ3CYvijK3tDM=";

        meta = {
          description = "Restic REST backend for 115 Open Platform storage";
          homepage = "https://github.com/gaoyifan/restic-115";
          license = pkgs.lib.licenses.mit;
          mainProgram = "restic-115";
        };
      };
    });

    devShells = forAllSystems (system: {
      default = nixpkgs.legacyPackages.${system}.mkShell {
        packages = with nixpkgs.legacyPackages.${system}; [
          cargo
          rustc
          rustfmt
        ];
      };
    });

    formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.alejandra);

    nixosModules.default = import ./nixos-module.nix {inherit self;};
  };
}
