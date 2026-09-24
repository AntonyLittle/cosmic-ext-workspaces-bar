# Installing cosmic-ext-workspaces-bar

A bar showing live workspace previews for the COSMIC desktop.

## Requirements

- **COSMIC desktop** (cosmic-comp). The bar relies on COSMIC-specific Wayland   protocols (`ext-workspace`, COSMIC screencopy) and will not work on other compositors.
- **Rust** 1.85 or newer (the project uses the 2024 edition). Install via [rustup](https://rustup.rs) if your distribution's toolchain is older.
- **Build tools and libraries** required by libcosmic:

  Debian/Ubuntu/Pop!_OS:

  ```sh
  sudo apt install build-essential git pkg-config libwayland-dev libxkbcommon-dev libgbm-dev libegl1-mesa-dev
  ```

  Fedora:

  ```sh
  sudo dnf install gcc git pkg-config wayland-devel libxkbcommon-devel mesa-libgbm-devel mesa-libEGL-devel
  ```

  Arch:

  ```sh
  sudo pacman -S base-devel git wayland libxkbcommon mesa
  ```

## Building

```sh
git clone https://github.com/AntonyLittle/cosmic-ext-workspaces-bar.git
cd cosmic-ext-workspaces-bar
cargo build --release
```

The first build fetches and compiles libcosmic from git, which takes a while.
The binary is produced at `target/release/cosmic-ext-workspaces-bar`.

## Installing

For the current user:

```sh
install -Dm755 target/release/cosmic-ext-workspaces-bar ~/.local/bin/cosmic-ext-workspaces-bar
install -Dm644 data/cosmic-ext-workspaces-bar.desktop ~/.local/share/applications/cosmic-ext-workspaces-bar.desktop
```

Or system-wide:

```sh
sudo install -Dm755 target/release/cosmic-ext-workspaces-bar /usr/local/bin/cosmic-ext-workspaces-bar
sudo install -Dm644 data/cosmic-ext-workspaces-bar.desktop /usr/local/share/applications/cosmic-ext-workspaces-bar.desktop
```

Make sure the install location is on your `PATH` (the desktop file launches the binary by name).

## Starting the bar

Run it directly to try it out:

```sh
cosmic-ext-workspaces-bar
```

To start it automatically when you log in to COSMIC, add the desktop file to your autostart directory:

```sh
install -Dm644 data/cosmic-ext-workspaces-bar.desktop ~/.config/autostart/cosmic-ext-workspaces-bar.desktop
```

## Settings

Open the settings window either by right-clicking the bar, or from a terminal:

```sh
cosmic-ext-workspaces-bar --settings
```

The application is single-instance: running the command again while the bar is already running just opens the settings window.

Configuration is stored via cosmic-config under
`~/.config/cosmic/com.github.antony.CosmicWorkspacesBar/`.

## Uninstalling

```sh
rm ~/.local/bin/cosmic-ext-workspaces-bar
rm ~/.local/share/applications/cosmic-ext-workspaces-bar.desktop
rm ~/.config/autostart/cosmic-ext-workspaces-bar.desktop   # if installed
rm -r ~/.config/cosmic/com.github.antony.CosmicWorkspacesBar  # config (optional)
```
