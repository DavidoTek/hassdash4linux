# AGENTS.md — working on hassdash4linux

Guidance for coding agents and contributors. Read this before changing code.
User-facing documentation is in `README.md`.

## What this is

A Home Assistant tray app for Linux, written in Rust. App ID
`net.davidotek.hassdash4linux`, license GPL-3.0-or-later. Distributed as an
AppImage, a Flatpak and a Nix flake (NixOS/home-manager modules).

## Architecture

There are two binaries, and the split is deliberate: initialising GTK costs 60–150 MB that is
never returned, while the always-running tray process stays around 10 MB.

```
hassdash4linux (crates/hd-daemon)            hassdash4linux-ui (crates/hd-ui)
always running, NO GTK                       GTK4 + libadwaita, started on demand,
• tray: ksni (StatusNotifierItem/DBusMenu)   exits when its last window closes
• one HA WebSocket session                   • main window: connect / entries /
• desktop notifications (fd.o D-Bus)           pickers / preferences
• mobile_app push channel                    • entity windows (light, switch)
• D-Bus: net.davidotek.hassdash4linux.Daemon • own short-lived HA session
      └─ spawns hassdash4linux-ui --settings | --entity <id> [--no-tray-host]
```

| Crate | Contents |
|---|---|
| `hd-core` | Config (`config.toml`), entry model and the **service allowlist** (`entry.rs`), keyring (`secrets.rs`, oo7), URL policy (`server.rs`), autostart for all install types, D-Bus interface names/proxy (`ipc.rs`), translations (`i18n.rs` + `build.rs`) |
| `hd-ha` | Home Assistant client: TLS + pinning (`tls.rs`), REST without redirects (`http.rs`), OAuth+PKCE loopback login (`auth.rs`), WebSocket session actor (`ws.rs`), `subscribe_entities` decoder (`entities.rs`), state formatting (`format.rs`), mobile_app registration and encrypted webhooks (`mobile_app.rs`), `Connector` (`client.rs`) |
| `hd-daemon` | Main loop (`app.rs`), tray menu (`tray.rs`), notifications (`notify.rs`), push payload parsing (`push.rs`), D-Bus service and logind (`dbus.rs`), embedded tray icons (`icons.rs`) |
| `hd-ui` | Windows; `rt.rs` bridges GTK and tokio (`on_tokio`), `ui.rs` holds shared state |

Data flow:
- **Config**: only the UI writes `~/.config/hassdash4linux/config.toml`, atomically and with mode 0600. It then calls `ReloadConfig()` on the daemon.
- **Secrets**: refresh token or long-lived token, webhook ID and webhook secret live only in the keyring. They are keyed by `application`, `server` (URL) and `kind`.
- **Window launches**: GtkApplication uniqueness forwards a second `hassdash4linux-ui` invocation to the running instance.

## Commands

The dev machine is Fedora Atomic. GTK headers come from Nix and flakes are not enabled globally, so prefix commands with
`nix --extra-experimental-features 'nix-command flakes'`, or enable flakes.

```sh
nix develop                          # toolchain + GTK/libadwaita + cargo-deny/audit, librsvg, appstream …
cargo build --workspace              # hd-core/hd-ha/hd-daemon also build outside the dev shell
cargo test --workspace               # unit tests (incl. translation completeness)
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check                     # advisories, licenses, bans, sources (deny.toml)
nix build                            # package (runs the tests in the sandbox)
nix build .#appimage                 # AppImage (slim GTK; first build compiles GTK, ~20 min)
nix flake check                      # package, clippy, NixOS VM test of the module
```

Nix flakes only see files tracked by git, so run `git add` on new files before `nix build`.

### Live test against a real Home Assistant

```sh
testing/run-ha.sh                    # throwaway HA in Docker/Podman on 127.0.0.1:8123 (demo entities)
HD_TEST_HA_URL=http://127.0.0.1:8123 cargo test -p hd-ha --test live_ha -- --ignored --nocapture
testing/run-ha.sh stop
```

