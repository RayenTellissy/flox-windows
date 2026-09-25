//! Golden-fixture tests for settings, progress and tool resolution.

#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use flox_core::model::MediaType;
use flox_core::progress::{ProgressRecord, ProgressStore, PROGRESS_CAP};
use flox_core::settings::{
    AspectMode, LibrarySort, ResumeMode, Settings, SettingsStore, SubtitleSize,
};
use flox_core::tools::{resolve, Tool};
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn copy_fixture(name: &str, dir: &Path) -> PathBuf {
    let dest = dir.join("settings.json");
    fs::copy(fixture(name), &dest).unwrap();
    dest
}

// ---------------------------------------------------------------- settings

#[test]
fn android_settings_load() {
    let s = Settings::load(&fixture("settings_android.json")).unwrap();
    assert_eq!(s.audio_language.as_deref(), Some("ja"));
    assert!(s.subtitles_enabled);
    assert_eq!(s.subtitle_language.as_deref(), Some("en"));
    assert_eq!(s.subtitle_size, SubtitleSize::Large);
    assert!(!s.autoplay_next);
    assert_eq!(s.seek_step_seconds, 30);
    assert!((s.loudness_gain_db - 6.5).abs() < f32::EPSILON);
    assert_eq!(s.overlay_hide_ms, 6000);
    assert_eq!(s.resume_mode, ResumeMode::Ask);
    assert_eq!(s.finished_threshold_percent, 90);
    assert!((s.playback_speed - 1.25).abs() < f32::EPSILON);
    assert_eq!(s.aspect_mode, AspectMode::Zoom);
    assert_eq!(s.library_sort, LibrarySort::Size);
    assert_eq!(s.telegram_channel, "Movies Vault");
    assert_eq!(s.continue_watching_limit, 25);
    assert_eq!(s.quality.as_deref(), Some("2160p DV"));
    assert!((s.ui_scale - 1.0).abs() < f32::EPSILON);
}

#[test]
fn android_settings_round_trip_keeps_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture("settings_android.json", dir.path());
    let s = Settings::load(&path).unwrap();
    s.save(&path).unwrap();
    assert_eq!(
        read_json(&path),
        read_json(&fixture("settings_android.saved.json"))
    );
    assert_eq!(Settings::load(&path).unwrap(), s);
    // Saving again is stable.
    s.save(&path).unwrap();
    assert_eq!(
        read_json(&path),
        read_json(&fixture("settings_android.saved.json"))
    );
}

#[test]
fn messy_settings_are_clamped_and_emptied() {
    let s = Settings::load(&fixture("settings_messy.json")).unwrap();
    assert_eq!(s.audio_language, None);
    assert_eq!(s.subtitle_language, None);
    assert_eq!(s.subtitle_size, SubtitleSize::Normal);
    assert!(s.autoplay_next);
    assert_eq!(s.seek_step_seconds, 5);
    assert!((s.loudness_gain_db - 12.0).abs() < f32::EPSILON);
    assert_eq!(s.overlay_hide_ms, 2000);
    assert_eq!(s.resume_mode, ResumeMode::Never);
    assert_eq!(s.finished_threshold_percent, 98);
    assert!((s.playback_speed - 1.5).abs() < f32::EPSILON);
    assert_eq!(s.aspect_mode, AspectMode::Fit);
    assert_eq!(s.library_sort, LibrarySort::Title);
    assert_eq!(s.telegram_channel, "Flox Library");
    assert_eq!(s.continue_watching_limit, 100);
    assert_eq!(s.quality, None);
    assert!((s.ui_scale - 1.25).abs() < f32::EPSILON);
    assert_eq!(s.tmdb_api_key, None);
    assert_eq!(s.telegram_api_id, None);
    assert_eq!(s.telegram_api_hash, None);
    assert_eq!(s.ffmpeg_path, None);
    assert_eq!(s.libmpv_path, Some(PathBuf::from("C:\\mpv\\libmpv-2.dll")));
}

#[test]
fn empty_values_are_removed_from_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture("settings_messy.json", dir.path());
    Settings::load(&path).unwrap().save(&path).unwrap();
    let saved = read_json(&path);
    for key in [
        "audio_language",
        "subtitle_language",
        "quality",
        "tmdb_api_key",
        "telegram_api_id",
        "telegram_api_hash",
        "ffmpeg_path",
        "ytdlp_path",
        "tdjson_path",
    ] {
        assert!(saved.get(key).is_none(), "{key} should be removed");
    }
    assert_eq!(saved["seek_step_seconds"], 5);
    assert_eq!(saved["subtitle_size"], "NORMAL");
    assert_eq!(saved["telegram_channel"], "Flox Library");
}

