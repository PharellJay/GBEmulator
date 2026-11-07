//! Audio output via cpal. The emulator pushes samples into a shared queue;
//! the device callback pulls them. Volume and mute apply in the callback so
//! changes take effect immediately.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// Queue depth the drift correction aims for, in seconds.
const TARGET_LATENCY: f32 = 0.06;
/// Largest sample-rate nudge used to track the target, as a fraction.
const MAX_RATE_ADJUST: f32 = 0.005;

struct Shared {
    /// Interleaved stereo samples waiting for the device.
    queue: Mutex<VecDeque<f32>>,
    /// 0-100.
    volume: AtomicU32,
    muted: AtomicBool,
}

pub struct Audio {
    shared: Arc<Shared>,
    sample_rate: u32,
    // Dropping the stream stops playback, so it lives as long as `Audio`.
    _stream: cpal::Stream,
}

impl Audio {
    /// Open the default output device. Errors are returned as text so the
    /// caller can report them and carry on without sound.
    pub fn start(volume: u8, muted: bool) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no audio output device found".to_string())?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            volume: AtomicU32::new(volume.min(100) as u32),
            muted: AtomicBool::new(muted),
        });
        let sample_rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.config();

        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, shared.clone()),
            SampleFormat::I16 => build::<i16>(&device, config, shared.clone()),
            SampleFormat::U16 => build::<u16>(&device, config, shared.clone()),
            SampleFormat::I32 => build::<i32>(&device, config, shared.clone()),
            other => return Err(format!("unsupported audio sample format {other:?}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;

        Ok(Self {
            shared,
            sample_rate,
            _stream: stream,
        })
    }

    pub fn set_volume(&self, volume: u8) {
        self.shared
            .volume
            .store(volume.min(100) as u32, Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    /// Queue interleaved stereo samples and return the emulator sample rate
    /// to use next, nudged so the queue stays near the target latency
    /// despite the audio clock drifting from the frame timer.
    pub fn push(&self, samples: &[f32]) -> u32 {
        let target = TARGET_LATENCY * self.sample_rate as f32;
        let mut queue = self.shared.queue.lock().unwrap();
        queue.extend(samples);
        // After a stall, drop the backlog instead of playing it late.
        let max = (target * 4.0) as usize * 2;
        if queue.len() > max {
            let excess = queue.len() - (target as usize * 2);
            queue.drain(..excess);
        }
        let queued_frames = (queue.len() / 2) as f32;
        let error = ((target - queued_frames) / target).clamp(-1.0, 1.0);
        (self.sample_rate as f32 * (1.0 + error * MAX_RATE_ADJUST)) as u32
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                let gain = if shared.muted.load(Ordering::Relaxed) {
                    0.0
                } else {
                    shared.volume.load(Ordering::Relaxed) as f32 / 100.0
                };
                let mut queue = shared.queue.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    // Underruns play silence.
                    let left = queue.pop_front().unwrap_or(0.0) * gain;
                    let right = queue.pop_front().unwrap_or(0.0) * gain;
                    match frame {
                        [mono] => *mono = T::from_sample((left + right) * 0.5),
                        [l, r, rest @ ..] => {
                            *l = T::from_sample(left);
                            *r = T::from_sample(right);
                            for extra in rest {
                                *extra = T::from_sample(0.0);
                            }
                        }
                        [] => {}
                    }
                }
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}
