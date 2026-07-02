# file-rescue

Rescue data from failing storage (SD cards, USB sticks, disks) in two stages:

1. **`rescue image`** — ddrescue-style block imaging. Copies a dying device
   into an image file, getting the easy data first and narrowing down bad
   sectors later (copy → trim → scrape → retry). Progress is tracked in a
   **GNU ddrescue compatible mapfile**, so runs are interruptible/resumable
   and interoperate with `ddrescue` / `ddrescueview`. Linux only.
2. **`rescue carve`** — signature-based extraction of photos, videos and
   documents from the image. Works on any platform.

## Why another carver? Videos, mostly

Tools like photorec often split recovered videos into many small
fragments: they guess where a file ends by sniffing content, and stop as
soon as the data "stops looking like video". `rescue carve` instead
parses the ISO-BMFF box structure (`ftyp`/`moov`/`mdat`/... with 32- and
64-bit sizes) that MP4, MOV and HEIC share, and follows the **declared**
box sizes to the file's exact end — so a contiguous video comes out
whole, as one file, even when its data doesn't look like anything.

### Supported formats

| Formats | How the length is found |
|---|---|
| MP4 / MOV / HEIC / AVIF / 3GP (ISO-BMFF) | box-structure walk (exact) |
| MKV / WebM — the usual containers for VP9/**AV1** | EBML element sizes (exact) |
| AVI (incl. >2 GiB OpenDML) / WAV / WebP (RIFF) | declared RIFF size (exact) |
| WMV / WMA (ASF) | object sizes (exact) |
| PNG | chunk walk (exact; damaged files carved partially) |
| GIF | block-structure walk (exact) |
| BMP | declared file size (exact) |
| TIFF and TIFF-based RAW (CR2; typically NEF/ARW/DNG) | IFD walk → furthest referenced strip/tile data |
| JPEG | marker-structure validation + entropy-aware EOI scan |
| MPEG-PS (.mpg — DVD recorders) | pack/PES walk by start codes |
| MPEG-TS (.ts) / AVCHD (.m2ts) | 188/192-byte packet sync run |
| PDF | last `%%EOF` (incremental updates handled) |

AV1 is a codec, not a container — AV1 video is recovered via its
container (MP4/WebM/MKV), AV1 stills via AVIF. Adding a format is one
file in `src/carve/formats/` plus one registry line.

Note: files that are *actually fragmented on disk* (interleaved writes on
FAT/exFAT) cannot be reassembled by any contiguous carver; a
filesystem-aware recovery pass is a planned extension.

## Usage

```console
# 1. Image the failing device (never opened for writing; the tool refuses
#    to write the image onto the source device).
rescue image /dev/sdb card.img card.map --retry-passes 2

#    Interrupted? Just re-run the same command — the mapfile resumes it.

# 2. Inspect the damage
rescue map show card.map
rescue map regions card.map --status bad-sector

# 3. Extract media from the image; unrescued regions are skipped and
#    files overlapping bad sectors are flagged "damaged" in the report.
rescue carve card.img recovered/ --map card.map
cat recovered/report.txt
```

Useful flags: `image`: `--direct` (O_DIRECT), `--reverse`, `--cluster-size`,
`--sector-size`, `--force`; `carve`: `--formats mp4,jpeg`, `--align 1`
(scan every byte instead of sector-aligned offsets).

The image is written sparse: holes mark exactly the bytes that were never
rescued, like ddrescue.

## Demo without broken hardware

```console
cargo run --example mkdemo -- demo.bin
cargo build --features fault-injection
target/debug/rescue image demo.bin out.img out.map \
    --simulate-bad 0x140000+0x2000,0xD08000+0x400@2 --retry-passes 1
target/debug/rescue map show out.map
target/debug/rescue carve out.img recovered/ --map out.map
```

`OFF+LEN@N` regions heal after N failed reads, which demonstrates the
retry pass. `ddrescueview out.map` works too.

For genuine EIO from a real block device, use a device-mapper `error`
target over a loop device (requires root):

```console
truncate -s 64M plain.img && sudo losetup -f --show plain.img
# then dmsetup a linear+error table over the loop device
```

## Development

```console
cargo test --all-features   # unit + e2e (synthetic disks, fault injection)
cargo clippy --all-targets --all-features
```

Imaging engine and carvers are pure library code (`file_rescue` crate)
driven through `SectorSource`/`ReadAt` traits; tests inject faults
in-memory, no root or loop devices needed. `rescue image` is Linux-only;
`carve` and `map` build and test on Linux/macOS/Windows.

## License

MIT
