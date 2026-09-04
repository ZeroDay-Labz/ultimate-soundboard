# Flatpak packaging

## Prerequisites (not installed in dev environments by default)

```sh
sudo dnf install flatpak-builder            # Fedora
flatpak install flathub org.freedesktop.Platform//24.08 org.freedesktop.Sdk//24.08
flatpak install flathub org.freedesktop.Sdk.Extension.rust-stable//24.08
```

CI builds and verifies this manifest on every push via
`.github/workflows/ci.yml`, using the prebuilt
`ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-24.08` container
-- so you don't need any of the above installed locally just to know the
manifest still builds; only for building/running it on your own machine.

## Build + install locally

From the repo root:

```sh
flatpak-builder --user --install --force-clean build-dir \
  packaging/flatpak/io.github.zerodaylabz.UltimateSoundboard.yml
```

Then run it like any other installed Flatpak app:

```sh
flatpak run io.github.zerodaylabz.UltimateSoundboard
```

## Regenerating `cargo-sources.json`

The manifest builds fully offline inside the sandbox (same requirement
Flathub enforces), so every crate dependency has to be pre-declared with a
download URL + checksum. `cargo-sources.json` in this directory was
generated with flatpak's own generator script and needs to be regenerated
any time `Cargo.lock` changes:

```sh
curl -sL -o /tmp/flatpak-cargo-generator.py \
  https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
uv run /tmp/flatpak-cargo-generator.py ../../Cargo.lock -o cargo-sources.json
```

(`uv run` picks up the script's declared Python dependencies --
`aiohttp`/`PyYAML`/`tomlkit` -- automatically; a plain venv + `pip install`
of those three works too.)

## Renaming the app ID

`io.github.zerodaylabz.UltimateSoundboard` is a placeholder. Before
publishing anywhere real (Flathub or otherwise), rename it consistently in:

- this manifest's filename and `app-id:` field
- `packaging/linux/ultimate-soundboard.desktop`'s installed name in the
  manifest's `install` command
- the icon's installed filename in the manifest's `install` command