The test onboards the instance (user `admin`) and creates a **non-admin** user
`tray`. It then covers:
- browser login with PKCE (the login form is driven like the HA frontend does)
- `get_states`, the registries, `subscribe_entities` and `call_service`
- mobile_app registration
- push notifications with confirmation
- encrypted webhooks
- long-lived tokens and revocation

Run it after touching `hd-ha`. Never point it at a real instance.

For manual UI tests:
- Run `XDG_CONFIG_HOME=<tmp dir> target/debug/hassdash4linux` against the test instance.
- Secrets go to the real keyring under `server = http://127.0.0.1:8123`; disconnecting in the UI removes them.
- The tray menu can be inspected without clicking: `gdbus call --session --dest org.kde.StatusNotifierItem-<pid>-1 --object-path /MenuBar --method com.canonical.dbusmenu.GetLayout -- 0 -1 '[]'`.

## Invariants — do not break these

Security:
1. **Services come from the allowlist only.** `hd_core::entry::action_service` (tray actions) and `entity_service_allowed` (entity windows) are the only places that decide which HA service is called. The config never contains service names, and `Entry` uses `deny_unknown_fields`.
2. **No remote code execution.** Notifications never run commands. `command_*` and other special messages are ignored (`push.rs`).
3. **HA content is untrusted plain text.**
   - Use `set_text`/`set_title` with `use_markup(false)`, never markup.
   - Escape `_` in DBusMenu labels.
   - Escape markup in notification bodies and the SNI tooltip.
   - Cap lengths.
   - Open URLs only on click, and only `http(s)` or HA-relative paths.
4. **TLS**:
   - System roots, or an explicit SHA-256 certificate pin chosen by the user.
   - Never add an "ignore certificate errors" option.
   - HTTP redirects stay disabled.
   - Plain `http://` is only for local hosts (`server.rs`).
5. **Secrets** stay in the keyring, wrapped in `Zeroizing`.
   - Never log tokens, the webhook ID or URLs that contain it.
   - Keep the server URL in the lookup attributes, so that editing the URL in the config can never send a token to another host.
6. **Minimal D-Bus API**: `ReloadConfig`, `ShowSettings` and `Quit`, all without arguments. Don't add methods that could control HA.
7. **Non-admin only**: use only WebSocket/REST APIs that work for non-admin users. The live test runs as a non-admin user and enforces this.
8. `#![forbid(unsafe_code)]` in every crate.

Resources:
- `hd-daemon` must not depend on GTK/glib. Check with `cargo tree -p hd-daemon | grep -i gtk`, which should print nothing.
- Idle targets: < 20 MB RSS and ~0 % CPU. The measured value is about 9.7 MB RSS (1.3 MB anonymous) with 1 tick of CPU per 90 s while connected.
- Avoid busy loops and polling. The daemon wakes up only for WebSocket messages and the 30 s ping.

## Conventions

- **Translations**: wrap every user-visible string in `hd_core::i18n::tr("…")`, or `trf("… {name} …", &[("name", v)])` when it has placeholders.
  - The English text is the message ID.
  - `cargo test -p hd-core` fails until the string has an entry in `po/de.po` and the placeholders match.
  - `build.rs` compiles `po/*.po` into the binary, so no `.mo` files are installed.
  - A new language is just a new `po/<lang>.po`.
- **Async in the UI**: never call `hd-ha` directly from GTK code. Use `glib::spawn_future_local(async { … rt::on_tokio(async move { … }).await … })`. Timers and sockets need the tokio runtime.
- **WebSocket**: one `Session` per process. `Session::call` / `subscribe` (dropping a `Subscription` unsubscribes). Reconnect logic lives in the consumer (`connection_task` in the daemon, `HaLink::session` in the UI).
- **Errors** shown to users are `HaError`'s translated `Display`.
- **Icons**: edit the SVGs in `data/icons/`, then run `data/icons/render.sh`, which needs `rsvg-convert` from `nix develop`. Tray icons are embedded pixmaps, because AppImage/Flatpak cannot rely on the host icon theme. They are amber when connected, grey when disconnected and red on error. Do not use Home Assistant's logo or brand colours.
- **Formatting**: `cargo fmt` with the default rustfmt config.

