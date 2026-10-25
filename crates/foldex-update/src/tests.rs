use super::*;
use serde_json::{Value, json};

fn release(version: &str, data: &[u8]) -> Value {
    let name = format!("Foldex-{version}-windows-x64-setup.exe");
    json!({
        "tag_name": format!("v{version}"), "draft": false, "prerelease": false,
        "assets": [{"name": name, "state": "uploaded", "size": data.len(),
            "digest": format!("sha256:{:x}", Sha256::digest(data)),
            "browser_download_url": format!("{REPOSITORY}/releases/download/v{version}/{name}")}]
    })
}
fn select(releases: Vec<Value>, current: &str, preview: bool) -> Result<Option<Update>, String> {
    select_update(&serde_json::to_vec(&releases).unwrap(), current, preview)
}

#[test]
fn selects_by_semver_without_downgrades_or_reinstalling_the_same_version() {
    let versions = ["0.9.0", "0.10.0", "0.2.0"];
    let releases = versions
        .map(|version| release(version, b"installer"))
        .to_vec();
    assert_eq!(
        select(releases.clone(), "0.1.0", false)
            .unwrap()
            .unwrap()
            .version(),
        "0.10.0"
    );
    assert!(select(releases.clone(), "0.10.0", false).unwrap().is_none());
    assert!(select(releases, "1.0.0", false).unwrap().is_none());
}

#[test]
fn draft_and_preview_policy_matches_the_release_workflow() {
    let mut draft = release("9.0.0", b"i");
    draft["draft"] = json!(true);
    let mut numeric_preview = release("0.2.0", b"i");
    numeric_preview["prerelease"] = json!(true);
    let mut marked_preview = release("1.0.0", b"i");
    marked_preview["prerelease"] = json!(true);
    let releases = vec![
        draft,
        numeric_preview,
        marked_preview,
        release("2.0.0-rc.1", b"i"),
    ];
    assert_eq!(
        select(releases.clone(), "0.1.0", false)
            .unwrap()
            .unwrap()
            .version(),
        "0.2.0"
    );
    assert_eq!(
        select(releases, "0.1.0", true).unwrap().unwrap().version(),
        "2.0.0-rc.1"
    );
}

#[test]
fn rejects_untrusted_urls_malformed_digests_oversized_and_duplicate_assets() {
    let original = release("0.2.0", b"installer");
    for field in [
        "http://github.com/installer.exe",
        "https://evil.example/installer.exe",
        "https://github.com/pfaizar/other/releases/download/v0.2.0/installer.exe",
    ] {
        let mut invalid = original.clone();
        invalid["assets"][0]["browser_download_url"] = json!(field);
        assert!(select(vec![invalid], "0.1.0", false).is_err());
    }
    for digest in [
        Value::Null,
        json!("sha1:abcd"),
        json!("sha256:00"),
        json!(format!("sha256:{}", "x".repeat(64))),
    ] {
        let mut invalid = original.clone();
        invalid["assets"][0]["digest"] = digest;
        assert!(select(vec![invalid], "0.1.0", false).is_err());
    }
    for size in [0, MAX_INSTALLER + 1] {
        let mut invalid = original.clone();
        invalid["assets"][0]["size"] = json!(size);
        assert!(select(vec![invalid], "0.1.0", false).is_err());
    }
    let mut duplicate = original.clone();
    duplicate["assets"]
        .as_array_mut()
        .unwrap()
        .push(original["assets"][0].clone());
    assert!(select(vec![duplicate], "0.1.0", false).is_err());
}

#[test]
fn incomplete_newest_release_does_not_silently_offer_an_older_release() {
    let mut pending = release("0.3.0", b"i");
    pending["assets"] = json!([]);
    assert!(select(vec![pending, release("0.2.0", b"i")], "0.1.0", false).is_err());
}

