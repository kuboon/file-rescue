//! Smoke tests of the `rescue` binary on plain files.

use assert_cmd::Command;
use file_rescue::testutil::*;
use predicates::prelude::*;

fn rescue() -> Command {
    Command::cargo_bin("rescue").unwrap()
}

fn make_disk(dir: &std::path::Path) -> std::path::PathBuf {
    let jpeg = minimal_jpeg(30_000);
    let mp4 = minimal_mp4(200_000, false, false);
    let disk = build_disk(2 * 1024 * 1024, &[(0x1000, &jpeg[..]), (0x80000, &mp4[..])]);
    let p = dir.join("disk.bin");
    std::fs::write(&p, disk).unwrap();
    p
}

#[test]
fn carve_extracts_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let disk = make_disk(dir.path());
    let out = dir.path().join("recovered");
    rescue()
        .args([
            "carve",
            disk.to_str().unwrap(),
            out.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("extracted 2 files"));
    assert!(out.join("report.txt").exists());
    assert!(out.join("report.json").exists());
    let names: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(names.iter().any(|n| n.ends_with(".jpg")), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with(".mp4")), "{names:?}");
}

#[test]
fn map_show_reads_ddrescue_mapfile() {
    let dir = tempfile::tempdir().unwrap();
    let map_path = dir.path().join("disk.map");
    std::fs::write(
        &map_path,
        "# Mapfile. Created by GNU ddrescue version 1.27\n\
         # current_pos  current_status  current_pass\n\
         0x00120000     ?               1\n\
         0x00000000  0x00110000  +\n\
         0x00110000  0x00010000  -\n",
    )
    .unwrap();
    rescue()
        .args(["map", "show", map_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("rescued"))
        .stdout(predicate::str::contains("bad-sector"));
    rescue()
        .args([
            "map",
            "regions",
            map_path.to_str().unwrap(),
            "--status",
            "bad-sector",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0x0000110000"));
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    #[test]
    fn image_copies_file_and_writes_map() {
        let dir = tempfile::tempdir().unwrap();
        let disk = make_disk(dir.path());
        let img = dir.path().join("out.img");
        let map = dir.path().join("out.map");
        rescue()
            .args([
                "image",
                disk.to_str().unwrap(),
                img.to_str().unwrap(),
                map.to_str().unwrap(),
                "--quiet",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("finished"));
        assert_eq!(
            std::fs::read(&disk).unwrap(),
            std::fs::read(&img).unwrap(),
            "image must match source"
        );
        let text = std::fs::read_to_string(&map).unwrap();
        assert!(text.contains("file-rescue"));
    }

    #[test]
    fn image_refuses_source_as_output() {
        let dir = tempfile::tempdir().unwrap();
        let disk = make_disk(dir.path());
        let map = dir.path().join("out.map");
        rescue()
            .args([
                "image",
                disk.to_str().unwrap(),
                disk.to_str().unwrap(),
                map.to_str().unwrap(),
                "--quiet",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("same file"));
    }

    #[test]
    fn image_refuses_clobber_without_map() {
        let dir = tempfile::tempdir().unwrap();
        let disk = make_disk(dir.path());
        let img = dir.path().join("out.img");
        std::fs::write(&img, b"precious").unwrap();
        let map = dir.path().join("out.map");
        rescue()
            .args([
                "image",
                disk.to_str().unwrap(),
                img.to_str().unwrap(),
                map.to_str().unwrap(),
                "--quiet",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("--force"));
    }

    #[cfg(feature = "fault-injection")]
    #[test]
    fn simulated_faults_flow_through_to_carve() {
        let dir = tempfile::tempdir().unwrap();
        let disk = make_disk(dir.path());
        let img = dir.path().join("out.img");
        let map = dir.path().join("out.map");
        // Bad range inside the planted MP4 (at 0x80000).
        rescue()
            .args([
                "image",
                disk.to_str().unwrap(),
                img.to_str().unwrap(),
                map.to_str().unwrap(),
                "--simulate-bad",
                "0x88000+0x400",
                "--quiet",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("1024 bad bytes"));
        let out = dir.path().join("recovered");
        rescue()
            .args([
                "carve",
                img.to_str().unwrap(),
                out.to_str().unwrap(),
                "--map",
                map.to_str().unwrap(),
                "--quiet",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("1 flagged damaged"));
    }
}
