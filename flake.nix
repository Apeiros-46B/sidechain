{
  description = "sidechain: music mirroring tool";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }: let
    supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
    forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    pkgsFor = system: nixpkgs.legacyPackages.${system};
  in {

    packages = forAllSystems (system: {
      default = (pkgsFor (system)).callPackage ./package.nix {};
    });

    devShells = forAllSystems (system:
      let pkgs = pkgsFor system; in {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = with pkgs; [ rust-analyzer clippy rustfmt ];
        };
      }
    );

    nixosModules.default = { pkgs, ... }: {
      imports = [ ./module.nix ];
      nixpkgs.overlays = [
        (final: prev: {
          sidechain = self.packages.${prev.system}.default;
        })
      ];
    };

  };
}
