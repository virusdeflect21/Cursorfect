//! Dev tool: print the loudness envelope of a sound file (used to sync visuals to SFX and to
//! pick the cut points of the full sequence).
//! `cargo run --example probe_sound -- path/to/file.mp3 [window_ms] [raw_out.f32]`
//! With `raw_out.f32`, the mono mixdown is also written as little-endian f32 samples.
use rodio::Source;
use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: probe_sound <file> [window_ms] [raw_out]");
    let win_ms: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(250);
    let raw_out = args.next();
    let dec = rodio::Decoder::try_from(std::fs::File::open(&path).expect("open")).expect("decode");
    let (ch, rate) = (
        dec.channels().get() as usize,
        dec.sample_rate().get() as usize,
    );
    let samples: Vec<f32> = dec.collect();
    let frames = samples.len() / ch;
    println!(
        "{path}: {:.2}s, {ch} ch, {rate} Hz",
        frames as f32 / rate as f32
    );
    if let Some(out) = raw_out {
        let mut f = std::fs::File::create(out).expect("create raw");
        for fr in samples.chunks(ch) {
            let m = fr.iter().sum::<f32>() / ch as f32;
            f.write_all(&m.to_le_bytes()).expect("write");
        }
        println!("rate {rate}");
    }
    let win = (rate * win_ms / 1000).max(1);
    for (i, chunk) in samples.chunks(win * ch).enumerate() {
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len().max(1) as f32).sqrt();
        let bar = "#".repeat((rms * 120.0) as usize);
        println!(
            "{:6.2}s {:.3} {bar}",
            (i * win) as f32 / rate as f32,
            rms
        );
    }
}
