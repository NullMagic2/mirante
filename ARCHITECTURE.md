<!--
Module: mirante.documentation.architecture
Purpose: Subsystem boundaries, data flow and implementation status.
Created: 2026-11-16
Architecture: STYLE_GUIDE.md owns coding rules; this file owns structure.
-->

# Mirante architecture

Mirante is a GTK 3 image viewer for Ubuntu, Debian and Linux Mint, written in
Rust 2024. It opens BMP, PNG, JPEG, TIFF, PSD, HEIC and camera RAW files, crops
and colour-corrects them non-destructively, and manages colour with ICC profiles
end to end. Apart from `gtk`, every line (codecs, compression, colour) is ours.

## Workspace

```
crates/
  mirante-core    Image, ColorSpace, Decoder/Encoder traits, PluginRegistry, parallel helper
  mirante-codec   shared primitives: ByteReader, inflate/deflate, CRC/Adler, PackBits,
                  TIFF IFDs, ISO-BMFF boxes, YCbCr/CMYK conversion
  mirante-color   ICC parser (matrix/TRC), curves, working/display/export transforms
  mirante-edit    Edits { crop, adjustments }, crop, area-average downscale
  mirante-raw     shared RAW parts: sensor planes, CFA patterns, demosaicing, camera
                  colour table, and `develop` (NativeRaw → linear Rec. 2020)
  mirante-plugin  plugin processes: MIRI pipe protocol, manifests, install/remove,
                  default-viewer claiming, and the mirante-plugin-admin helper
  mirante-app     the `mirante` binary: GTK window, settings, i18n, theme, rendering
plugins/
  bmp png jpeg tiff psd heic raw   one crate per format: a library (`register`) and
                                   a `mirante-plugin-<id>` executable
  raw/makers/<maker>               canon fuji nikon olympus panasonic pentax sony:
                                   one library per camera maker, each `decode → NativeRaw`
packaging/        .desktop template, icon, polkit policy, Debian control and scripts
assets/flags/     language flags for the title-bar menu
```

Dependencies point one way: `core` ← `codec` ← plugins; `core` ← `color`, `edit`,
`plugin`; everything ← `app`. The app links only the PNG library (for its flag
icons); every format reaches it through a plugin process. A plugin may use another plugin's public functions (RAW and
HEIC call `mirante_format_jpeg`; TIFF calls it for JPEG compression).

## Plugins

Each format is a separate executable, `mirante-plugin-<id>`, whose `main` is one
call to `mirante_plugin::serve(register)`. Rust has no stable ABI and the
workspace forbids `unsafe`, so loading `.so` files is not an option. Processes
also mean a decoder that crashes on a hostile file only fails that file.

- **Protocol.** `<exe> decode` reads the file on stdin and writes a MIRI image
  on stdout: a 20-byte header, the ICC profile, then RGBA samples as u16 for
  encoded colour or f32 for linear data. `<exe> encode` does the reverse. Errors
  go to stderr as `unsupported: …` or `corrupt: …` with a non-zero exit.
- **Manifest.** `<exe> manifest` prints `plugin.ini`: id, name, version,
  executable, extensions, MIME types, magic bytes (`offset:hex`) and whether it
  writes. The build generates it, so it always matches the code. The viewer
  matches magic itself and starts a process only to decode or save.
- **Choosing a decoder.** A decoder whose magic **and** extension both match
  wins, then magic alone, then extension alone. This is how a `.nef` (TIFF
  magic) reaches the RAW plugin while a `.tif` reaches TIFF.
- **Where plugins live,** searched in order (the first copy of an id wins):
  `~/.local/share/mirante/plugins` (for me), `/usr/local/lib/mirante/plugins`
  (installed for everyone), `/usr/lib/mirante/plugins` (shipped in the package),
  plus `MIRANTE_PLUGIN_PATH` for development. Each plugin is a `<id>/` folder.
- **Bundles.** A `.mirante-plugin` file is a ustar archive of that folder,
  made by `mirante-plugin-admin pack` (and readable by `tar`).
- **Settings → Plugins.** *Install…* asks "Just me" or "Everyone". *Remove* on
  a personal plugin deletes it; on a shared plugin it asks again: "Just me"
  turns it off in the user's settings, "Everyone" runs
  `pkexec /usr/lib/mirante/mirante-plugin-admin remove <id>`. That deletes a
  `/usr/local` plugin, or lists a packaged one in `/etc/mirante/disabled-plugins`,
  because dpkg owns those files. *Restore* undoes either. The viewer never runs
  as root; the polkit policy names only the helper.
