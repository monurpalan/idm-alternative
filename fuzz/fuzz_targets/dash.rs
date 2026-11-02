#![no_main]
use libfuzzer_sys::fuzz_target;

const URL: &str = "https://example.com/a/manifest.mpd";

fuzz_target!(|text: &str| {
    if let Ok(streams) = shardload_media::parse(URL, text) {
        for v in &streams.video {
            let _ = shardload_media::dash_track(URL, text, &v.id);
            let _ = shardload_media::dash_live(URL, text, &v.id, 1.7e9);
        }
    }
    let _ = shardload_media::dash_track(URL, text, "1");
});
