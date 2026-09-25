//! ffmpeg argument lists and `-stats` parsing, exactly as on the Mac.

use std::ffi::OsString;
use std::path::Path;

/// The Chrome 128 user agent the Mac passes to ffmpeg for HLS pulls (`Sniffer.userAgent`).
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// The Referer every HLS pull carries, replacing any the page sent.
pub const VIDLINK_REFERER: &str = "https://vidlink.pro/";

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

/// The `-headers` value: `"k: v\r\n"` per header, the page's own Referer replaced by VidLink's.
pub fn header_block(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("referer"))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(std::iter::once(("Referer", VIDLINK_REFERER)))
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect()
}

/// HLS remux: `-user_agent`, `-headers` (with the VidLink Referer), `-c copy -bsf:a aac_adtstoasc
/// -movflags +faststart`.
pub fn hls_args(input: &str, headers: &[(String, String)], out: &Path) -> Vec<OsString> {
    let mut args = os(&[
        "-y",
        "-loglevel",
        "warning",
        "-stats",
        "-user_agent",
        USER_AGENT,
    ]);
    args.push("-headers".into());
    args.push(header_block(headers).into());
    args.push("-i".into());
    args.push(input.into());
    args.extend(os(&[
        "-c",
        "copy",
        "-bsf:a",
        "aac_adtstoasc",
        "-movflags",
        "+faststart",
    ]));
    args.push(out.into());
    args
}

/// DASH mux of the downloaded tracks: `-c copy -tag:v hvc1 -movflags +faststart`.
/// Exactly the Mac's arguments, which assume HEVC video; see [`dash_args_for`].
pub fn dash_args(video: &Path, audio: Option<&Path>, out: &Path) -> Vec<OsString> {
    dash_args_for(video, audio, out, "hevc")
}

/// DASH mux for a video track of the given codec (an ffprobe `codec_name`). `-tag:v hvc1`
/// is added only for HEVC: ffmpeg's mp4 muxer rejects that tag on any other codec.
pub fn dash_args_for(
    video: &Path,
    audio: Option<&Path>,
    out: &Path,
    video_codec: &str,
) -> Vec<OsString> {
    let mut args = os(&["-y", "-loglevel", "error", "-i"]);
    args.push(video.into());
    if let Some(audio) = audio {
        args.push("-i".into());
        args.push(audio.into());
    }
    args.extend(os(&["-c", "copy"]));
    if video_codec.eq_ignore_ascii_case("hevc") {
        args.extend(os(&["-tag:v", "hvc1"]));
    }
    args.extend(os(&["-movflags", "+faststart"]));
    args.push(out.into());
    args
}

/// The value after `key=` in an ffmpeg stats line (ffmpeg pads values with spaces).
fn stat<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let mut from = 0;
    while let Some(i) = line[from..].find(&needle) {
        let at = from + i;
        let starts_word = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        let rest = line[at + needle.len()..].trim_start();
        if starts_word {
            let value = rest.split_whitespace().next()?;
            return (value != "N/A").then_some(value);
        }
        from = at + needle.len();
    }
    None
}

