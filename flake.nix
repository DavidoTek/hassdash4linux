{
  description = "hassdash4linux – Home Assistant in your Linux system tray";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-appimage = {
      url = "github:ralismark/nix-appimage";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      nix-appimage,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        system: pkgs:
        let
          hassdash4linux = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit hassdash4linux;
          default = hassdash4linux;
          appimage = pkgs.callPackage ./nix/appimage.nix {
            inherit hassdash4linux;
            mkAppImage = nix-appimage.lib.${system}.mkAppImage;
          };
        }
      );

      overlays.default = final: _prev: {
        hassdash4linux = final.callPackage ./nix/package.nix { };
      };

      nixosModules.default = import ./nix/module.nix self;
      homeManagerModules.default = import ./nix/hm-module.nix self;

      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.hassdash4linux ];
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              cargo-deny
              cargo-audit
              librsvg # data/icons/render.sh
              appstream # appstreamcli validate
              desktop-file-utils
              flatpak-builder
              gettext
            ];
          };
        }
      );

      checks = forAllSystems (
        system: pkgs:
        let
          pkg = self.packages.${system}.hassdash4linux;
        in
        {
          package = pkg;
          clippy = pkg.overrideAttrs (old: {
            pname = "hassdash4linux-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = "cargo clippy --workspace --all-targets --offline -- -D warnings";
            installPhase = "touch $out";
            doCheck = false;
            dontFixup = true;
          });
          nixos-module = pkgs.testers.runNixOSTest {
            name = "hassdash4linux-module";
            nodes.machine = {
              imports = [ self.nixosModules.default ];
              programs.hassdash4linux = {
                enable = true;
                autostart = true;
              };
              users.users.alice.isNormalUser = true;
            };
            testScript = ''
              machine.wait_for_unit("multi-user.target")
              machine.succeed("hassdash4linux --version")
              machine.succeed("test -f /etc/systemd/user/hassdash4linux.service")
              machine.succeed("test -e /etc/systemd/user/graphical-session.target.wants/hassdash4linux.service")
              machine.succeed("test -f /run/current-system/sw/share/applications/net.davidotek.hassdash4linux.desktop")
            '';
          };
        }
      );

      formatter = forAllSystems (_: pkgs: pkgs.nixfmt);
    };
}