#[test]
fn release_tags_cannot_escape_into_paths_or_commands() {
    for tag in ["main", "v0.2.0/../../x", "v0.2.0;calc", "v0.2.0+build"] {
        let mut invalid = release("0.2.0", b"i");
        invalid["tag_name"] = json!(tag);
        assert!(select(vec![invalid], "0.1.0", true).unwrap().is_none());
    }
}

#[test]
fn rejects_truncated_corrupted_extra_bytes_cancelled_and_failed_streams() {
    let data = b"verified installer";
    let update = select(vec![release("0.2.0", data)], "0.1.0", false)
        .unwrap()
        .unwrap();
    let cancelled = AtomicBool::new(false);
    let mut destination = Vec::new();
    verify_stream(
        data.as_slice(),
        &mut destination,
        &update,
        &cancelled,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(destination, data);
    for bytes in [
        b"verified installe".as_slice(),
        b"modified installer".as_slice(),
        b"verified installer!".as_slice(),
    ] {
        assert!(verify_stream(bytes, io::sink(), &update, &cancelled, &mut |_| {}).is_err());
    }
    cancelled.store(true, Ordering::Release);
    assert!(
        verify_stream(
            data.as_slice(),
            io::sink(),
            &update,
            &cancelled,
            &mut |_| {}
        )
        .is_err()
    );
    let failed = io::Error::other("connection lost");
    struct FailedReader(io::Error);
    impl Read for FailedReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other(self.0.to_string()))
        }
    }
    assert!(
        verify_stream(
            FailedReader(failed),
            io::sink(),
            &update,
            &AtomicBool::new(false),
            &mut |_| {}
        )
        .is_err()
    );
}

#[test]
fn redirects_cannot_leave_github_or_downgrade_tls() {
    for url in [
        "http://github.com/file",
        "https://github.com.evil.test/file",
        "https://github.com@evil.test/file",
        "https://github.com:444/file",
        "file:///tmp/setup.exe",
        "/relative",
        "https://api.github.com/anything",
    ] {
        assert!(!allowed_download_url(url), "{url}");
    }
    assert!(allowed_download_url(
        "https://release-assets.githubusercontent.com/file?token=value"
    ));
}

#[test]
fn downloaded_files_are_reverified_before_execution_and_removed_on_drop() {
    let data = b"installer";
    let update = select(vec![release("0.2.0", data)], "0.1.0", false)
        .unwrap()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("setup.exe"), data).unwrap();
    let downloaded = Downloaded { directory, update };
    let path = downloaded.path();
    downloaded.verify().unwrap();
    fs::write(&path, b"tampered!").unwrap();
    assert!(downloaded.verify().is_err());
    drop(downloaded);
    assert!(!path.exists());
}

#[test]
fn settings_survive_atomic_replacement_and_missing_fields() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    assert_eq!(load_settings(&path).unwrap(), Settings::default());
    let settings = Settings {
        download_automatically: true,
        ..Default::default()
    };
    save_settings(&path, &settings).unwrap();
    assert_eq!(load_settings(&path).unwrap(), settings);
    save_settings(&path, &Settings::default()).unwrap();
    assert_eq!(load_settings(&path).unwrap(), Settings::default());
    fs::write(&path, br#"{"check_automatically":false}"#).unwrap();
    assert!(!load_settings(&path).unwrap().check_automatically);
    fs::write(&path, vec![b' '; 16 * 1024 + 1]).unwrap();
    assert!(load_settings(&path).is_err());
}

#[test]
fn inno_mutex_hash_matches_its_documented_utf16_example() {
    assert_eq!(
        running_mutex(Path::new("Test")),
        format!(
            "Global\\Foldex.Running.{:x}",
            Sha256::digest(b"t\0e\0s\0t\0")
        )
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(b"T\0e\0s\0t\0")),
        "e6fa3ca87b1b641ab646d3b4933bba8d0970763f030b6578a60abdeae7366247"
    );
    assert_eq!(
        running_mutex(Path::new(r"C:\Apps\Foldex\")),
        running_mutex(Path::new(r"\\?\c:\apps\foldex"))
    );
}
