# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
{
  description = "ClipSync CLI and relay";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      forAll = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAll (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "clipsync";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # The check phase runs `cargo test`, and buildRustPackage's
            # checkType defaults to buildType ("release"). Test builds enable
            # clipsync-storage/test-isolated-keychain, which refuses to compile
            # in the release profile on darwin, so test in the debug profile.
            # The installed binaries are still built with buildType "release".
            checkType = "debug";
          };
        });
    };
}
