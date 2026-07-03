# Flatpak packaging (manifest only - not built yet)

`org.nexuscontext.Manager.json` targets `org.gnome.Platform`/`Sdk` 47 (confirmed available on Flathub) plus the `rust-stable` SDK extension, since Flatpak builds run network-isolated and need Rust vendored in rather than fetched at build time.

**Before this manifest will actually build**, generate the vendored Cargo sources it references:

```bash
# from https://github.com/flatpak/flatpak-builder-tools
python3 flatpak-cargo-generator.py ../../Cargo.lock -o generated-sources.json
```

Then build with:

```bash
flatpak-builder --user --install build-dir org.nexuscontext.Manager.json
```

This wasn't run as part of this change - the GNOME Platform + SDK runtimes are a ~1.5-2GB download, and the manifest can be reviewed/corrected without pulling that in. `finish-args` deliberately scopes filesystem access to the app's own subdirectories (`xdg-run/nexuscontext`, `xdg-data/nexuscontext`, `xdg-config/nexuscontext`) rather than broad home access, since the GUI only ever needs the control socket, the log file, and its own config.

**Known gap:** `org.nexuscontext.Manager.desktop` references an `org.nexuscontext.Manager` icon that doesn't exist yet, and the manifest doesn't install one. Needs an actual icon (SVG, installed to `/app/share/icons/hicolor/scalable/apps/`) plus ideally an AppStream `metainfo.xml` before this would pass Flathub submission requirements.
