# cosmic-ext-workspaces-bar

A bar showing live workspace previews for the [COSMIC desktop](https://github.com/pop-os/cosmic-epoch).

Each workspace is rendered as a small, continuously-updating thumbnail of its
actual contents, so you can see what's happening on a workspace before
switching to it. The bar can also show MPRIS media controls (play/pause,
skip, title/artist, album art) for whatever's currently playing.

## Features

- **Live workspace previews** — each workspace shows a real-time thumbnail
  (via COSMIC's screencopy protocol), not just a static icon or label.
- **Click or scroll to switch** workspaces; right-click for a context menu
  (rename a workspace, open settings).
- **Flexible layout** — attach the bar to any screen edge, choose its
  thickness, and either stretch it along the full edge or shrink it to fit
  its content, centered.
- **Autohide**, similar to the COSMIC dock, sliding off-screen until hovered.
- **Thumbnail clipping** — crop the bar's own reserved screen strip (or every
  bar/panel/dock strip) out of the previews so they aren't obscured by
  bars overlapping the workspace.
- **Media controls** (optional, MPRIS2) — previous/play-pause/next, marquee
  title and artist, album art (local or remote), click to raise the player's
  window and switch to its workspace, scroll to adjust volume, and a
  preferred-player override for when multiple players are running.
- **Appearance customization** — background color, blur, corner radius,
  active-workspace border, hover color, and text color, independent of the
  system theme.

## Requirements

COSMIC desktop (cosmic-comp) only — the bar relies on COSMIC-specific
Wayland protocols and will not work on other compositors or desktops.

## Installing

See [INSTALL.md](INSTALL.md) for build and installation instructions.

## Usage

Once installed, start it directly or add it to your COSMIC autostart apps
(see INSTALL.md). Right-click the bar, or run
`cosmic-ext-workspaces-bar --settings`, to open the settings window.

## License

[GPL-3.0-only](LICENSE.md)
