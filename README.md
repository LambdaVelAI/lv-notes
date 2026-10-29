# lv-notes
Academic note taking, knowledge management and authoring

## Installing (alpha)

lv-notes is in early alpha, for macOS (Apple Silicon and Intel).

1. Download the `.dmg` from the latest release on the
   [Releases page](https://github.com/LambdaVelAI/lv-notes/releases).
2. Open it and drag **lv-notes** into Applications.
3. Open lv-notes and create a project: one `.lv` file holds all of a
   project's notes, links and history.

Releases are signed and notarized by Apple, so lv-notes opens like any
other app. If macOS says it can't check the app, the build wasn't
notarized: please report it.

Alpha builds can have bugs, so keep your project files somewhere that's
backed up. **Export…** in the sidebar writes every note out at any time, as
a Jupyter Book or an Obsidian vault, and **Import…** brings either kind of
folder back in, links included. When a new version changes the file
format, it keeps a copy of the old file next to it before upgrading.

## Developing

See `CLAUDE.md` for the layout and commands, `docs/design/` for the design
notes, and `docs/release.md` for cutting a release.
