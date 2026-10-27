# Keychron Assistant for Linux

<img src="icons/128x128.png" align="right" width="96" alt="">

Unofficial Linux port of [Keychron Assistant](https://www.keychron.com/). Share one keyboard and mouse between
a Linux PC and computers running Keychron Assistant for Windows or macOS (move the cursor over the screen edge),
and send files between them.

It speaks the same network protocol as Keychron's apps (documented in [`re/PROTOCOL.md`](re/PROTOCOL.md)) and
uses Keychron's own interface, so it looks and behaves like the original.

> Not affiliated with or endorsed by Keychron. "Keychron" is a trademark of its owner.

## Keychron's interface

The interface is Keychron's, so it isn't included in this repository or in the release packages. On first
start the app shows a setup page that downloads the official Windows installer from Keychron
(`https://launcher.keychron.com/static/assist/KeychronAssist-1.1.6.zip`), checks its SHA-256, and unpacks only
the interface files into `~/.local/share/keychron-assist-linux/`. Nothing from the installer is run.

Offline: download that zip (or the `.exe` inside it) yourself and pick it with "Use a downloaded file…", or run
`keychron-assist-linux --install-ui KeychronAssist-1.1.6.zip`.

## Features

| Feature | Status |
|---|---|
| Find computers on the LAN, pair from either side, trusted devices, automatic connection | works with Windows |
| Mouse and keyboard sharing, both directions | works with Windows |
| File transfer, both directions | works with Windows |
| Screen position, file-transfer switch, disconnect/lock shortcuts (Shift+Esc+key) | works |
| Multiple monitors (the outer edges of the whole layout are used) | works |
| Quick Start (launch apps with keyboard keys) | ported, needs more testing |
| macOS peers | not tested; same protocol as the macOS app |
| Linux ↔ Linux | not tested; same code paths |
| Dragging files onto the window | not supported, use "Select Files" |

Moving the mouse between computers needs one of these desktops (everything else works on any desktop):

| Desktop | How |
|---|---|
| Hyprland | IPC socket (classic and 0.56+ Lua syntax) |
| KDE Plasma 6, Wayland | a small KWin script reports the cursor and screen layout over D-Bus; tested on a headless KWin so far |
| Any X11 session | RandR, `XQueryPointer`, `XWarpPointer` |
| GNOME Wayland, other Wayland compositors | not yet |

`keychron-assist-linux --screen-probe` prints the detected desktop, monitors and cursor position.
`KEYCHRON_SCREEN=hyprland|kwin|x11|none` overrides the detection.

## Install

Packages are on the [releases page](https://github.com/Tymon3310/keychron-assist-linux/releases): AppImage,
`.deb`, `.rpm`, and `keychron-assist-linux-bin` on the AUR.

Permissions:

- **Reading your keyboard and mouse** (to notice the screen edge and forward input): your user must be in the
  `input` group: `sudo usermod -aG input $USER`, then log out and back in.
- **The virtual mouse/keyboard** (`/dev/uinput`, to replay input from the other computer): the `.deb`/`.rpm`/AUR
  packages install a udev rule for this. With the AppImage, copy
  [`packaging/60-keychron-assist-linux.rules`](packaging/60-keychron-assist-linux.rules) to
  `/etc/udev/rules.d/` and run `sudo udevadm control --reload && sudo udevadm trigger`.
- **Firewall:** allow UDP 7654 (discovery) and UDP 40001 (session) on the LAN.

`--background` starts the app hidden in the tray (used for "start at login").

## Build

Needs Rust, and WebKitGTK 4.1, GTK 3 and libayatana-appindicator development files.

```sh
cargo build --release
./target/release/keychron-assist-linux
# packages (AppImage, deb, rpm):
npx @tauri-apps/cli@2 build
```

Settings and logs are in `~/.Keychron_Screen/`, as with the original app (`log.json`; set `KEYCHRON_DEBUG=1`
for debug lines). Received files go to `~/Downloads` unless changed in the app.

## Layout

- `src/rdp/`: cross-screen. `wire.rs` (protocol types), `mod.rs` (session, discovery, pairing), `files.rs` (file
  transfer), `capture.rs` (controlling side), `inject.rs` (controlled side), `keymap.rs` (Linux/Windows/macOS key
  codes), `screen/` (desktop backends and the edge math), `commands.rs` (interface commands)
- `src/uipack.rs`: unpacks Keychron's interface from the official installer and serves it
- `src/commands.rs`, `src/service/`, `src/linux/`: Quick Start, the launcher WebSocket, key watcher
- `frontend/`: the setup page shown until the interface is installed
- `packaging/`: icon source, desktop entry, udev rule
- `re/`: protocol notes and packet captures from the Windows app

## License

GPL-3.0-or-later for the code in this repository. Keychron's interface, downloaded at first run, remains
Keychron's.
