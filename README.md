# Mirante

Mirante is an image viewer for Linux, built with GTK 3 and Rust, made
for Ubuntu, Debian and Linux Mint with non-destructive editing.

# Features

- Cropping, rotating, flipping or color-correcting without ever touching
the original file.

- Raw file format support

- Exporting

 Mirante remembers your edits for next time, and **Export…**
writes a new file whenever you want one.

![Mirante showing a camera RAW photo of a river valley](docs/screenshot.png)

## What it opens

PNG, JPEG, TIFF, BMP, Photoshop PSD, HEIC/HEIF and camera RAW files from Canon,
Nikon, Sony, Fujifilm, Olympus, Panasonic and Pentax, plus DNG. Every decoder is
written from scratch in Rust, and each format is a plugin you can add or remove
in **Settings**.

Colours are managed from start to finish: embedded profiles are honoured, and the
picture is shown through your monitor's profile, a standard preset or an ICC
file of your own.


## Install

Download the package for your system from the
[latest release](https://github.com/NullMagic2/mirante/releases/latest):

| System | Package |
|---|---|
| Ubuntu 24.04 or newer | `mirante_<version>+ubuntu24.04_amd64.deb` |
| Ubuntu 22.04 | `mirante_<version>+ubuntu22.04_amd64.deb` |
| Linux Mint 22 | `mirante_<version>+mint22_amd64.deb` |
| Linux Mint 21 | `mirante_<version>+mint21_amd64.deb` |
| Debian 13 (LMDE 7) | `mirante_<version>+debian13_amd64.deb` |
| Debian 12 (LMDE 6) | `mirante_<version>+debian12_amd64.deb` |

Then install it from the folder you saved it in, for example:

```bash
sudo apt install ./mirante_*+ubuntu24.04_amd64.deb
```

Installing makes Mirante your default image viewer; removing it gives that role
back.

To build the packages yourself, install Rust 1.85 or newer from
[rustup](https://rustup.rs), then run:

```bash
sudo apt install libgtk-3-dev dpkg-dev
./build.sh
```

The packages appear in `dist/`.

## Learn more

- [ARCHITECTURE.md](ARCHITECTURE.md): how Mirante is put together.
- [STYLE_GUIDE.md](STYLE_GUIDE.md): how to contribute code.

## License

Mirante is free software under the [GNU GPL, version 3 or later](LICENSE).

It includes code ported from dcraw by Dave Coffin and, for Canon CR3, from
LibRaw. Camera colour matrices come from dcraw, and the flag icons come from
flag-icons (MIT, see `assets/flags/COPYING.flag-icons`).
