# sidechain

> [!CAUTION]
>
> This tool was thrown together in a few hours, is still WIP, and has not yet been thoroughly tested.

Tool that makes a lossy mirror of your lossless music collection.

# TODOs

- [ ] Track renamed files using hash
- [ ] Nix derivation and NixOS module (for setting up systemd timer)

# dependencies

- sqlite3
- ffmpeg

# works well with

- https://syncthing.net/

# alternatives

- https://github.com/nschlia/ffmpegfs - if you need on-demand transcoding. In my case, I only need syncing to happen periodically, and I need lossy files to be passed through instead of re-transcoded, so I made this