/// `"45KiB"` or `"1024kB"` as bytes.
fn size_bytes(value: &str) -> Option<f64> {
    let split = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let n: f64 = value[..split].parse().ok()?;
    let scale = match value[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "kib" => 1024.0,
        "mb" => 1e6,
        "mib" => 1024.0 * 1024.0,
        "gb" => 1e9,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(n * scale)
}

/// The job detail string for a `frame=… time=… speed=…` line:
/// `"00:12:34 · 245.1 MB · 12.3x"`, leaving out what ffmpeg reports as `N/A`.
/// Anything that is not a stats line (warnings, a negative start time) gives `None`.
pub fn parse_stats(line: &str) -> Option<String> {
    let line = line.trim();
    if stat(line, "frame").is_none() && stat(line, "size").is_none() {
        return None;
    }
    let time = stat(line, "time")?;
    if time.starts_with('-') || time.split(':').count() != 3 {
        return None;
    }
    let mut parts = vec![time.split('.').next().unwrap_or(time).to_string()];
    if let Some(bytes) = stat(line, "size")
        .or_else(|| stat(line, "Lsize"))
        .and_then(size_bytes)
    {
        parts.push(format!("{:.1} MB", bytes / 1e6));
    }
    if let Some(speed) = stat(line, "speed") {
        parts.push(speed.to_string());
    }
    Some(parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn hls_matches_the_mac() {
        let headers = vec![
            ("Origin".to_string(), "https://vidlink.pro".to_string()),
            ("referer".to_string(), "https://example.com/".to_string()),
        ];
        let args = hls_args(
            "https://cdn.example/master.m3u8",
            &headers,
            Path::new("/tmp/j/media.mp4"),
        );
        assert_eq!(
            strings(&args),
            vec![
                "-y",
                "-loglevel",
                "warning",
                "-stats",
                "-user_agent",
                USER_AGENT,
                "-headers",
                "Origin: https://vidlink.pro\r\nReferer: https://vidlink.pro/\r\n",
                "-i",
                "https://cdn.example/master.m3u8",
                "-c",
                "copy",
                "-bsf:a",
                "aac_adtstoasc",
                "-movflags",
                "+faststart",
                "/tmp/j/media.mp4",
            ]
        );
    }

    #[test]
    fn hls_without_headers_still_sends_the_referer() {
        assert_eq!(header_block(&[]), "Referer: https://vidlink.pro/\r\n");
    }

    #[test]
    fn dash_matches_the_mac() {
        let out = Path::new("/tmp/j/media.mp4");
        assert_eq!(
            strings(&dash_args(
                Path::new("/tmp/j/v.m4s"),
                Some(Path::new("/tmp/j/a.m4s")),
                out
            )),
            vec![
                "-y",
                "-loglevel",
                "error",
                "-i",
                "/tmp/j/v.m4s",
                "-i",
                "/tmp/j/a.m4s",
                "-c",
                "copy",
                "-tag:v",
                "hvc1",
                "-movflags",
                "+faststart",
                "/tmp/j/media.mp4",
            ]
        );
        assert_eq!(
            strings(&dash_args(Path::new("/tmp/j/v.m4s"), None, out)),
            vec![
                "-y",
                "-loglevel",
                "error",
                "-i",
                "/tmp/j/v.m4s",
                "-c",
                "copy",
                "-tag:v",
                "hvc1",
                "-movflags",
                "+faststart",
                "/tmp/j/media.mp4",
            ]
        );
    }

    #[test]
    fn dash_tags_hvc1_only_for_hevc() {
        let (v, out) = (Path::new("/tmp/j/v.m4s"), Path::new("/tmp/j/media.mp4"));
        for codec in ["hevc", "HEVC"] {
            assert_eq!(dash_args_for(v, None, out, codec), dash_args(v, None, out));
        }
        for codec in ["h264", "av1", ""] {
            assert_eq!(
                strings(&dash_args_for(v, None, out, codec)),
                vec![
                    "-y",
                    "-loglevel",
                    "error",
                    "-i",
                    "/tmp/j/v.m4s",
                    "-c",
                    "copy",
                    "-movflags",
                    "+faststart",
                    "/tmp/j/media.mp4",
                ],
                "{codec}"
            );
        }
    }

    #[test]
    fn stats_lines() {
        assert_eq!(
            parse_stats("frame= 1234 fps=250 q=-1.0 size=  239360KiB time=00:12:34.56 bitrate=2600.1kbits/s speed=12.3x"),
            Some("00:12:34 · 245.1 MB · 12.3x".to_string())
        );
        assert_eq!(
            parse_stats("frame=   75 fps=0.0 q=-1.0 Lsize=      45KiB time=00:00:03.00 bitrate= 122.0kbits/s speed= 365x elapsed=0:00:00.00"),
            Some("00:00:03 · 0.0 MB · 365x".to_string())
        );
        assert_eq!(
            parse_stats("size=    1024kB time=01:02:03.00 bitrate=N/A speed=N/A"),
            Some("01:02:03 · 1.0 MB".to_string())
        );
        assert_eq!(
            parse_stats("frame=    0 fps=0.0 q=0.0 size=       0KiB time=-577014:32:22.77 bitrate=  -0.0kbits/s speed=N/A"),
            None
        );
        assert_eq!(
            parse_stats("frame=    0 fps=0.0 q=0.0 size=N/A time=N/A bitrate=N/A speed=N/A"),
            None
        );
        assert_eq!(
            parse_stats("[hls @ 0x1] Opening 'seg-1.ts' for reading"),
            None
        );
        assert_eq!(parse_stats(""), None);
    }
}