#[test]
fn clamping_cases() {
    let snap = |f: fn(&mut Settings)| {
        let mut s = Settings::default();
        f(&mut s);
        s.clamped()
    };
    // Ties go to the first option, as Kotlin's minByOrNull.
    assert_eq!(snap(|s| s.seek_step_seconds = 45).seek_step_seconds, 30);
    assert_eq!(snap(|s| s.seek_step_seconds = 0).seek_step_seconds, 5);
    assert_eq!(snap(|s| s.seek_step_seconds = 999).seek_step_seconds, 60);
    assert_eq!(snap(|s| s.overlay_hide_ms = 8000).overlay_hide_ms, 6000);
    assert_eq!(snap(|s| s.overlay_hide_ms = 8001).overlay_hide_ms, 10000);
    assert_eq!(
        snap(|s| s.finished_threshold_percent = 50).finished_threshold_percent,
        85
    );
    assert_eq!(
        snap(|s| s.continue_watching_limit = 60).continue_watching_limit,
        50
    );
    assert!((snap(|s| s.playback_speed = 0.1).playback_speed - 0.75).abs() < f32::EPSILON);
    assert!((snap(|s| s.playback_speed = 1.75).playback_speed - 1.5).abs() < f32::EPSILON);
    assert!((snap(|s| s.playback_speed = f32::NAN).playback_speed - 1.0).abs() < f32::EPSILON);
    assert!((snap(|s| s.loudness_gain_db = -3.0).loudness_gain_db).abs() < f32::EPSILON);
    assert!((snap(|s| s.loudness_gain_db = 12.5).loudness_gain_db - 12.0).abs() < f32::EPSILON);
    assert!((snap(|s| s.loudness_gain_db = 3.3).loudness_gain_db - 3.3).abs() < f32::EPSILON);
    assert!(
        (snap(|s| s.loudness_gain_db = f32::INFINITY).loudness_gain_db - 8.0).abs() < f32::EPSILON
    );
    assert!((snap(|s| s.ui_scale = 0.5).ui_scale - 0.75).abs() < f32::EPSILON);
    assert!((snap(|s| s.ui_scale = 1.1).ui_scale - 1.0).abs() < f32::EPSILON);
    assert_eq!(
        snap(|s| s.telegram_channel = " ".into()).telegram_channel,
        "Flox Library"
    );
    assert_eq!(
        snap(|s| s.audio_language = Some(String::new())).audio_language,
        None
    );
    assert_eq!(
        snap(|s| s.ytdlp_path = Some(PathBuf::new())).ytdlp_path,
        None
    );
    assert_eq!(Settings::default().clamped(), Settings::default());
}

#[test]
fn missing_and_blank_files_give_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.json");
    assert_eq!(Settings::load(&missing).unwrap(), Settings::default());
    let blank = dir.path().join("blank.json");
    fs::write(&blank, "  \n").unwrap();
    assert_eq!(Settings::load(&blank).unwrap(), Settings::default());
    let broken = dir.path().join("broken.json");
    fs::write(&broken, "{not json").unwrap();
    assert!(Settings::load(&broken).is_err());
    let store = SettingsStore::open(broken).unwrap();
    assert_eq!(store.get(), Settings::default());
}

#[test]
fn store_update_saves_clamps_and_notifies() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture("settings_android.json", dir.path());
    let store = SettingsStore::open(path.clone()).unwrap();
    let mut rx = store.subscribe();
    store.update(|s| s.seek_step_seconds = 14).unwrap();
    assert!(rx.has_changed().unwrap());
    assert_eq!(rx.borrow_and_update().seek_step_seconds, 15);
    assert_eq!(store.get().seek_step_seconds, 15);
    let saved = read_json(&path);
    assert_eq!(saved["seek_step_seconds"], 15);
    assert_eq!(saved["legacy_flag"], "keep me");
    // A no-op update does not notify.
    store.update(|s| s.seek_step_seconds = 15).unwrap();
    assert!(!rx.has_changed().unwrap());
    // No temp files are left behind.
    let names: Vec<OsString> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![OsString::from("settings.json")]);
}

#[test]
fn store_update_failure_keeps_old_value() {
    let dir = tempfile::tempdir().unwrap();
    // The settings path is a directory, so the rename fails.
    let path = dir.path().join("settings.json");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("x"), b"").unwrap();
    let store = SettingsStore::new(path, Settings::default());
    assert!(store.update(|s| s.autoplay_next = false).is_err());
    assert!(store.get().autoplay_next);
}

// ---------------------------------------------------------------- progress

