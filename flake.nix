{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    utils,
    naersk,
  }:
    utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
      naersk-lib = pkgs.callPackage naersk {};
    in {
      packages = rec {
        garage-operator = naersk-lib.buildPackage {
          pname = "garage-operator";
          version = "0.2.0";
          src = ./.;

          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
          buildInputs = with pkgs; [
            openssl
          ];
        };
        default = garage-operator;

        container = pkgs.dockerTools.buildLayeredImage {
          name = "ghcr.io/simple-rack/garage-operator";
          tag = "latest";
          contents = [garage-operator];
          config = {
            Entrypoint = [
              "${garage-operator}/bin/operator"
            ];
            Env = [
              "RUST_LOG=info"
            ];
          };
        };
      };

      devShell = with pkgs;
        mkShell {
          nativeBuildInputs = [pkg-config openssl];

          buildInputs =
            [cargo rustc rustfmt pre-commit rustPackages.clippy]
            ++ [
              alejandra
              act
              docker-machine-kvm2
              kubernetes-helm
              httpie
              kubectl
              minikube
            ];

          RUST_SRC_PATH = rustPlatform.rustLibSrc;
        };
    });
}
