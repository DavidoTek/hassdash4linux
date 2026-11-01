{
  lib,
  stdenv,
  callPackage,
  gtk4,
  libadwaita,
  wrapGAppsHook4,
  hassdash4linux,
  mkAppImage,
}:

# A single-file AppImage containing the complete Nix closure (glibc, GTK,
# Mesa …), so it runs on any distribution. The entry point is the tray
# daemon; it starts the bundled UI binary from its own directory.
#
# To keep the file small, GTK is built without features hassdash4linux does
# not use: the GStreamer media backend (which drags in Python, ffmpeg, x265,
# speech synthesis …), tracker search, CUPS printing and Broadway.
let
  gtk4Slim =
    (gtk4.override {
      trackerSupport = false;
      cupsSupport = false;
      broadwaySupport = false;
    }).overrideAttrs
      (old: {
        buildInputs = lib.filter (p: !(lib.hasPrefix "gst-plugins" (p.pname or ""))) old.buildInputs;
        mesonFlags = old.mesonFlags ++ [ "-Dmedia-gstreamer=disabled" ];
      });
  libadwaitaSlim = libadwaita.override { gtk4 = gtk4Slim; };
  slim = callPackage ./package.nix {
    gtk4 = gtk4Slim;
    libadwaita = libadwaitaSlim;
    wrapGAppsHook4 = wrapGAppsHook4.override { gtk3 = gtk4Slim; };
  };
in
mkAppImage {
  program = lib.getExe slim;
  pname = "hassdash4linux";
  name = "hassdash4linux-${hassdash4linux.version}-${stdenv.hostPlatform.uname.processor}.AppImage";
  squashfsArgs = [
    "-comp"
    "zstd"
    "-Xcompression-level"
    "19"
  ];
}
