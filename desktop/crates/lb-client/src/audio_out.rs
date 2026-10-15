//! Plays phone audio into an output device. To make it a *microphone* for
//! other apps, pick a virtual cable: on Windows "CABLE Input (VB-Audio
//! Virtual Cable)" (apps then select "CABLE Output" as mic), on macOS
//! "BlackHole".
//!
//! Jitter/drift handling: a small FIFO with a target fill; on overflow the
//! oldest samples are dropped, on underrun we output silence and re-prime.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::session::{AudioChunk, AudioSink};

const TARGET_MS: usize = 60;
const MAX_MS: usize = 200;

/// Output device names, default device first.
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Some(d) = host.default_output_device() {
        if let Ok(desc) = d.description() {
            names.push(desc.name().to_string());
        }
    }
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(desc) = d.description() {
                let n = desc.name().to_string();
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        }
    }
    names
}

/// Finds an installed virtual audio cable (VB-Cable, BlackHole, ...).
pub fn find_virtual_cable(devices: &[String]) -> Option<String> {
    const PATTERNS: [&str; 5] = ["cable input", "vb-audio", "blackhole", "voicemeeter input", "virtual audio cable"];
    devices
        .iter()
        .find(|d| PATTERNS.iter().any(|p| d.to_lowercase().contains(p)))
        .cloned()
}

struct Fifo {
    samples: VecDeque<f32>,
    primed: bool,
    target: usize,
    max: usize,
    underruns: u64,
}

struct Resampler {
    ratio: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let n = input.len() as f64;
        while self.pos < n {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.prev } else { input[i - 1] };
            let b = input[i];
            out.push(a + (b - a) * frac);
            self.pos += self.ratio;
        }
        self.pos -= n;
        self.prev = input[input.len() - 1];
    }
}

pub struct AudioOutput {
    _stream: cpal::Stream,
    fifo: Arc<Mutex<Fifo>>,
    out_rate: u32,
    pub device_name: String,
}

impl AudioOutput {
    /// Opens `name` (exact device name) or the default output device.
    pub fn open(name: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = match name {
            Some(n) => host
                .output_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.description().map(|x| x.name() == n).unwrap_or(false))
                .ok_or_else(|| format!("устройство вывода «{n}» не найдено"))?,
            None => host.default_output_device().ok_or("нет устройства вывода звука")?,
        };
        let device_name = device.description().map(|d| d.name().to_string()).unwrap_or_default();
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let out_rate = config.sample_rate();
        let fifo = Arc::new(Mutex::new(Fifo {
            samples: VecDeque::new(),
            primed: false,
            target: out_rate as usize * TARGET_MS / 1000,
            max: out_rate as usize * MAX_MS / 1000,
            underruns: 0,
        }));
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config.into(), fifo.clone()),
            SampleFormat::I16 => build::<i16>(&device, config.into(), fifo.clone()),
            SampleFormat::U16 => build::<u16>(&device, config.into(), fifo.clone()),
            SampleFormat::I32 => build::<i32>(&device, config.into(), fifo.clone()),
            other => return Err(format!("формат {other} не поддерживается")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            _stream: stream,
            fifo,
            out_rate,
            device_name,
        })
    }

    /// A thread-safe sink to plug into `ClientConfig::audio_sink`.
    pub fn sink(&self) -> AudioSink {
        let fifo = self.fifo.clone();
        let out_rate = self.out_rate;
        let rs = Mutex::new(Resampler {
            ratio: 1.0,
            pos: 0.0,
            prev: 0.0,
        });
        Arc::new(move |chunk: AudioChunk| {
            let ch = chunk.channels.max(1) as usize;
            let mono: Vec<f32> = chunk
                .samples
                .chunks_exact(ch)
                .map(|f| f.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32)
                .collect();
            let mut out = Vec::with_capacity(mono.len() * out_rate as usize / chunk.rate.max(1) as usize + 2);
            {
                let mut r = rs.lock().unwrap();
                r.ratio = chunk.rate as f64 / out_rate as f64;
                r.process(&mono, &mut out);
            }
            let mut f = fifo.lock().unwrap();
            f.samples.extend(out);
            if f.samples.len() > f.max {
                let excess = f.samples.len() - f.target;
                f.samples.drain(..excess);
            }
        })
    }

    pub fn buffered_ms(&self) -> f32 {
        self.fifo.lock().unwrap().samples.len() as f32 * 1000.0 / self.out_rate as f32
    }

    pub fn underruns(&self) -> u64 {
        self.fifo.lock().unwrap().underruns
    }
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig, fifo: Arc<Mutex<Fifo>>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut f = fifo.lock().unwrap();
                if !f.primed && f.samples.len() >= f.target {
                    f.primed = true;
                }
                for frame in data.chunks_mut(channels) {
                    let s = if f.primed {
                        match f.samples.pop_front() {
                            Some(s) => s,
                            None => {
                                f.primed = false;
                                f.underruns += 1;
                                0.0
                            }
                        }
                    } else {
                        0.0
                    };
                    let v = T::from_sample(s);
                    for out in frame.iter_mut() {
                        *out = v;
                    }
                }
            },
            |e| log::warn!("audio output: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_preserves_duration() {
        let mut r = Resampler {
            ratio: 48_000.0 / 44_100.0,
            pos: 0.0,
            prev: 0.0,
        };
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&[0.5; 480], &mut out);
        }
        // 48 000 samples in → ~44 100 out.
        assert!((out.len() as i64 - 44_100).abs() <= 2, "{}", out.len());
    }

    #[test]
    fn detects_cables() {
        let devs = vec!["Speakers (Realtek)".to_string(), "CABLE Input (VB-Audio Virtual Cable)".to_string()];
        assert_eq!(find_virtual_cable(&devs).as_deref(), Some("CABLE Input (VB-Audio Virtual Cable)"));
        assert_eq!(find_virtual_cable(&devs[..1]), None);
    }
}
