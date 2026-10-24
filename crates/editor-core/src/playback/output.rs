use super::PlaybackOutput;
use anyhow::Result;
use std::time::{Duration, Instant};

pub(super) enum Output {
    Silent(Silent),
    #[cfg(windows)]
    Device(native::Device),
}
impl Output {
    pub fn open(mode: PlaybackOutput) -> Result<Self> {
        match mode {
            PlaybackOutput::Silent => Ok(Self::Silent(Silent {
                queued: 0,
                written: 0,
                anchor: None,
            })),
            PlaybackOutput::Device => {
                #[cfg(windows)]
                {
                    Ok(Self::Device(native::Device::open()?))
                }
                #[cfg(not(windows))]
                {
                    anyhow::bail!("Native editor audio output is not implemented on this platform")
                }
            }
        }
    }
    pub fn capacity(&self) -> u32 {
        match self {
            Self::Silent(_) => 960,
            #[cfg(windows)]
            Self::Device(d) => d.capacity,
        }
    }
    pub fn padding(&mut self) -> Result<u32> {
        match self {
            Self::Silent(s) => {
                s.update();
                Ok(s.queued)
            }
            #[cfg(windows)]
            Self::Device(d) => Ok(unsafe { d.client.GetCurrentPadding()? }),
        }
    }
    pub fn position(&mut self) -> Result<u64> {
        match self {
            Self::Silent(s) => {
                s.update();
                Ok(s.written - u64::from(s.queued))
            }
            #[cfg(windows)]
            Self::Device(d) => {
                let mut position = 0;
                unsafe {
                    d.clock.GetPosition(&mut position, None)?;
                }
                Ok((u128::from(position) * 48_000 / u128::from(d.frequency)) as u64)
            }
        }
    }
    pub fn write(&mut self, frames: &[[f32; 2]]) -> Result<()> {
        match self {
            Self::Silent(s) => {
                s.queued += frames.len() as u32;
                s.written += frames.len() as u64;
                Ok(())
            }
            #[cfg(windows)]
            Self::Device(d) => {
                unsafe {
                    let target = d.render.GetBuffer(frames.len() as u32)?;
                    std::ptr::copy_nonoverlapping(
                        frames.as_ptr().cast::<u8>(),
                        target,
                        frames.len() * 8,
                    );
                    d.render.ReleaseBuffer(frames.len() as u32, 0)?;
                }
                Ok(())
            }
        }
    }
    pub fn play(&mut self) -> Result<()> {
        match self {
            Self::Silent(s) => {
                s.anchor = Some(Instant::now());
                Ok(())
            }
            #[cfg(windows)]
            Self::Device(d) => {
                unsafe {
                    d.client.Start()?;
                }
                Ok(())
            }
        }
    }
    pub fn pause(&mut self) -> Result<()> {
        match self {
            Self::Silent(s) => {
                s.update();
                s.anchor = None;
                Ok(())
            }
            #[cfg(windows)]
            Self::Device(d) => {
                unsafe {
                    d.client.Stop()?;
                }
                Ok(())
            }
        }
    }
    pub fn wait(&self) {
        match self {
            Self::Silent(_) => std::thread::sleep(Duration::from_millis(2)),
            #[cfg(windows)]
            Self::Device(d) => unsafe {
                let _ = windows::Win32::System::Threading::WaitForSingleObject(d.event, 5);
            },
        }
    }
}
pub(super) struct Silent {
    queued: u32,
    written: u64,
    anchor: Option<Instant>,
}
impl Silent {
    fn update(&mut self) {
        if let Some(anchor) = self.anchor {
            let consumed = (anchor.elapsed().as_secs_f64() * 48_000.) as u32;
            self.queued = self.queued.saturating_sub(consumed);
            self.anchor = Some(anchor + Duration::from_secs_f64(f64::from(consumed) / 48_000.));
        }
    }
}

#[cfg(windows)]
mod native {
    use anyhow::Result;
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Media::Audio::*,
        System::{Com::*, Threading::CreateEventW},
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    pub(in crate::playback) struct Device {
        pub client: IAudioClient,
        pub render: IAudioRenderClient,
        pub clock: IAudioClock,
        pub frequency: u64,
        pub capacity: u32,
        pub event: HANDLE,
        _apartment: Apartment,
    }
    impl Device {
        pub fn open() -> Result<Self> {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
                let apartment = Apartment;
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let endpoint = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let client: IAudioClient = endpoint.Activate(CLSCTX_ALL, None)?;
                let format = WAVEFORMATEX {
                    wFormatTag: 3,
                    nChannels: 2,
                    nSamplesPerSec: 48_000,
                    nAvgBytesPerSec: 48_000 * 8,
                    nBlockAlign: 8,
                    wBitsPerSample: 32,
                    cbSize: 0,
                };
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    200_000,
                    0,
                    &format,
                    None,
                )?;
                let capacity = client.GetBufferSize()?;
                let render = client.GetService()?;
                let clock: IAudioClock = client.GetService()?;
                let frequency = clock.GetFrequency()?;
                anyhow::ensure!(frequency > 0, "Audio device clock has no frequency");
                let event = CreateEventW(None, false, false, None)?;
                let result = Self {
                    client,
                    render,
                    clock,
                    frequency,
                    capacity,
                    event,
                    _apartment: apartment,
                };
                result.client.SetEventHandle(event)?;
                Ok(result)
            }
        }
    }
    impl Drop for Device {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
                let _ = CloseHandle(self.event);
            }
        }
    }
}
