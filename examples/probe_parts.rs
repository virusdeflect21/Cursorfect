//! Dev tool: print peak/RMS of each part the sound is cut into, and of the worst-case mix
//! (start + movement loop at full level + dash), to check headroom.
//! `cargo run --example probe_parts -- path/to/sandevistan.mp3`
use rodio::Source;

fn stats(name: &str, s: &[f32]) {
    let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt();
    println!(
        "{name:<12} peak {peak:.3}  rms {rms:.3}  ({} samples)",
        s.len()
    );
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_parts <file>");
    let dec = rodio::Decoder::try_from(std::fs::File::open(&path).expect("open")).expect("decode");
    let ch = dec.channels().get() as usize;
    let rate = dec.sample_rate().get() as f32;
    let s: Vec<f32> = dec.collect();
    let at = |t: f32| ((t * rate) as usize * ch).min(s.len());
    let part = |a: f32, b: f32| s[at(a)..at(b)].to_vec();
    stats("whole", &s);
    let start = part(0.08, 3.27);
    let mv = part(5.45, 9.35);
    let dash = part(3.27, 4.25);
    let end = part(13.72, 15.84);
    stats("start", &start);
    stats("move(raw)", &mv);
    stats("dash", &dash);
    stats("end", &end);
    let mv_rms = (mv.iter().map(|x| x * x).sum::<f32>() / mv.len() as f32).sqrt();
    let mv_gain = 0.20 / mv_rms;
    // Worst case at volume 100: start + move (full level, 0.7 move volume) + dash * 1.1.
    let n = start.len().min(dash.len()).min(mv.len());
    let mix: Vec<f32> = (0..n)
        .map(|i| start[i] + mv[i] * mv_gain * 0.7 + dash[i] * 1.1)
        .collect();
    stats("worst mix", &mix);
}
