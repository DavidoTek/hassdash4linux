# home-manager module: `programs.hassdash4linux`.
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
      description = "Start the tray icon in every graphical session (systemd user service).";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.hassdash4linux = lib.mkIf cfg.autostart {
      Unit = {
        Description = "hassdash4linux Home Assistant tray icon";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} --autostart";
        Restart = "on-failure";
        RestartSec = 5;
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
