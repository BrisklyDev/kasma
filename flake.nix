{
  description = "Kasma - a multi-connection download accelerator";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
    in
    {
      packages = forAllSystems (system: rec {
        kasma = nixpkgs.legacyPackages.${system}.callPackage ./package.nix { };
        default = kasma;
      });

      overlays.default = final: prev: {
        kasma = final.callPackage ./package.nix { };
      };

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.kasma ];
            packages = [
              pkgs.rust-analyzer
              pkgs.clippy
              pkgs.rustfmt
            ];
          };
        }
      );
    };
}
