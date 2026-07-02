//! Build a synthetic "media card" for demoing the tool without real
//! hardware: `cargo run --example mkdemo -- demo.bin`

use file_rescue::testutil::*;

fn main() -> std::io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo.bin".to_string());
    let mp4 = minimal_mp4(6_000_000, false, false);
    let mov = minimal_mov(1_500_000);
    let heic = minimal_heic(300_000);
    let jpeg = minimal_jpeg(400_000);
    let jpeg2 = minimal_jpeg(150_000);
    let png = minimal_png(80_000);
    let pdf = minimal_pdf(60_000);
    let plants: Vec<(u64, &[u8])> = vec![
        (0x0010_0000, &mp4[..]),
        (0x00A0_0000, &mov[..]),
        (0x00C0_0000, &heic[..]),
        (0x00D0_0000, &jpeg[..]),
        (0x00E0_0000, &jpeg2[..]),
        (0x00F0_0000, &png[..]),
        (0x00F8_0000, &pdf[..]),
    ];
    let disk = build_disk(16 * 1024 * 1024, &plants);
    std::fs::write(&path, &disk)?;
    println!(
        "wrote {path} ({} bytes) with {} planted files",
        disk.len(),
        plants.len()
    );
    println!("try:");
    println!("  cargo run --features fault-injection -- image {path} out.img out.map --simulate-bad 0x140000+0x2000");
    println!("  cargo run -- map show out.map");
    println!("  cargo run -- carve out.img recovered/ --map out.map");
    Ok(())
}