fn record(media: MediaType, id: u64, watched: u32, duration: u32, updated: i64) -> ProgressRecord {
    ProgressRecord {
        id,
        media,
        title: format!("title {id}"),
        poster: Some(format!("/p{id}.jpg")),
        watched,
        duration,
        season: 1,
        episode: 1,
        updated,
    }
}

#[test]
fn android_progress_load() {
    let store = ProgressStore::open(&fixture("progress_android.json")).unwrap();
    let all = store.all();
    let keys: Vec<(MediaType, u64)> = all.iter().map(|r| (r.media, r.id)).collect();
    assert_eq!(
        keys,
        vec![
            (MediaType::Tv, 1396),
            (MediaType::Movie, 27205),
            (MediaType::Movie, 603),
            (MediaType::Movie, 550),
        ]
    );
    let bb = store.get(MediaType::Tv, 1396).unwrap();
    assert_eq!(bb.title, "Breaking Bad");
    assert_eq!((bb.season, bb.episode, bb.watched), (3, 7, 1200));
    assert_eq!(bb.updated, 1_727_200_000_000);
    assert_eq!(store.get(MediaType::Movie, 27205).unwrap().poster, None);
    let matrix = store.get(MediaType::Movie, 603).unwrap();
    assert_eq!((matrix.season, matrix.episode), (1, 1));
    assert_eq!(matrix.poster, None);
    assert!(store.get(MediaType::Movie, 1396).is_none());
}

#[test]
fn continue_watching_filters_finished_and_limits() {
    let store = ProgressStore::open(&fixture("progress_android.json")).unwrap();
    let ids = |v: Vec<ProgressRecord>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
    // Inception is 8500/8880 = 95.7%: finished at 95, not at 98.
    assert_eq!(ids(store.continue_watching(95, 50)), vec![1396, 603, 550]);
    assert_eq!(
        ids(store.continue_watching(98, 50)),
        vec![1396, 27205, 603, 550]
    );
    assert_eq!(ids(store.continue_watching(95, 2)), vec![1396, 603]);
    assert!(store.continue_watching(95, 0).is_empty());
}

#[test]
fn put_upserts_moves_to_front_and_caps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.json");
    let store = ProgressStore::open(&path).unwrap();
    assert!(store.all().is_empty());
    for id in 0..(PROGRESS_CAP as u64 + 20) {
        store
            .put(record(MediaType::Movie, id, 10, 100, id as i64))
            .unwrap();
    }
    let all = store.all();
    assert_eq!(all.len(), PROGRESS_CAP);
    assert_eq!(all[0].id, PROGRESS_CAP as u64 + 19);
    assert_eq!(all[PROGRESS_CAP - 1].id, 20);
    // A TV title with the same id is a different record.
    store.put(record(MediaType::Tv, 50, 1, 100, 1)).unwrap();
    // Re-putting an existing movie moves it to the front without duplicating it.
    store.put(record(MediaType::Movie, 50, 60, 100, 2)).unwrap();
    let all = store.all();
    assert_eq!(all.len(), PROGRESS_CAP);
    assert_eq!(
        (all[0].media, all[0].id, all[0].watched),
        (MediaType::Movie, 50, 60)
    );
    assert_eq!((all[1].media, all[1].id), (MediaType::Tv, 50));
    assert_eq!(
        all.iter()
            .filter(|r| r.media == MediaType::Movie && r.id == 50)
            .count(),
        1
    );
    // The file matches memory and reloads identically.
    let reopened = ProgressStore::open(&path).unwrap();
    assert_eq!(reopened.all(), all);
    let file = read_json(&path);
    assert_eq!(file.as_array().unwrap().len(), PROGRESS_CAP);
    assert_eq!(file[0]["type"], "movie");
    assert_eq!(file[1]["type"], "tv");
    assert_eq!(file[0]["poster"], "/p50.jpg");
}

#[test]
fn saved_progress_uses_the_android_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.json");
    let store = ProgressStore::open(&path).unwrap();
    let mut r = record(MediaType::Tv, 1396, 1200, 2820, 1_727_200_000_000);
    r.poster = None;
    r.season = 3;
    r.episode = 7;
    store.put(r).unwrap();
    assert_eq!(
        read_json(&path),
        serde_json::json!([{
            "id": 1396, "type": "tv", "title": "title 1396", "watched": 1200,
            "duration": 2820, "season": 3, "episode": 7, "updated": 1_727_200_000_000_i64
        }])
    );
}

#[test]
fn clear_wipes_memory_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.json");
    fs::copy(fixture("progress_android.json"), &path).unwrap();
    let store = ProgressStore::open(&path).unwrap();
    assert!(!store.all().is_empty());
    store.clear().unwrap();
    assert!(store.all().is_empty());
    assert!(store.get(MediaType::Tv, 1396).is_none());
    assert!(ProgressStore::open(&path).unwrap().all().is_empty());
}

