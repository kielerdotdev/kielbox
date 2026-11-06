//! Safe, thread-confined NVENC session around the pinned 12.1 ABI.
//! The NVIDIA DLL is loaded from System32; no CUDA toolkit is required.
use anyhow::{bail, ensure, Context, Result};
use libloading::Library;
use media_core::{Codec, EncodedPacket, EncoderCapabilities, VideoConfig, VideoEncoder};
use nvenc_sys::*;
use std::{
    collections::VecDeque,
    ffi::{c_void, CStr},
    marker::PhantomData,
    ptr,
    rc::Rc,
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Graphics::Direct3D11::*,
        System::Threading::{CreateEventW, WaitForSingleObject},
    },
};

const POOL: usize = 4;

fn completion_timeout_ms(deadline: Option<Instant>, now: Instant) -> u32 {
    deadline.map_or(5000, |deadline| {
        deadline.saturating_duration_since(now).as_millis().min(5000) as u32
    })
}

struct Slot {
    texture: ID3D11Texture2D,
    registered: NV_ENC_REGISTERED_PTR,
    mapped: NV_ENC_INPUT_PTR,
    bitstream: NV_ENC_OUTPUT_PTR,
    event: HANDLE,
    event_registered: bool,
}

/// Intentionally !Send + !Sync: a session and its immediate D3D11 context live
/// on one worker. The only shared resource with GPUI has a keyed mutex.
pub struct Nvenc {
    _library: Library,
    api: NV_ENCODE_API_FUNCTION_LIST,
    session: *mut c_void,
    slots: Vec<Slot>,
    pending: VecDeque<usize>,
    config: VideoConfig,
    flushing: bool,
    completion_failed: bool,
    drain_deadline: Option<Instant>,
    eos_event: HANDLE,
    eos_registered: bool,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl Nvenc {
    pub fn new(device: &ID3D11Device, config: &VideoConfig) -> Result<Self> {
        config.validate()?;
        unsafe {
            let system_root = std::env::var_os("SystemRoot").context("SystemRoot is missing")?;
            let path = std::path::PathBuf::from(system_root).join("System32/nvEncodeAPI64.dll");
            let library = Library::new(path).context("NVIDIA encoder driver is unavailable")?;
            let max_version: libloading::Symbol<unsafe extern "C" fn(*mut u32) -> NVENCSTATUS> =
                library.get(b"NvEncodeAPIGetMaxSupportedVersion\0")?;
            let mut version = 0;
            ensure!(
                max_version(&mut version) == NVENCSTATUS::NV_ENC_SUCCESS,
                "NVENC version query failed"
            );
            ensure!(
                version >= ((NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION),
                "NVIDIA driver is older than NVENC API 12.1"
            );
            let create: libloading::Symbol<
                unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS,
            > = library.get(b"NvEncodeAPICreateInstance\0")?;
            let mut api: NV_ENCODE_API_FUNCTION_LIST = std::mem::zeroed();
            api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
            ensure!(
                create(&mut api) == NVENCSTATUS::NV_ENC_SUCCESS,
                "NvEncodeAPICreateInstance failed"
            );
            let mut result = Self {
                _library: library,
                api,
                session: ptr::null_mut(),
                slots: Vec::new(),
                pending: VecDeque::new(),
                config: config.clone(),
                flushing: false,
                completion_failed: false,
                drain_deadline: None,
                eos_event: HANDLE::default(),
                eos_registered: false,
                _thread_affinity: PhantomData,
            };
            let mut open: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = std::mem::zeroed();
            open.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            open.apiVersion = NVENCAPI_VERSION;
            open.device = device.as_raw();
            open.deviceType = NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_DIRECTX;
            let opened = (api
                .nvEncOpenEncodeSessionEx
                .context("missing NVENC open function")?)(
                &mut open, &mut result.session
            );
            result.check(opened, "open session")?;
            let mut count = 0;
            result.check(
                api.nvEncGetEncodeGUIDCount.unwrap()(result.session, &mut count),
                "query codecs",
            )?;
            let mut codecs = vec![std::mem::zeroed::<GUID>(); count as usize];
            result.check(
                api.nvEncGetEncodeGUIDs.unwrap()(
                    result.session,
                    codecs.as_mut_ptr(),
                    count,
                    &mut count,
                ),
                "enumerate codecs",
            )?;
            ensure!(
                codecs.contains(&NV_ENC_CODEC_AV1_GUID),
                "this GPU does not support AV1 NVENC encoding"
            );
            let mut preset: NV_ENC_PRESET_CONFIG = std::mem::zeroed();
            preset.version = NV_ENC_PRESET_CONFIG_VER;
            preset.presetCfg.version = NV_ENC_CONFIG_VER;
            result.check(
                api.nvEncGetEncodePresetConfigEx
                    .context("missing preset function")?(
                    result.session,
                    NV_ENC_CODEC_AV1_GUID,
                    NV_ENC_PRESET_P3_GUID,
                    NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
                    &mut preset,
                ),
                "query AV1 preset",
            )?;
            let enc = &mut preset.presetCfg;
            enc.gopLength = config.keyframe_interval;
            enc.frameIntervalP = 1;
            enc.profileGUID = NV_ENC_AV1_PROFILE_MAIN_GUID;
            enc.rcParams.rateControlMode = NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_VBR;
            enc.rcParams.averageBitRate = config.bitrate;
            enc.rcParams.maxBitRate = config.bitrate * 3 / 2;
            enc.rcParams.set_enableLookahead(0);
            enc.rcParams.set_enableAQ(0);
            enc.rcParams.set_enableTemporalAQ(0);
            enc.rcParams.multiPass = NV_ENC_MULTI_PASS::NV_ENC_MULTI_PASS_DISABLED;
            let av1 = &mut enc.encodeCodecConfig.av1Config;
            av1.idrPeriod = config.keyframe_interval;
            av1.set_repeatSeqHdr(1);
            av1.set_outputAnnexBFormat(0);
            av1.set_chromaFormatIDC(1);
            av1.colorPrimaries = NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
            av1.transferCharacteristics =
                NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
            av1.matrixCoefficients = NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709;
            av1.colorRange = 0;
            let mut init: NV_ENC_INITIALIZE_PARAMS = std::mem::zeroed();
            init.version = NV_ENC_INITIALIZE_PARAMS_VER;
            init.encodeGUID = NV_ENC_CODEC_AV1_GUID;
            init.presetGUID = NV_ENC_PRESET_P3_GUID;
            init.encodeWidth = config.width;
            init.encodeHeight = config.height;
            init.darWidth = config.width;
            init.darHeight = config.height;
            init.frameRateNum = config.fps;
            init.frameRateDen = 1;
            init.enableEncodeAsync = 1;
            init.enablePTD = 1;
            init.encodeConfig = enc;
            init.tuningInfo = NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY;
            result.check(
                api.nvEncInitializeEncoder.unwrap()(result.session, &mut init),
                "initialize AV1",
            )?;
            result.eos_event = CreateEventW(None, false, false, None)?;
            let mut eos: NV_ENC_EVENT_PARAMS = std::mem::zeroed();
            eos.version = NV_ENC_EVENT_PARAMS_VER;
            eos.completionEvent = result.eos_event.0;
            result.check(
                api.nvEncRegisterAsyncEvent.unwrap()(result.session, &mut eos),
                "register drain event",
            )?;
            result.eos_registered = true;
            for _ in 0..POOL {
                let tex = crate::gpu::texture(
                    device,
                    config.width,
                    config.height,
                    windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12,
                    false,
                )?;
                // Insert before acquiring native resources so Drop handles partial init.
                result.slots.push(Slot {
                    texture: tex,
                    registered: ptr::null_mut(),
                    mapped: ptr::null_mut(),
                    bitstream: ptr::null_mut(),
                    event: HANDLE::default(),
                    event_registered: false,
                });
                let i = result.slots.len() - 1;
                let mut register: NV_ENC_REGISTER_RESOURCE = std::mem::zeroed();
                register.version = NV_ENC_REGISTER_RESOURCE_VER;
                register.resourceType =
                    NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
                register.width = config.width;
                register.height = config.height;
                register.resourceToRegister = result.slots[i].texture.as_raw();
                register.bufferFormat = NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12;
                register.bufferUsage = NV_ENC_BUFFER_USAGE::NV_ENC_INPUT_IMAGE;
                result.check(
                    api.nvEncRegisterResource.unwrap()(result.session, &mut register),
                    "register NV12 texture",
                )?;
                result.slots[i].registered = register.registeredResource;
                let mut bitstream: NV_ENC_CREATE_BITSTREAM_BUFFER = std::mem::zeroed();
                bitstream.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
                result.check(
                    api.nvEncCreateBitstreamBuffer.unwrap()(result.session, &mut bitstream),
                    "create bitstream",
                )?;
                result.slots[i].bitstream = bitstream.bitstreamBuffer;
                let event = CreateEventW(None, false, false, None)?;
                result.slots[i].event = event;
                let mut ev: NV_ENC_EVENT_PARAMS = std::mem::zeroed();
                ev.version = NV_ENC_EVENT_PARAMS_VER;
                ev.completionEvent = event.0;
                result.check(
                    api.nvEncRegisterAsyncEvent.unwrap()(result.session, &mut ev),
                    "register completion event",
                )?;
                result.slots[i].event_registered = true;
            }
            Ok(result)
        }
    }

    fn check(&self, status: NVENCSTATUS, operation: &str) -> Result<()> {
        if status == NVENCSTATUS::NV_ENC_SUCCESS {
            return Ok(());
        }
        let detail = if self.session.is_null() {
            String::new()
        } else {
            unsafe {
                self.api
                    .nvEncGetLastErrorString
                    .and_then(|f| {
                        let p = f(self.session);
                        (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
                    })
                    .unwrap_or_default()
            }
        };
        bail!("NVENC {operation}: {status:?} {detail}")
    }
    pub fn free_surface(&self) -> Option<(usize, ID3D11Texture2D)> {
        self.slots
            .iter()
            .enumerate()
            .find(|(i, _)| !self.pending.contains(i))
            .map(|(i, s)| (i, s.texture.clone()))
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    pub fn textures(&self) -> Vec<ID3D11Texture2D> {
        self.slots.iter().map(|s| s.texture.clone()).collect()
    }
}

impl VideoEncoder for Nvenc {
    type Surface = usize;
    fn capabilities(&self) -> EncoderCapabilities {
        EncoderCapabilities {
            backend: "NVENC / D3D11",
            codecs: vec![Codec::Av1],
            gpu_resident_input: true,
        }
    }
    fn submit(&mut self, index: &usize, pts: u64, force_keyframe: bool) -> Result<()> {
        ensure!(!self.flushing, "cannot submit after encoder flush");
        ensure!(
            *index < self.slots.len() && !self.pending.contains(index),
            "encoder surface is busy or invalid"
        );
        unsafe {
            let mut map: NV_ENC_MAP_INPUT_RESOURCE = std::mem::zeroed();
            map.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
            map.registeredResource = self.slots[*index].registered;
            self.check(
                self.api.nvEncMapInputResource.unwrap()(self.session, &mut map),
                "map input",
            )?;
            self.slots[*index].mapped = map.mappedResource;
            let mut pic: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            pic.version = NV_ENC_PIC_PARAMS_VER;
            pic.inputBuffer = map.mappedResource;
            pic.bufferFmt = NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12;
            pic.inputWidth = self.config.width;
            pic.inputHeight = self.config.height;
            pic.inputTimeStamp = pts;
            pic.inputDuration = 1;
            pic.pictureStruct = NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME;
            pic.outputBitstream = self.slots[*index].bitstream;
            pic.completionEvent = self.slots[*index].event.0;
            if force_keyframe {
                pic.encodePicFlags = NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR.0 as u32;
            }
            let status = self.api.nvEncEncodePicture.unwrap()(self.session, &mut pic);
            if status != NVENCSTATUS::NV_ENC_SUCCESS
                && status != NVENCSTATUS::NV_ENC_ERR_NEED_MORE_INPUT
            {
                let _ = self.api.nvEncUnmapInputResource.unwrap()(self.session, map.mappedResource);
                self.slots[*index].mapped = ptr::null_mut();
                return self.check(status, "submit picture");
            }
            self.pending.push_back(*index);
        }
        Ok(())
    }
    fn receive(&mut self, wait: bool) -> Result<Option<EncodedPacket>> {
        // An auto-reset completion event may already have been consumed when
        // locking/unmapping fails. Retrying that slot during finalization or
        // Drop can only repeat the timeout, not recover its packet.
        ensure!(!self.completion_failed, "NVENC completion previously failed");
        let result = self.receive_packet(wait);
        if result.is_err() {
            self.completion_failed = true;
        }
        result
    }
    fn flush(&mut self) -> Result<()> {
        if self.flushing {
            return Ok(());
        }
        self.drain_deadline = Some(Instant::now() + Duration::from_secs(5));
        unsafe {
            let mut pic: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            pic.version = NV_ENC_PIC_PARAMS_VER;
            pic.encodePicFlags = NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_EOS.0 as u32;
            pic.completionEvent = self.eos_event.0;
            self.check(
                self.api.nvEncEncodePicture.unwrap()(self.session, &mut pic),
                "flush",
            )?;
        }
        self.flushing = true;
        Ok(())
    }
}

impl Nvenc {
    fn completion_timeout_ms(&self) -> u32 {
        completion_timeout_ms(self.drain_deadline, Instant::now())
    }

    fn receive_packet(&mut self, wait: bool) -> Result<Option<EncodedPacket>> {
        let Some(&i) = self.pending.front() else {
            return Ok(None);
        };
        unsafe {
            let signaled = WaitForSingleObject(self.slots[i].event, if wait { self.completion_timeout_ms() } else { 0 });
            if signaled == WAIT_TIMEOUT && !wait {
                return Ok(None);
            }
            ensure!(
                signaled == WAIT_OBJECT_0,
                "NVENC completion wait failed or timed out: {signaled:?}"
            );
            let mut lock: NV_ENC_LOCK_BITSTREAM = std::mem::zeroed();
            lock.version = NV_ENC_LOCK_BITSTREAM_VER;
            lock.outputBitstream = self.slots[i].bitstream;
            self.check(
                self.api.nvEncLockBitstream.unwrap()(self.session, &mut lock),
                "lock bitstream",
            )?;
            // Only compressed bytes cross to CPU memory. Unlock before any fallible I/O.
            let data = std::slice::from_raw_parts(
                lock.bitstreamBufferPtr.cast::<u8>(),
                lock.bitstreamSizeInBytes as usize,
            )
            .into();
            let unlocked =
                self.api.nvEncUnlockBitstream.unwrap()(self.session, self.slots[i].bitstream);
            let unmapped =
                self.api.nvEncUnmapInputResource.unwrap()(self.session, self.slots[i].mapped);
            self.slots[i].mapped = ptr::null_mut();
            self.pending.pop_front();
            self.check(unlocked, "unlock bitstream")?;
            self.check(unmapped, "unmap input")?;
            Ok(Some(EncodedPacket {
                data,
                pts: lock.outputTimeStamp,
                keyframe: lock.pictureType == NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR
                    || lock.pictureType == NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_I,
            }))
        }
    }
}

impl Drop for Nvenc {
    fn drop(&mut self) {
        if self.session.is_null() {
            return;
        }
        unsafe {
            let _ = self.flush();
            // Drain before destroying mapped textures. Bound waits protect shutdown on device loss.
            while !self.completion_failed && !self.pending.is_empty() {
                if self.receive(true).is_err() {
                    break;
                }
            }
            if !self.completion_failed && self.flushing && !self.eos_event.is_invalid() {
                let _ = WaitForSingleObject(self.eos_event, self.completion_timeout_ms());
            }
            for slot in &mut self.slots {
                if !slot.mapped.is_null() {
                    let _ = self.api.nvEncUnmapInputResource.unwrap()(self.session, slot.mapped);
                }
                if slot.event_registered {
                    let mut ev: NV_ENC_EVENT_PARAMS = std::mem::zeroed();
                    ev.version = NV_ENC_EVENT_PARAMS_VER;
                    ev.completionEvent = slot.event.0;
                    let _ = self.api.nvEncUnregisterAsyncEvent.unwrap()(self.session, &mut ev);
                }
                if !slot.bitstream.is_null() {
                    let _ =
                        self.api.nvEncDestroyBitstreamBuffer.unwrap()(self.session, slot.bitstream);
                }
                if !slot.registered.is_null() {
                    let _ =
                        self.api.nvEncUnregisterResource.unwrap()(self.session, slot.registered);
                }
                if !slot.event.is_invalid() {
                    let _ = CloseHandle(slot.event);
                }
            }
            if self.eos_registered {
                let mut ev: NV_ENC_EVENT_PARAMS = std::mem::zeroed();
                ev.version = NV_ENC_EVENT_PARAMS_VER;
                ev.completionEvent = self.eos_event.0;
                let _ = self.api.nvEncUnregisterAsyncEvent.unwrap()(self.session, &mut ev);
            }
            if !self.eos_event.is_invalid() {
                let _ = CloseHandle(self.eos_event);
            }
            let _ = self.api.nvEncDestroyEncoder.unwrap()(self.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_packets_and_eos_share_one_completion_wait_budget() {
        let start = Instant::now();
        let deadline = Some(start + Duration::from_secs(5));
        assert_eq!(completion_timeout_ms(None, start), 5000);
        assert_eq!(completion_timeout_ms(deadline, start), 5000);
        assert_eq!(completion_timeout_ms(deadline, start + Duration::from_millis(4500)), 500);
        assert_eq!(completion_timeout_ms(deadline, start + Duration::from_secs(5)), 0);
        // EOS and destructor cleanup cannot start another five-second wait.
        assert_eq!(completion_timeout_ms(deadline, start + Duration::from_secs(20)), 0);
    }
}