- **File associations follow plugins.** After every system change the helper
  rewrites the .desktop entry (in `/usr/local/share/applications` when it
  differs from the package's) and claims the new MIME types as defaults. For
  personal plugins, the viewer writes `~/.local/share/applications` and
  `~/.config/mimeapps.list`, without overriding a choice the user already made.

Decoders return pixels in the file's own colour space, tagged with `ColorSpace`
(`Srgb`, `LinearSrgb`, `LinearRec2020` or the embedded `Icc` bytes). They apply
EXIF/TIFF orientation with `Image::oriented`. Encoders receive sRGB in 0..1.

| Format | Read | Write | Notes |
|---|---|---|---|
| BMP | core, v3–v5 headers, 1–32 bit, RLE4/8, bitfields, v5 ICC | — | |
| PNG | all colour types/depths, tRNS, Adam7, iCCP | 16-bit, sRGB chunk | own inflate/deflate |
| JPEG | baseline, progressive, any subsampling, CMYK/YCCK, ICC, EXIF orientation | baseline 4:2:0, q92 | lossless SOF3 for DNG |
| TIFF | strips/tiles, LZW, Deflate, PackBits, JPEG, predictors, float | 16-bit Deflate | BigTIFF unsupported |
| PSD | merged composite, 8/16/32-bit, RGB/grey/CMYK/indexed | — | layers not composited |
| HEIC | HEIF container (grid, alpha, irot/imir/clap, ICC/nclx) with our HEVC intra decoder; JPEG items | — | Main/Main 10/Main Still Picture, 4:0:0–4:4:4, up to 12-bit |
| RAW | DNG: full pipeline; others: largest embedded JPEG preview | — | see below |

### RAW

The RAW plugin is a thin dispatcher (`plugins/raw/src/decode.rs`). DNG files
go to `dng.rs`. Every other format goes to its maker crate, chosen by the TIFF
Make tag, or by container for RAF and CR3. A maker crate only decodes the
file into a `NativeRaw` (`crates/mirante-raw/src/frame.rs`): photosites, CFA
pattern, visible area, black and white levels, as-shot white balance, camera
name and orientation. It never demosaics or converts colour.

`mirante_raw::develop` does the rest for every maker. It normalises, demosaics
(Malvar–He–Cutler for Bayer, a dedicated X-Trans path for Fujifilm), applies
white balance, converts with the camera's matrix to linear Rec. 2020, and clips
highlights to neutral. The camera matrix comes from a table generated from
dcraw's `adobe_coeff` (`tools/camera_table.py`). Cameras newer than the table
get a matrix fitted against their own embedded preview. DNG uses the same
steps with the matrices stored in the file.

If a maker decoder fails or meets a variant it doesn't handle, the viewer
shows the embedded full-size JPEG preview instead, so a file always opens.

Shared pieces live in `mirante-raw`: `bits` (MSB-first bit reader, Huffman
tables, the two-column predictor), `tiff_raw` (raw IFD, strips, CFA pattern,
maker note, names), `unpack` (packed samples), `levels`, `demosaic`, `color`,
`camera` + `camera_table` and `fit`. Maker crates only hold what is specific to
their format.

Every sample below decodes **bit-exactly** like LibRaw 0.21; white balance,
levels and CFA phase match too. Samples are CC0 files from raw.pixls.us, used
for verification only and not in the repository.

| Maker | Formats | Verified on |
|---|---|---|
| Canon | CR2 (Bayer and sRAW/mRAW, lossless JPEG slices), CR3 (CRX lossless "RAW" and lossy "CRAW") | 5D Mark III Bayer and sRAW, 80D sRAW, R6 RAW and CRAW |
| Nikon | NEF (uncompressed, lossy and lossless Huffman, 12/14-bit) | D750 ×3, Z 6 |
| Pentax | PEF (Pentax Huffman, uncompressed) | K-1 |
| Sony | ARW (ARW2 compressed, uncompressed; SR2 decryption) | A7 III ×2 |
| Olympus | ORF (Olympus compression, 12-bit packed) | E-M10 Mark III |
| Panasonic | RW2 (RawFormat 4) | G9 |
| Fujifilm | RAF (lossless compressed X-Trans/Bayer, uncompressed) | X-T3, X100F ×2 |

Colour: 571 cameras come from the dcraw table and land within 0.3–2.3 chroma
units of LibRaw. Newer cameras (R6, X-T3, Z 6 among the samples) get a matrix
fitted to their smallest adequate preview, and match those previews more
closely than LibRaw does. Decode plus develop takes 0.12–0.45 s per file, and
about 1 s for the 26 MP X-Trans X-T3 (release build).

Known gaps, each falling back to the preview:
- CRAW with several tiles (R5-class sensors).
- Lossless-JPEG ARW and ARW1.
- RW2 RawFormat 5 and later.
- Lossy compressed RAF and SuperCCD.
- Older Nikons whose white balance is only in the encrypted block (these
  develop, but without the camera's white balance).

X-Trans demosaicing still shows faint purple specks on fine bright detail.

### HEIC

`plugins/heic/src/hevc/` is our HEVC intra decoder. It covers NAL and
parameter-set parsing, CABAC, the coding tree, intra prediction, the inverse
transforms with scaling lists, deblocking and SAO, including tiles, wavefront,
slices, dependent slices, PCM and lossless coding. HEIC stills are intra-only,
so P/B slices are rejected.

Output is bit-exact with ffmpeg and libde265 on:
- 75 of 77 x265 streams (the other two match libde265);
- about 500 hand-made streams covering tiles, slices and PCM;
- all 1102 HEVC items of 20 iPhone photos.

Grids of HEVC tiles (iPhones use 512x512) are decoded in parallel and stitched
as YCbCr planes before a single conversion to RGB, so chroma upsampling crosses
tile seams. A whole photo then matches an independent reference within
0.002/255. A 12 MP photo takes 0.2–0.4 s.

Range-extension tools that no local encoder produces are implemented but not
reference-checked.

## Colour pipeline

```
file pixels ──(source profile)──▶ linear Rec. 2020 working image ──(edits)──▶
    ├─▶ monitor profile ─▶ premultiplied ARGB32 ─▶ Cairo      (preview)
    └─▶ sRGB 0..1 ─▶ encoder                                  (Export)
```

`mirante-color` parses matrix/TRC ICC profiles (v2 and v4: XYZ colorants,
`curv` and `para` curves, grey `kTRC`). That covers sRGB, Adobe RGB, Display P3,
ProPhoto, camera profiles and most monitor profiles. LUT-only and CMYK profiles
are reported as unsupported and treated as sRGB (CMYK decoders convert naively).
Each conversion is curve → one 3x3 matrix through D50 XYZ → inverse curve, with
curves flattened to 4096-entry tables and work split across cores.

Linear Rec. 2020 keeps wide-gamut colours (RAW, P3 photos) intact while editing;
clipping only happens on the way to the monitor or the file.

The display profile is chosen in Settings (`display_profile::Choice`):
- **Current monitor** (the default): colord over D-Bus for the monitor the
  window is on, or sRGB if it has none. It is looked up again when the window
  moves to another monitor.
- **A preset** from `mirante_color::Preset`: sRGB, Display P3, Adobe RGB (1998)
  or Rec. 2020 (BT.1886 gamma).
- **A custom ICC file** loaded with *Load…*. It then appears in the list by its
  file name; if it becomes unreadable, the monitor's profile is used.

## Editing

`Edits` is a small `Copy` value with three parts:
- an `Orientation`: 0–3 clockwise quarter turns after an optional mirror,
  which covers every rotation and flip;
- an optional `CropRect`, in displayed pixels, so a crop follows the picture
  when it is rotated or flipped (`Edits::reorient` moves it);
- `Adjustments`: exposure in EV; contrast, saturation, temperature and tint in
  -1..1.

The source image is never modified. `Edits::apply` works in this order:
1. maps the crop back to source pixels and crops;
2. optionally downscales (area averaging in linear light);
3. turns and mirrors the image with `Image::oriented`;
4. adjusts.

Turning after cropping and shrinking keeps previews cheap. The same function
serves the preview and the full-resolution export.

Editing is non-destructive for every format; no image file is ever written over.
- `edit_store.rs` keeps each image's edits as `key=value` lines
  (`Edits::to_text`) in `$XDG_DATA_HOME/mirante/edits/<key>.edits`.
- The key is a 64-bit FNV-1a hash of the file's size and its first and last
  64 KiB. It follows the content, so edits survive renaming or moving a photo.
- Edits are saved 400 ms after the last change, when switching images and on
  closing, and they come back when the file is opened again.
- *Revert to Original* removes them.
- *Export…* writes a new file with the edits applied. Its default name is
  `<name>-edited.<ext>`, and exporting over the original is refused.

## Application

- `window.rs` owns the `Document` (path, shared working image, edits, store key)
  and the header bar: Open, Previous/Next … Rotate left/right and Flip
  horizontally/vertically (one linked group), Crop, Adjust, Language, Menu.
  Ctrl+R rotates right and Ctrl+Shift+R rotates left. Rotating and flipping act
  on the picture as displayed and are paused while cropping.
- Cropping:
  - The selection starts as the whole image (or the current crop), with
    handles on its corners and edges.
  - Dragging a handle resizes it, dragging inside moves it, and dragging
    outside draws a new one. The cursor shows which.
  - A bar under the image offers Cancel and Apply (also Escape and Enter).
    Applying the whole image removes the crop.
  - `crop_tool.rs` holds this geometry as plain, tested maths; `viewer.rs`
    draws the frame, the rule-of-thirds overlay and the handles.
- Zoom:
  - The mouse wheel zooms around the pointer; zooming out past the window
    returns to fit.
  - Ctrl+plus / Ctrl+minus zoom around the centre; Ctrl+0 fits; Ctrl+1 is 100%.
    The plain keys work too.
  - A zoomed image pans by dragging, or with Shift+wheel.
- `render.rs` renders off the main loop. One render runs at a time and only the
  newest request waits, so slider drags never queue work. A render is skipped
  when image, edits, size and display profile are unchanged (lazy rendering).
- `adjust_panel.rs` is the colour popover; `document.rs` loads, exports and
  finds folder neighbours without GTK.
- `settings_dialog.rs` holds the display profile choice and the plugin
  manager. It is not modal: a modal parent makes GTK 3 file choosers opened
  from it ignore every click.
- `plugins.rs` keeps the live registry and reloads it after changes.
- `dialogs.rs` is the shared file-chooser, question and error code, built from
  GTK's own modal dialogs.
- `i18n.rs` is the title-bar language menu used in our previous projects (flag
  button, popover with native names, per-user choice, live re-translation).
  Languages: Português (Brasil), English, Ελληνικά, Español. `translations.rs`
  stores each string with all three translations, so a missing one fails the build.
- `theme.rs` applies Yaru (or Yaru-dark, following the desktop) unless the
  desktop already uses a Yaru variant, which keeps its accent colour.
- `settings.rs` stores `language`, `display-profile` and `disabled-plugins` in
  `$XDG_CONFIG_HOME/mirante/settings`.

## Distribution

Targets: Ubuntu 24.04+, Debian 13+, Linux Mint 22+ and LMDE 7, all of which ship
GTK 3.24, `libgtk-3-0t64` and Yaru. Build with `./build.sh` on the oldest target
(Ubuntu 24.04) using rustup's stable toolchain; it checks formatting, clippy and
tests, then writes `dist/mirante_<version>_<arch>.deb`. Users need no Rust.

The package depends on `yaru-theme-gtk`, `yaru-theme-icon` and `pkexec`. It
ships the seven plugins in `/usr/lib/mirante/plugins`, the bundles are also
written to `dist/plugins/`, and installing it makes Mirante the default image
viewer: `postinst` runs `mirante-plugin-admin refresh`. That writes every
plugin MIME type to `/etc/xdg/mimeapps.list` and to any desktop-specific
`/etc/xdg/*-mimeapps.list`, which outrank the distribution defaults in
`/usr/share` on GNOME, Cinnamon, MATE and Xfce. The previous values are saved in
`/var/lib/mirante/previous-defaults`. `prerm` runs `release-defaults`, which
restores them unless an administrator changed them since. A user's own choice
in `~/.config/mimeapps.list` is never overridden. Purging also removes
admin-installed plugins and `/etc/mirante`.

To run from the source tree after `./build.sh`:
`MIRANTE_PLUGIN_PATH=target/plugins target/release/mirante`.

## Status

| Area | State |
|---|---|
| Core, registry, colour engine, edits | Done, unit-tested |
| PNG, JPEG, TIFF, BMP, PSD codecs | Done, checked against PIL |
| DNG, RAW previews, HEIF container | Done, synthetic tests |
| Native maker RAW decoders | Done; bit-exact with LibRaw on 17 CC0 samples |
| HEVC intra decoder (HEIC) | Done; bit-exact against ffmpeg/libde265 |
| Window, crop, adjust, zoom, language, Yaru | Done; smoke-tested under Xvfb, needs hands-on use |
| Plugin processes, install/remove, Settings | Done; store and defaults unit-tested, UI smoke-tested |
| .deb with default viewer and polkit helper | Builds; not yet installed on a test machine |