## Packaging notes

- **Nix** (`nix/`):
  - `package.nix` wraps only the UI with `wrapGApp`; the daemon stays unwrapped.
  - `module.nix` and `hm-module.nix` provide `programs.hassdash4linux.{enable,package,autostart}`. Autostart is a systemd user unit, and the app detects it via systemd's `GetUnitFileState`, which makes its "Start on login" switch read-only.
- **AppImage** (`nix/appimage.nix`):
  - Built with ralismark/nix-appimage (userns-chroot AppRun, so it needs unprivileged user namespaces).
  - GTK is rebuilt without GStreamer, tracker, CUPS or Broadway; with the full GTK closure the AppImage was 372 MB.
  - The daemon sets `GSK_RENDERER=cairo` for the UI when `$APPIMAGE` is set, because the bundled Mesa cannot use proprietary GPU drivers.
  - Autostart writes a desktop file pointing to `$APPIMAGE` and repairs it when the file moves.
  - The AppImage is about 78 MB. It needs `fusermount3` on the host, or `APPIMAGE_EXTRACT_AND_RUN=1`. The AppImage runtime process adds about 10 MB RSS next to the daemon.
- **Flatpak** (`build-aux/flatpak/`):
  - Built on GNOME 51.
  - After every `Cargo.lock` change, run `build-aux/flatpak/gen-cargo-sources.py`. It is a small, dependency-free replacement for flatpak-cargo-generator.
  - Inside Flatpak the tray registers with its unique bus name (`disable_dbus_name`), because the D-Bus proxy can't allow `org.kde.StatusNotifierItem-<pid>` names.
  - Autostart goes through the Background portal; its state can't be queried, so it is cached in `preferences.flatpak_autostart`.
- **Versions**: bump `workspace.package.version` in `Cargo.toml` and add a `<release>` entry to `data/net.davidotek.hassdash4linux.metainfo.xml`.

## Behaviour worth knowing

- **Expired or revoked login**: the daemon stops reconnecting and shows a red tray state. The main page then offers "Log in again", which opens the connection screen with the address pre-filled; entries and the device ID are kept.
- **Companion device deleted in HA**: HA answers the push-channel subscription with `not_found` (webhooks return an empty 200 until HA restarts, or 410). The daemon then drops the registration and shows a hint. "Register again" in the preferences re-registers with the same device ID.
- **Config writes**: the daemon never writes the config. Anything it learns at runtime, such as a deleted registration, is only reflected in the tray.

## Known gaps and follow-ups

- **zbus 6**: `deny.toml` ignores RUSTSEC-2026-0370/0371/0372 (zbus 5 helper crates merged into zbus 6). Upgrade once ksni and oo7 support zbus 6.
- **Dependency versions**: some dependencies are a minor version behind on purpose (gtk4-rs 0.10, libadwaita-rs 0.8, reqwest 0.12, oo7 0.5). Upgrade deliberately and re-run the live test.
- **No CI yet**: a workflow should run `nix flake check`, `cargo deny check` and the Flatpak build, with actions pinned by commit SHA.
- **Flathub metadata**: the metainfo needs a homepage URL and screenshots before submission.
- **Untested**: the AppImage and Flatpak have only been tested on x86_64 Fedora Atomic (GNOME). KDE Plasma still needs a manual check: left click should open the menu (`MENU_ON_ACTIVATE`).
- **Device name**: the companion device name can't be changed after registration from the UI. Changing it means disconnecting and reconnecting.
- **Not implemented**: sending desktop sensors (battery, active app …) to Home Assistant, more entity domains (cover, climate, media player), and internal/external URL failover.
