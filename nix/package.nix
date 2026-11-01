{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  desktop-file-utils,
  gtk4,
  libadwaita,
}:

let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "hassdash4linux";
  inherit (cargoToml.workspace.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../data
      ../po
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
    desktop-file-utils
  ];
  buildInputs = [
    gtk4
    libadwaita
  ];

  # Only the GTK binary needs the GApps wrapper; the tray daemon stays a
  # plain binary (it does not use GTK at all).
  dontWrapGApps = true;

  postInstall = ''
    install -Dm644 data/net.davidotek.hassdash4linux.desktop -t $out/share/applications
    install -Dm644 data/net.davidotek.hassdash4linux.metainfo.xml -t $out/share/metainfo
    install -Dm644 data/icons/scalable/net.davidotek.hassdash4linux.svg -t $out/share/icons/hicolor/scalable/apps
    install -Dm644 data/icons/symbolic/net.davidotek.hassdash4linux-symbolic.svg -t $out/share/icons/hicolor/symbolic/apps
    for s in 16 22 24 32 48 64 128 256 512; do
      install -Dm644 data/icons/hicolor/''${s}x''${s}/apps/net.davidotek.hassdash4linux.png \
        -t $out/share/icons/hicolor/''${s}x''${s}/apps
    done
    desktop-file-validate $out/share/applications/net.davidotek.hassdash4linux.desktop
  '';

  postFixup = ''
    wrapGApp $out/bin/hassdash4linux-ui
  '';

  meta = {
    description = "Home Assistant in your Linux system tray";
    longDescription = ''
      Run automations, scenes and scripts, control lights and outlets, see
      sensor values and receive Home Assistant notifications from a tray icon.
    '';
    homepage = "https://github.com/DavidoTek/hassdash4linux";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "hassdash4linux";
  };
}
