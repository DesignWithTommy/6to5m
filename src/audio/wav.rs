//! RIFF/WAVE writers: 16-bit PCM (patched on close) and raw XMA2 wrapping.

use std::io::{self, Seek, SeekFrom, Write};

pub struct WavWriter<W: Write + Seek> {
    out: W,
    data: u64,
}

impl<W: Write + Seek> WavWriter<W> {
    pub fn new(mut out: W, channels: u16, rate: u32) -> io::Result<Self> {
        out.write_all(&pcm_header(channels, rate, 0))?;
        Ok(Self { out, data: 0 })
    }

    pub fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.data += bytes.len() as u64;
        self.out.write_all(&bytes)
    }

    pub fn finish(mut self, channels: u16, rate: u32) -> io::Result<u64> {
        let size = u32::try_from(self.data).map_err(|_| io::Error::other("WAV data exceeds 4 GiB"))?;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&pcm_header(channels, rate, size))?;
        self.out.flush()?;
        Ok(self.data)
    }
}

pub fn pcm_header(channels: u16, rate: u32, data: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend(b"RIFF");
    h.extend(data.saturating_add(36).to_le_bytes());
    h.extend(b"WAVEfmt ");
    h.extend(16u32.to_le_bytes());
    h.extend(1u16.to_le_bytes());
    h.extend(channels.to_le_bytes());
    h.extend(rate.to_le_bytes());
    h.extend((rate * u32::from(channels) * 2).to_le_bytes());
    h.extend((channels * 2).to_le_bytes());
    h.extend(16u16.to_le_bytes());
    h.extend(b"data");
    h.extend(data.to_le_bytes());
    h
}

/// RIFF header for a non-streamed XMA2 stream (from rpf9-explorer AwcRecovery.WriteXmaWave).
pub fn xma2_header(channels: u16, rate: u32, samples: u32, data_len: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    let mut put = |at: usize, bytes: &[u8]| h[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, b"RIFF");
    put(4, &(72 + data_len).to_le_bytes());
    put(8, b"WAVEfmt ");
    put(16, &52u32.to_le_bytes());
    put(20, &0x166u16.to_le_bytes());
    put(22, &channels.to_le_bytes());
    put(24, &rate.to_le_bytes());
    put(28, &(rate * u32::from(channels) / 2).to_le_bytes());
    put(32, &(channels * 2).to_le_bytes());
    put(34, &16u16.to_le_bytes());
    put(36, &34u16.to_le_bytes());
    put(38, &channels.div_ceil(2).to_le_bytes());
    put(40, &(if channels == 1 { 4u32 } else { 3 }).to_le_bytes());
    put(44, &(samples.wrapping_mul(u32::from(channels) * 2)).to_le_bytes());
    put(48, &0x8000u32.to_le_bytes());
    put(69, &[4]);
    put(70, &(data_len.div_ceil(0x8000) as u16).to_le_bytes());
    put(72, b"data");
    put(76, &data_len.to_le_bytes());
    h
}

/// Stateful linear resampler for interleaved 16-bit PCM (preview quality).
pub struct Resampler {
    channels: usize,
    step: f64,
    position: f64,
    samples: Vec<i16>,
}

impl Resampler {
    pub fn new(channels: usize, source: u32, target: u32) -> Self {
        Self { channels, step: f64::from(source) / f64::from(target), position: 0.0, samples: Vec::new() }
    }

    pub fn process(&mut self, pcm: &[i16]) -> Vec<i16> {
        let ch = self.channels;
        self.samples.extend_from_slice(pcm);
        let frames = self.samples.len() / ch;
        let mut out = Vec::new();
        while (self.position as usize) + 1 < frames {
            let index = self.position as usize;
            let frac = self.position - index as f64;
            for c in 0..ch {
                let a = f64::from(self.samples[index * ch + c]);
                let b = f64::from(self.samples[(index + 1) * ch + c]);
                out.push((a + (b - a) * frac).round().clamp(-32768.0, 32767.0) as i16);
            }
            self.position += self.step;
        }
        let keep = (self.position as usize).min(frames.saturating_sub(1));
        self.samples.drain(..keep * ch);
        self.position -= keep as f64;
        out
    }

    pub fn flush(&mut self) -> Vec<i16> {
        let frames = self.samples.len() / self.channels;
        let tail = if frames > 0 { self.samples[(frames - 1) * self.channels..frames * self.channels].to_vec() } else { Vec::new() };
        self.samples.clear();
        self.position = 0.0;
        tail
    }
}
