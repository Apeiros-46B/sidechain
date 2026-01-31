# sidechain

> [!CAUTION]
>
> This tool was thrown together in a few hours, is still WIP, and has not yet been thoroughly tested.

Tool that makes a lossy mirror of your lossless music collection.

# usage notes

- Symlinks in the source directory will not be followed.
- To force a full rebuild, delete the destination directory and database file.
- All files that are not matched by the `--allowed` and `--ignored` flags will be passed through (hardlinked or copied, depending on the --copy flag).
- Non-UTF8 file names or paths ARE NOT SUPPORTED and may result in strange output filenames.
- Unexpected behaviour may occur if your destination directory is on a case-insensitive file system and your source directory had case collisions (e.g. `Song.flac` and `song.wav`, which would both get converted to `song.opus`). THIS SCENARIO IS NOT SUPPORTED.
- If your filesystem doesn't support hardlinks (or if your destination directory is on a different fs from your source), use the `--copy` option to prevent the default hardlinking behaviour.

# dependencies

- sqlite3
- ffmpeg

# works well with

- https://syncthing.net/

# alternatives

- https://github.com/nschlia/ffmpegfs - if you need on-demand transcoding. In my case, I only need syncing to happen periodically, and I need lossy files to be passed through instead of re-transcoded, so I made this
