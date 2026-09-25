//! mpv options derived from settings (plan section 6).

use flox_core::settings::{AspectMode, Settings, SubtitleSize};

/// The options passed to `Mpv::new`. `slang` comes from the settings only; use
/// [`base_options_with_ui_language`] to fall back to the OS UI language.
pub fn base_options(settings: &Settings) -> Vec<(&'static str, String)> {
    base_options_with_ui_language(settings, None)
}

/// [`base_options`], with `slang` falling back to `ui_language` (an ISO 639-1
/// code such as `"en"`) when no subtitle language is set.
pub fn base_options_with_ui_language(
    settings: &Settings,
    ui_language: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut o: Vec<(&'static str, String)> = vec![
        ("vo", "libmpv".into()),
        ("hwdec", "auto".into()),
        // No passthrough: everything is decoded to PCM.
        ("audio-spdif", String::new()),
        ("audio-exclusive", "no".into()),
        ("keep-open", "yes".into()),
        ("idle", "yes".into()),
        ("osc", "no".into()),
        ("input-default-bindings", "no".into()),
        ("input-vo-keyboard", "no".into()),
        // Mirrors Android's buffer: min 30 / max 90 / rebuffer 5 seconds.
        ("cache", "yes".into()),
        ("demuxer-max-bytes", "48MiB".into()),
        ("demuxer-readahead-secs", "90".into()),
        ("cache-pause-wait", "5".into()),
    ];

    if let Some(a) = non_empty(settings.audio_language.as_deref()) {
        o.push(("alang", a.to_owned()));
    }
    if let Some(s) = non_empty(settings.subtitle_language.as_deref()).or(non_empty(ui_language)) {
        o.push(("slang", s.to_owned()));
    }
    o.push((
        "sid",
        if settings.subtitles_enabled {
            "auto"
        } else {
            "no"
        }
        .into(),
    ));
    o.push((
        "sub-scale",
        match settings.subtitle_size {
            SubtitleSize::Small => "0.8",
            SubtitleSize::Normal => "1.0",
            SubtitleSize::Large => "1.25",
        }
        .into(),
    ));
    o.push(("speed", settings.playback_speed.to_string()));
    match settings.aspect_mode {
        AspectMode::Fit => {
            o.push(("panscan", "0".into()));
            o.push(("keepaspect", "yes".into()));
        }
        AspectMode::Fill => o.push(("keepaspect", "no".into())),
        AspectMode::Zoom => o.push(("panscan", "1.0".into())),
    }
    o.push(("network-timeout", "20".into()));
    o.push(("tls-verify", "yes".into()));
    o
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(o: &'a [(&'static str, String)], k: &str) -> Option<&'a str> {
        o.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn defaults() {
        let o = base_options(&Settings::default());
        let expected: &[(&str, &str)] = &[
            ("vo", "libmpv"),
            ("hwdec", "auto"),
            ("audio-spdif", ""),
            ("audio-exclusive", "no"),
            ("keep-open", "yes"),
            ("idle", "yes"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("cache", "yes"),
            ("demuxer-max-bytes", "48MiB"),
            ("demuxer-readahead-secs", "90"),
            ("cache-pause-wait", "5"),
            ("sid", "no"),
            ("sub-scale", "1.0"),
            ("speed", "1"),
            ("panscan", "0"),
            ("keepaspect", "yes"),
            ("network-timeout", "20"),
            ("tls-verify", "yes"),
        ];
        let got: Vec<(&str, &str)> = o.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn languages_and_subtitles() {
        let s = Settings {
            audio_language: Some("ja".into()),
            subtitles_enabled: true,
            subtitle_language: Some("en".into()),
            subtitle_size: SubtitleSize::Large,
            ..Settings::default()
        };
        let o = base_options_with_ui_language(&s, Some("fr"));
        assert_eq!(get(&o, "alang"), Some("ja"));
        assert_eq!(get(&o, "slang"), Some("en"));
        assert_eq!(get(&o, "sid"), Some("auto"));
        assert_eq!(get(&o, "sub-scale"), Some("1.25"));

        let o = base_options_with_ui_language(&Settings::default(), Some("fr"));
        assert_eq!(get(&o, "slang"), Some("fr"));
        assert_eq!(get(&o, "alang"), None);

        let small = Settings {
            subtitle_size: SubtitleSize::Small,
            ..Settings::default()
        };
        assert_eq!(get(&base_options(&small), "sub-scale"), Some("0.8"));
    }

    #[test]
    fn speed_and_aspect() {
        let s = Settings {
            playback_speed: 1.25,
            aspect_mode: AspectMode::Fill,
            ..Settings::default()
        };
        let o = base_options(&s);
        assert_eq!(get(&o, "speed"), Some("1.25"));
        assert_eq!(get(&o, "keepaspect"), Some("no"));
        assert_eq!(get(&o, "panscan"), None);

        let z = Settings {
            aspect_mode: AspectMode::Zoom,
            playback_speed: 0.75,
            ..Settings::default()
        };
        let o = base_options(&z);
        assert_eq!(get(&o, "panscan"), Some("1.0"));
        assert_eq!(get(&o, "keepaspect"), None);
        assert_eq!(get(&o, "speed"), Some("0.75"));
    }
}
