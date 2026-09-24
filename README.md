# cosmic-ext-workspaces-bar

A bar showing live workspace previews for the [COSMIC desktop](https://github.com/pop-os/cosmic-epoch).

Each workspace is rendered as a small, continuously-updating thumbnail of its
actual contents, so you can see what's happening on a workspace before
switching to it. The bar can also show MPRIS media controls (play/pause,
skip, title/artist, album art) for whatever's currently playing.

## Authors note

This app is entirely vibe coded. I hate that it works as well as it does, but
it does. Aside from a little prompting here and there I have done nothing, and 
likely deserve little credit except for the idea and the design. I have been
a software developer for my entire adult life, using mostly C++, C#, and Java.
I have never touched Rust before this in my life.

I cannot guarantee anything about this repo. Do not trust it. It may hack your 
toaster, then fire warmed bread at your cat. That's on you.

In the future, the following things may happen, depending on my token budget:

- Separation of the workspaces and the media player into separate, but 
  compatible, components.
- Make the components into Applets.
- Maybe, maybe turn this into yet another configurable bar thing like polybar 
  or eww or similar. There are a lot of those already, though.

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
