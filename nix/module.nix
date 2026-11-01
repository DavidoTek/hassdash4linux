# NixOS module: `programs.hassdash4linux`.
self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.hassdash4linux;
in
{
  options.programs.hassdash4linux = {
    enable = lib.mkEnableOption "hassdash4linux, a Home Assistant tray icon";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "hassdash4linux.packages.\${system}.default";
      description = "The hassdash4linux package to use.";
    };

    autostart = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Start the tray icon in every graphical session (systemd user service
        bound to graphical-session.target). The "Start on login" switch in
        the app then shows that autostart is managed by Nix.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.user.services.hassdash4linux = lib.mkIf cfg.autostart {
      description = "hassdash4linux Home Assistant tray icon";
      wantedBy = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      # xdg-open and friends for links in notifications.
      path = [ "/run/current-system/sw" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --autostart";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}
