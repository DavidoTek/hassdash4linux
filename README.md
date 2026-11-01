# hassdash4linux

Home Assistant in your Linux system tray (GNOME, KDE Plasma and other desktops
with StatusNotifierItem support).

- **Run** automations, scenes, scripts and buttons with one click
- **Control** lights (on/off, brightness, color temperature, color) and outlets/switches
- **See** live sensor values right in the menu (e.g. `Outside: 15.6 °C`)
- **Receive** Home Assistant notifications on your desktop, including action buttons
- **Small**: the tray process uses about 10 MB of memory and practically no CPU;
  the settings window is a separate process that exits when you close it

<p align="center"><img src="data/icons/scalable/net.davidotek.hassdash4linux.svg" width="96" alt=""></p>

## Installation

| Channel | How |
|---|---|
| **AppImage** | Download `hassdash4linux-<version>-x86_64.AppImage`, make it executable, run it. Needs FUSE 3 (`fuse3` package) and unprivileged user namespaces; without FUSE, run it with `APPIMAGE_EXTRACT_AND_RUN=1`. |
| **Flatpak** | See [Building the Flatpak](#building-the-flatpak). |
| **NixOS / home-manager** | Use the flake (below). |

### GNOME

GNOME does not show tray icons without an extension. Install and enable
**AppIndicator and KStatusNotifierItem Support** (on Fedora:
`gnome-shell-extension-appindicator`; on Fedora Atomic, install it from
extensions.gnome.org or with `rpm-ostree install`). Ubuntu ships it enabled.
If no tray is found, hassdash4linux opens its window and tells you so.

### NixOS

```nix
{
  inputs.hassdash4linux.url = "github:<owner>/hassdash4linux"; # or a path
  outputs = { nixpkgs, hassdash4linux, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        hassdash4linux.nixosModules.default
        {
          programs.hassdash4linux = {
            enable = true;
            autostart = true; # systemd user service in every graphical session
          };
        }
      ];
    };
  };
}
```

home-manager users import `hassdash4linux.homeManagerModules.default` and set
the same `programs.hassdash4linux` options. Without a module,
`nix profile install .#default` works too.

## Usage

1. Start **hassdash4linux**. On first start, the window asks for your Home
   Assistant address (e.g. `homeassistant.local:8123` or `https://ha.example.org`).
2. Click **Log in with browser** and log in as usual (multi-factor authentication
   works). Alternatively, paste a long-lived access token.
3. Add entries to the tray menu. Reorder them with the arrows; click an entry
   to change its label.
4. Enable **Start on login** in the preferences (main menu → Preferences).

Clicking the tray icon opens the menu. Starting hassdash4linux again (e.g. from
the app grid) opens the settings.

### Notifications

When "Receive notifications" is on, hassdash4linux registers itself as a
companion device. Notifications only arrive while hassdash4linux is running.
Send them like to the phone apps:

```yaml
action: notify.mobile_app_<device_name>
data:
  title: Washing machine
  message: The laundry is done.
  data:
    tag: laundry            # a newer notification with the same tag replaces it
    url: /lovelace/home     # opened when the notification is clicked
    actions:                # up to 3 buttons
      - action: SNOOZE      # fires mobile_app_notification_action in Home Assistant
        title: Remind me later
      - action: URI
        title: Open camera
        uri: /lovelace/cameras
```

`message: clear_notification` with a `tag` closes a notification. Commands
(`command_*`) are ignored on purpose: Home Assistant can never run programs on
your computer through hassdash4linux.

## Security

- Log in with a **dedicated Home Assistant user without administrator rights**.
  hassdash4linux only uses APIs that work for normal users.
- The login (refresh token or access token) and the companion device's webhook
  credentials are stored in the **system keyring**, never in the configuration file.
- The browser login uses OAuth2 with PKCE; disconnecting revokes the login.
- Certificates are always verified. A self-signed certificate is only accepted
  after you compare and explicitly trust its fingerprint. Unencrypted `http://`
  is only allowed for addresses in your local network.
- The configuration never contains service names: what a click does is derived
  from the entity type (automation → `automation.trigger`, scene →
  `scene.turn_on`, …). A tampered configuration cannot call other services.
- Everything Home Assistant sends (names, states, notifications) is shown as
  plain text; notification links only open when you click them, and only
  `http(s)` links.

## Building

```sh
nix develop              # Rust toolchain, GTK 4, libadwaita and tools
cargo build --release    # target/release/hassdash4linux{,-ui}
cargo test --workspace
nix build                # the Nix package
nix build .#appimage     # the AppImage
```

Without Nix you need Rust ≥ 1.88, GTK ≥ 4.14 and libadwaita ≥ 1.5 development files.

### Building the Flatpak

```sh
flatpak-builder --user --install --force-clean --install-deps-from=flathub \
  build-aux/flatpak/build build-aux/flatpak/net.davidotek.hassdash4linux.yml
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