#[test]
fn corrupt_progress_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.json");
    fs::write(&path, "{\"items\": oops").unwrap();
    assert!(ProgressStore::open(&path).unwrap().all().is_empty());
}

// ---------------------------------------------------------------- tools

struct Layout {
    _root: tempfile::TempDir,
    app: PathBuf,
    bin1: PathBuf,
    bin2: PathBuf,
    custom: PathBuf,
}

fn layout() -> Layout {
    let root = tempfile::tempdir().unwrap();
    let dirs = ["app", "app/tools", "bin1", "bin2", "custom"];
    for d in dirs {
        fs::create_dir_all(root.path().join(d)).unwrap();
    }
    Layout {
        app: root.path().join("app"),
        bin1: root.path().join("bin1"),
        bin2: root.path().join("bin2"),
        custom: root.path().join("custom"),
        _root: root,
    }
}

fn touch(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, b"").unwrap();
    p
}

fn path_env(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).unwrap()
}

#[test]
fn resolver_order() {
    let l = layout();
    let ffmpeg = Tool::Ffmpeg.file_name();
    let env = path_env(&[&l.bin1, &l.bin2]);
    let env = Some(env.as_os_str());

    // Nothing anywhere.
    assert_eq!(resolve(Tool::Ffmpeg, &l.app, env, None), None);

    // PATH: first entry that has it wins.
    let in_bin2 = touch(&l.bin2, ffmpeg);
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.app, env, None),
        Some(in_bin2.clone())
    );
    let in_bin1 = touch(&l.bin1, ffmpeg);
    assert_eq!(resolve(Tool::Ffmpeg, &l.app, env, None), Some(in_bin1));

    // App dir beats PATH; <app>/tools counts, <app> itself beats <app>/tools.
    let in_tools = touch(&l.app.join("tools"), ffmpeg);
    assert_eq!(resolve(Tool::Ffmpeg, &l.app, env, None), Some(in_tools));
    let in_app = touch(&l.app, ffmpeg);
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.app, env, None),
        Some(in_app.clone())
    );

    // Override beats everything when it exists, and is ignored when it does not.
    let custom = touch(&l.custom, "my-ffmpeg-build");
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.app, env, Some(&custom)),
        Some(custom.clone())
    );
    let gone = l.custom.join("missing");
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.app, env, Some(&gone)),
        Some(in_app.clone())
    );

    // An override directory is searched for the tool's file name.
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.app, env, Some(&l.custom)),
        Some(in_app)
    );
    let custom_named = touch(&l.custom, ffmpeg);
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.app, env, Some(&l.custom)),
        Some(custom_named)
    );

    // No PATH at all, and the file only on PATH.
    assert_eq!(resolve(Tool::YtDlp, &l.app, None, None), None);
    assert_eq!(
        resolve(Tool::Ffmpeg, &l.bin1, None, None),
        Some(l.bin1.join(ffmpeg))
    );
    assert!(in_bin2.is_file());
}

#[test]
fn resolver_libraries() {
    let l = layout();
    let env = path_env(&[&l.bin1]);
    let td = touch(&l.bin1, Tool::TdJson.file_name());
    assert_eq!(
        resolve(Tool::TdJson, &l.app, Some(env.as_os_str()), None),
        Some(td)
    );
    let mpv = touch(&l.app, Tool::LibMpv.file_name());
    assert_eq!(
        resolve(Tool::LibMpv, &l.app, Some(env.as_os_str()), None),
        Some(mpv)
    );
}

#[test]
fn ffprobe_is_derived_from_ffmpeg() {
    let l = layout();
    let env = path_env(&[&l.bin1]);
    let env = Some(env.as_os_str());

    // ffprobe on PATH only, ffmpeg nowhere: found by its own name.
    let probe_path = touch(&l.bin1, Tool::Ffprobe.file_name());
    assert_eq!(resolve(Tool::Ffprobe, &l.app, env, None), Some(probe_path));

    // ffmpeg + ffprobe in the app tools dir: the one beside ffmpeg wins.
    touch(&l.app.join("tools"), Tool::Ffmpeg.file_name());
    let probe_tools = touch(&l.app.join("tools"), Tool::Ffprobe.file_name());
    assert_eq!(resolve(Tool::Ffprobe, &l.app, env, None), Some(probe_tools));

    // An ffmpeg override with a versioned stem gives the matching ffprobe.
    let custom = touch(&l.custom, "ffmpeg-7");
    let custom_probe = touch(&l.custom, "ffprobe-7");
    assert_eq!(
        resolve(Tool::Ffprobe, &l.app, env, Some(&custom)),
        Some(custom_probe)
    );
}
