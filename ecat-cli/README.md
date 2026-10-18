# ecat-cli

Command-line interface for e-cat.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.

## Pet forms

`ecat pet` exposes the project mascot's expression forms:

```bash
ecat pet list                  # list slugs, names and shortcodes
ecat pet show smile            # print one SVG to stdout
ecat pet show :emoji-smile:    # shortcodes and localized names also work
ecat pet export ./pet          # write all 13 SVGs to a directory
```

The bundled SVGs are visual assets under
[`assets/pet/ASSETS-LICENSE.md`](assets/pet/ASSETS-LICENSE.md) and are **not**
Apache-2.0; commercial use requires a license.
