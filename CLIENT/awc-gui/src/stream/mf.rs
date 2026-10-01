//! Media Foundation hardware decode path (Windows, via `windows-rs`).
//!
//! Uses the inbox decoder MFTs (`CLSID_MSH264DecoderMFT` / `CLSID_MSH265DecoderMFT`)
//! with AVCC/hvcC sequence headers built from the SDP/in-band parameter sets, plus the
//! inbox video processor MFT (`CLSID_VideoProcessorMFT`) for hardware rescale + NV12
//! conversion straight to the virtual-camera target size. Falls back to the software
//! decoders in [`crate::stream::rtsp`] when MF is unavailable (see `MfRtspDecoder::new`).

use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::OnceLock;

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

use crate::stream::rtsp::DecodeResult;
use crate::stream::rtsp_client::RtspCodec;

const FCC_NV12: u32 = u32::from_le_bytes([b'N', b'V', b'1', b'2']);
const TS_STEP_100NS: i64 = 333_333; // ~30fps in 100ns units

static MF_STARTUP: OnceLock<Result<(), String>> = OnceLock::new();

fn mf_ensure_started() -> Result<(), String> {
    MF_STARTUP
        .get_or_init(|| unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|e| format!("MFStartup failed: {e:?}"))
                .map(|_| ())
        })
        .clone()
}

fn com_ensure_init() {
    // MTA for the worker thread. eframe owns STA on the UI thread; decode never
    // runs there, so this is safe. Errors (already-initialized) are benign.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

#[inline]
fn dbg_log(msg: String) {
    if std::env::var("MF_DEBUG").is_ok() {
        eprintln!("[mf-debug] {msg}");
    }
}

struct D3DState {
    _device: ID3D11Device,
    manager: IMFDXGIDeviceManager,
}

/// Create a VIDEO-capable D3D11 device and an MF DXGI device manager.
/// Stored per-decoder (COM interfaces are not Send/Sync for statics).
/// Unlocks the DXVA hardware path in decoder/processor MFTs.
fn create_d3d_state() -> Result<D3DState, String> {
    unsafe {
        let mut dev_opt: Option<ID3D11Device> = None;
        let mut fl = D3D_FEATURE_LEVEL_11_0;
        let mut ctx_opt: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev_opt as *mut _),
            Some(&mut fl as *mut _),
            Some(&mut ctx_opt as *mut _),
        )
        .map_err(|e| format!("D3D11CreateDevice failed: {e:?}"))?;
        let device = dev_opt.ok_or("no D3D11 device returned")?;
        let mut token = 0u32;
        let mut mgr_opt: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut mgr_opt)
            .map_err(|e| format!("MFCreateDXGIDeviceManager failed: {e:?}"))?;
        let manager = mgr_opt.ok_or("no DXGI device manager returned")?;
        manager
            .ResetDevice(&device, token)
            .map_err(|e| format!("ResetDevice failed: {e:?}"))?;
        Ok(D3DState {
            _device: device,
            manager,
        })
    }
}

fn attach_d3d_manager(mft: &IMFTransform, st: &D3DState) -> Result<(), String> {
    unsafe {
        // The raw COM object pointer, NOT the address of the wrapper struct.
        let ptr = Interface::as_raw(&st.manager) as usize;
        mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, ptr)
            .map_err(|e| format!("SET_D3D_MANAGER failed: {e:?}"))
    }
}

#[inline]
fn annexb_offset(nalu: &[u8]) -> Option<usize> {
    if nalu.starts_with(&[0, 0, 0, 1]) {
        Some(4)
    } else if nalu.starts_with(&[0, 0, 1]) {
        Some(3)
    } else {
        None
    }
}

#[inline]
fn to_avcc(nalu: &[u8], out: &mut Vec<u8>) -> bool {
    let off = match annexb_offset(nalu) {
        Some(o) if o < nalu.len() => o,
        _ => return false,
    };
    let body = &nalu[off..];
    let len = body.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    true
}

/// Build an AVCCDecoderConfigurationRecord from raw SPS/PPS NAL payloads (no start codes).
fn build_avcc(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4 || pps.is_empty() {
        return None;
    }
    let mut v = Vec::with_capacity(sps.len() + pps.len() + 11);
    v.push(0x01);
    v.push(sps[1]); // profile_idc
    v.push(sps[2]); // profile_compat
    v.push(sps[3]); // level_idc
    v.push(0xFF); // lengthSizeMinusOne = 4 bytes
    v.push(0xE1); // 1 SPS
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(0x01); // 1 PPS
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    Some(v)
}

/// Build an HEVCDecoderConfigurationRecord (hvcC) from raw VPS/SPS/PPS payloads.
fn build_hvcc(vps: &[u8], sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    // profile_tier_level (12 bytes for a single layer) starts at SPS offset 2.
    if sps.len() < 14 || vps.is_empty() || pps.is_empty() {
        return None;
    }
    let ptl = &sps[2..14];
    let mut v = Vec::with_capacity(vps.len() + sps.len() + pps.len() + 30);
    v.push(0x01); // configurationVersion
    v.push(ptl[0]); // profile_space(2) + tier(1) + profile_idc(5)
    v.extend_from_slice(&ptl[1..5]); // general_profile_compatibility_flags
    v.extend_from_slice(&ptl[5..11]); // general_constraint_indicator_flags
    v.push(ptl[11]); // general_level_idc
    v.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc
    v.push(0xFC); // parallelismType
    v.push(0xFD); // chromaFormat 4:2:0
    v.push(0xF8); // bitDepthLumaMinus8 = 0 (8-bit)
    v.push(0xF8); // bitDepthChromaMinus8 = 0
    v.extend_from_slice(&[0x00, 0x00]); // avgFrameRate
    v.push(0x03); // lengthSizeMinusOne = 4 bytes
    v.push(0x03); // numOfArrays = 3
    for (nal_type, data) in [(32u8, vps), (33u8, sps), (34u8, pps)] {
        v.push(0x80 | nal_type);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&(data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
    }
    Some(v)
}

fn make_input_sample(payload: &[u8], ts: i64) -> Result<IMFSample, String> {
    unsafe {
        let buf = MFCreateMemoryBuffer(payload.len() as u32)
            .map_err(|e| format!("MFCreateMemoryBuffer failed: {e:?}"))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut maxlen = 0u32;
        let mut curlen = 0u32;
        buf.Lock(&mut ptr, Some(&mut maxlen), Some(&mut curlen))
            .map_err(|e| format!("Lock failed: {e:?}"))?;
        if (maxlen as usize) < payload.len() {
            let _ = buf.Unlock();
            return Err("input buffer too small".into());
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), ptr, payload.len());
        // NOTE: length MUST be set while still locked (matches the working
        // reference: some buffer implementations snapshot state at Unlock).
        buf.SetCurrentLength(payload.len() as u32)
            .map_err(|e| format!("SetCurrentLength failed: {e:?}"))?;
        buf.Unlock().map_err(|e| format!("Unlock failed: {e:?}"))?;
        let sample = MFCreateSample().map_err(|e| format!("MFCreateSample failed: {e:?}"))?;
        sample
            .AddBuffer(&buf)
            .map_err(|e| format!("AddBuffer failed: {e:?}"))?;
        // Optional A/B: mark samples clean-point (helps some MFTs emit sooner).
        if std::env::var("MF_CLEANPOINT").is_ok() {
            let _ = sample.SetUINT32(&MFSampleExtension_CleanPoint, 1);
        }
        // MF_NO_TS experiment: some MFTs stall on synthetic timestamps.
        if std::env::var("MF_NO_TS").is_err() {
            sample
                .SetSampleTime(ts)
                .map_err(|e| format!("SetSampleTime failed: {e:?}"))?;
            sample
                .SetSampleDuration(TS_STEP_100NS)
                .map_err(|e| format!("SetSampleDuration failed: {e:?}"))?;
        }
        Ok(sample)
    }
}

fn make_2d_nv12_sample(w: u32, h: u32) -> Result<IMFSample, String> {
    unsafe {
        let buf = MFCreate2DMediaBuffer(w, h, FCC_NV12, false)
            .map_err(|e| format!("MFCreate2DMediaBuffer failed: {e:?}"))?;
        let sample = MFCreateSample().map_err(|e| format!("MFCreateSample failed: {e:?}"))?;
        sample
            .AddBuffer(&buf)
            .map_err(|e| format!("AddBuffer failed: {e:?}"))?;
        Ok(sample)
    }
}

fn copy_2d_nv12_to_vec(sample: &IMFSample, w: u32, h: u32, out: &mut Vec<u8>) -> Result<(), String> {
    unsafe {
        let buf = sample
            .GetBufferByIndex(0)
            .map_err(|e| format!("GetBufferByIndex failed: {e:?}"))?;
        let b2d: IMF2DBuffer = buf
            .cast()
            .map_err(|e| format!("output buffer is not 2D: {e:?}"))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut pitch = 0i32;
        b2d.Lock2D(&mut ptr, &mut pitch)
            .map_err(|e| format!("Lock2D failed: {e:?}"))?;
        let pitch = pitch as usize;
        let w = w as usize;
        let h = h as usize;
        let total = w * h * 3 / 2;
        if out.len() != total {
            out.resize(total, 0);
        }
        if pitch < w {
            let _ = b2d.Unlock2D();
            return Err("unexpected pitch".into());
        }
        let (dst_y, dst_uv) = out.split_at_mut(w * h);
        for row in 0..h {
            let src = std::slice::from_raw_parts(ptr.add(row * pitch), w);
            dst_y[row * w..(row + 1) * w].copy_from_slice(src);
        }
        let uv_base = ptr.add(pitch * h);
        for row in 0..h / 2 {
            let src = std::slice::from_raw_parts(uv_base.add(row * pitch), w);
            dst_uv[row * w..(row + 1) * w].copy_from_slice(src);
        }
        b2d.Unlock2D().map_err(|e| format!("Unlock2D failed: {e:?}"))?;
        Ok(())
    }
}

fn fill_2d_nv12_from_contiguous(sample: &IMFSample, w: u32, h: u32, src: &[u8]) -> Result<(), String> {
    unsafe {
        let w = w as usize;
        let h = h as usize;
        if src.len() < w * h * 3 / 2 {
            return Err("staging NV12 too small".into());
        }
        let buf = sample
            .GetBufferByIndex(0)
            .map_err(|e| format!("GetBufferByIndex failed: {e:?}"))?;
        let b2d: IMF2DBuffer = buf
            .cast()
            .map_err(|e| format!("buffer is not 2D: {e:?}"))?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut pitch = 0i32;
        b2d.Lock2D(&mut ptr, &mut pitch)
            .map_err(|e| format!("Lock2D failed: {e:?}"))?;
        let pitch = pitch as usize;
        if pitch < w {
            let _ = b2d.Unlock2D();
            return Err("unexpected pitch".into());
        }
        let (src_y, src_uv) = src.split_at(w * h);
        for row in 0..h {
            let dst = std::slice::from_raw_parts_mut(ptr.add(row * pitch), w);
            dst.copy_from_slice(&src_y[row * w..(row + 1) * w]);
        }
        let uv_base = ptr.add(pitch * h);
        for row in 0..h / 2 {
            let dst = std::slice::from_raw_parts_mut(uv_base.add(row * pitch), w);
            dst.copy_from_slice(&src_uv[row * w..(row + 1) * w]);
        }
        b2d.Unlock2D().map_err(|e| format!("Unlock2D failed: {e:?}"))?;
        Ok(())
    }
}

fn frame_size_of(mt: &IMFMediaType) -> Result<(u32, u32), String> {
    unsafe {
        let packed = mt
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| format!("MF_MT_FRAME_SIZE missing: {e:?}"))?;
        Ok((
            ((packed >> 32) & 0xFFFF_FFFF) as u32,
            (packed & 0xFFFF_FFFF) as u32,
        ))
    }
}

/// MF_EXP_ATTRS=1 experiment: full input type (size + rate + progressive +
/// square pixels) set in a second phase after output negotiation reveals dims.
fn enrich_input_type(mt: &IMFMediaType, w: u32, h: u32) -> Result<(), String> {
    unsafe {
        let packed = ((w as u64) << 32) | (h as u64);
        mt.SetUINT64(&MF_MT_FRAME_SIZE, packed)
            .map_err(|e| format!("input frame size failed: {e:?}"))?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, (30u64 << 32) | 1)
            .map_err(|e| format!("input frame rate failed: {e:?}"))?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| format!("input interlace failed: {e:?}"))?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(|e| format!("input aspect failed: {e:?}"))?;
        Ok(())
    }
}

fn new_video_type(subtype: &GUID, w: u32, h: u32, blob: Option<&[u8]>) -> Result<IMFMediaType, String> {
    unsafe {
        let mt = MFCreateMediaType().map_err(|e| format!("MFCreateMediaType failed: {e:?}"))?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| format!("SetGUID major failed: {e:?}"))?;
        mt.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|e| format!("SetGUID subtype failed: {e:?}"))?;
        if w > 0 && h > 0 {
            let packed = ((w as u64) << 32) | (h as u64);
            mt.SetUINT64(&MF_MT_FRAME_SIZE, packed)
                .map_err(|e| format!("SetUINT64 frame size failed: {e:?}"))?;
        }
        if let Some(b) = blob {
            mt.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, b)
                .map_err(|e| format!("SetBlob sequence header failed: {e:?}"))?;
        }
        Ok(mt)
    }
}

struct MftPipe {
    mft: IMFTransform,
}

impl MftPipe {
    fn create_decoder(codec: RtspCodec) -> Result<Self, String> {
        unsafe {
            let clsid = match codec {
                RtspCodec::H264 => &CLSID_MSH264DecoderMFT,
                RtspCodec::H265 => &CLSID_MSH265DecoderMFT,
            };
            let mft: IMFTransform = CoCreateInstance(clsid, None, CLSCTX_ALL)
                .map_err(|e| format!("decoder MFT unavailable ({codec:?}): {e:?}"))?;
            Ok(Self { mft })
        }
    }

    fn create_processor() -> Result<Self, String> {
        unsafe {
            let mft: IMFTransform = CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_ALL)
                .map_err(|e| format!("video processor MFT unavailable: {e:?}"))?;
            Ok(Self { mft })
        }
    }

    fn set_input(&self, mt: &IMFMediaType) -> Result<(), String> {
        unsafe {
            self.mft
                .SetInputType(0, mt, 0)
                .map_err(|e| format!("SetInputType failed: {e:?}"))
        }
    }

    /// Pick NV12 output when offered, else fall back to the first available type.
    /// MF_FIRST_OUT=1 forces the first offered type (diagnostic).
    /// MF_LIST_OUT=1 logs every offered output type.
    fn set_output_prefer_nv12(&self) -> Result<IMFMediaType, String> {
        unsafe {
            let force_first = std::env::var("MF_FIRST_OUT").is_ok();
            let mut first: Option<IMFMediaType> = None;
            for idx in 0..64u32 {
                let mt = match self.mft.GetOutputAvailableType(0, idx) {
                    Ok(m) => m,
                    Err(_) => break,
                };
                if std::env::var("MF_LIST_OUT").is_ok() {
                    let st = mt
                        .GetGUID(&MF_MT_SUBTYPE)
                        .map(|g| format!("{g:?}"))
                        .unwrap_or_else(|_| "?".into());
                    let sz = frame_size_of(&mt).unwrap_or((0, 0));
                    dbg_log(format!("offered output[{idx}]: {st} {sz:?}"));
                }
                if force_first && idx == 0 {
                    self.mft
                        .SetOutputType(0, &mt, 0)
                        .map_err(|e| format!("SetOutputType failed: {e:?}"))?;
                    return Ok(mt);
                }
                if first.is_none() {
                    first = Some(mt.clone());
                }
                let is_nv12 = mt
                    .GetGUID(&MF_MT_SUBTYPE)
                    .map(|g| g == MFVideoFormat_NV12)
                    .unwrap_or(false);
                if is_nv12 {
                    self.mft
                        .SetOutputType(0, &mt, 0)
                        .map_err(|e| format!("SetOutputType failed: {e:?}"))?;
                    return Ok(mt);
                }
            }
            match first {
                Some(mt) => {
                    self.mft
                        .SetOutputType(0, &mt, 0)
                        .map_err(|e| format!("SetOutputType failed: {e:?}"))?;
                    Ok(mt)
                }
                None => Err("decoder offers no output types".into()),
            }
        }
    }

    fn refresh_output_type(&self) -> Result<IMFMediaType, String> {
        unsafe {
            // After MF_E_TRANSFORM_STREAM_CHANGE, re-resolve and re-set the output type.
            for idx in 0..64u32 {
                let mt = match self.mft.GetOutputAvailableType(0, idx) {
                    Ok(m) => m,
                    Err(_) => break,
                };
                let is_nv12 = mt
                    .GetGUID(&MF_MT_SUBTYPE)
                    .map(|g| g == MFVideoFormat_NV12)
                    .unwrap_or(false);
                if is_nv12 {
                    self.mft
                        .SetOutputType(0, &mt, 0)
                        .map_err(|e| format!("SetOutputType failed: {e:?}"))?;
                    return Ok(mt);
                }
            }
            Err("stream change: no usable output type".into())
        }
    }

    fn feed(&self, sample: &IMFSample) -> Result<(), String> {
        unsafe {
            // MF_NO_GATE=1: feed blindly like the reference (no GetInputStatus).
            if std::env::var("MF_NO_GATE").is_err() {
                // Drain-first if the transform is not accepting input.
                match self.mft.GetInputStatus(0) {
                    Ok(flags) if (flags & MFT_INPUT_STATUS_ACCEPT_DATA.0 as u32) == 0 => {
                        return Err("not-accepting".into());
                    }
                    Err(e) => return Err(format!("GetInputStatus failed: {e:?}")),
                    _ => {}
                }
            }
            self.mft
                .ProcessInput(0, sample, 0)
                .map_err(|e| format!("ProcessInput failed: {e:?}"))
        }
    }

    fn log_stream_info(&self) {
        unsafe {
            let mut ii = std::mem::zeroed::<MFT_INPUT_STREAM_INFO>();
            if self.mft.GetInputStreamInfo(0, &mut ii).is_ok() {
                dbg_log(format!(
                    "input stream info: flags={:#x} size={} align={}",
                    ii.dwFlags, ii.cbSize, ii.cbAlignment
                ));
            }
            if let Ok(oi) = self.mft.GetOutputStreamInfo(0) {
                dbg_log(format!(
                    "output stream info: flags={:#x} size={} align={}",
                    oi.dwFlags, oi.cbSize, oi.cbAlignment
                ));
            }
        }
    }

    /// Try to pull one output frame into a caller-provided 2D NV12 sample.
    /// Ok(true) = frame produced, Ok(false) = need more input.
    fn drain_into(&self, out_sample: &IMFSample) -> Result<bool, String> {
        unsafe {
            let mut ob: MFT_OUTPUT_DATA_BUFFER = MaybeUninit::zeroed().assume_init();
            ob.dwStreamID = 0;
            ob.pSample = ManuallyDrop::new(Some(out_sample.clone()));
            let mut status = 0u32;
            let res = self
                .mft
                .ProcessOutput(0, std::slice::from_mut(&mut ob), &mut status);
            ManuallyDrop::drop(&mut ob.pSample);
            match res {
                Ok(()) => Ok(true),
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Err("stream-change".into()),
                Err(e) => Err(format!("ProcessOutput failed: {e:?}")),
            }
        }
    }
}

/// Lossless self-check for [`to_avcc`]: AVCC(NALU) converted back must equal input.
pub fn mf_avcc_roundtrip_check() {
    let cases: Vec<Vec<u8>> = vec![
        vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80, 0x0A, 0xDA, 0x01],
        vec![0x00, 0x00, 0x01, 0x68, 0xCE, 0x06, 0xF2],
        vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xB8, 0x00, 0x2B, 0x00, 0x00, 0x03, 0x01],
    ];
    for nalu in &cases {
        let mut avcc = Vec::new();
        assert!(to_avcc(nalu, &mut avcc), "to_avcc rejected {nalu:02X?}");
        let off = annexb_offset(nalu).unwrap();
        assert_eq!(&avcc[..4], &((nalu.len() - off) as u32).to_be_bytes());
        assert_eq!(&avcc[4..], &nalu[off..]);
    }
}

/// Normalize an encoder output packet to Annex-B (start codes). Handles both
/// AVCC (length-prefixed) and pass-through Annex-B.
fn packet_to_annexb(pkt: &[u8], out: &mut Vec<u8>) {
    if pkt.starts_with(&[0, 0, 0, 1]) || pkt.starts_with(&[0, 0, 1]) {
        out.extend_from_slice(pkt);
        return;
    }
    let mut off = 0usize;
    let mut ok = false;
    while off + 4 <= pkt.len() {
        let len =
            u32::from_be_bytes([pkt[off], pkt[off + 1], pkt[off + 2], pkt[off + 3]]) as usize;
        if len == 0 || off + 4 + len > pkt.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&pkt[off + 4..off + 4 + len]);
        off += 4 + len;
        ok = true;
    }
    if !ok {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(pkt);
    }
}

/// Encoder->decoder round-trip used by tests: synthesize NV12 frames, encode with
/// the inbox H264 encoder MFT, feed packets into a fresh [`MfRtspDecoder`].
/// Returns (encoder packets, decoded frames, first frame dims).
pub fn mf_roundtrip_selftest() -> Result<(usize, usize, (u32, u32)), String> {
    mf_ensure_started()?;
    com_ensure_init();
    const W: u32 = 640;
    const H: u32 = 480;
    const NFRAMES: usize = 60;

    let enc: IMFTransform = unsafe {
        CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_ALL)
            .map_err(|e| format!("encoder MFT unavailable: {e:?}"))?
    };
    let in_mt = new_video_type(&MFVideoFormat_NV12, W, H, None)?;
    unsafe {
        in_mt
            .SetUINT64(&MF_MT_FRAME_RATE, (30u64 << 32) | 1)
            .map_err(|e| format!("enc input rate failed: {e:?}"))?;
    }
    let out_mt = new_video_type(&MFVideoFormat_H264, W, H, None)?;
    unsafe {
        out_mt
            .SetUINT64(&MF_MT_FRAME_RATE, (30u64 << 32) | 1)
            .map_err(|e| format!("enc output rate failed: {e:?}"))?;
        out_mt
            .SetUINT32(&MF_MT_AVG_BITRATE, 2_000_000)
            .map_err(|e| format!("enc output bitrate failed: {e:?}"))?;
        out_mt
            .SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_Progressive.0 as u32,
            )
            .map_err(|e| format!("enc output interlace failed: {e:?}"))?;
        // Encoders typically require the output type before the input type.
        enc.SetOutputType(0, &out_mt, 0)
            .map_err(|e| format!("enc SetOutputType failed: {e:?}"))?;
        enc.SetInputType(0, &in_mt, 0)
            .map_err(|e| format!("enc SetInputType failed: {e:?}"))?;
    }
    dbg_log("roundtrip: encoder configured".to_string());

    // Synthetic moving-gradient NV12 frames.
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut ts = 0i64;
    let mut enc_buf = vec![0u8; W as usize * H as usize * 3 / 2];
    for f in 0..NFRAMES {
        for y in 0..H as usize {
            for x in 0..W as usize {
                enc_buf[y * W as usize + x] = ((x + y + f * 4) & 0xFF) as u8;
            }
        }
        for b in enc_buf[W as usize * H as usize..].iter_mut() {
            *b = 128;
        }
        ts += TS_STEP_100NS;
        let sample = make_input_sample(&enc_buf, ts)?;
        unsafe {
            match enc.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) => return Err(format!("enc ProcessInput failed: {e:?}")),
            }
        }
        drain_mft_to_packets(&enc, &mut packets)?;
    }
    // Flush the encoder.
    for _ in 0..30 {
        if drain_mft_to_packets(&enc, &mut packets)? == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    dbg_log(format!("roundtrip: encoder emitted {} packets", packets.len()));
    if packets.is_empty() {
        return Err("encoder emitted nothing".into());
    }

    // Feed normalized Annex-B into a fresh decoder, primed from the encoder's
    // own AVCC sequence-header blob (encoders typically keep SPS/PPS out of band).
    if !packets.is_empty() {
        dbg_log(format!(
            "roundtrip: first packet {}B head={:02X?}",
            packets[0].len(),
            packets[0][..packets[0].len().min(16)].to_vec()
        ));
    }
    let mut dec = MfRtspDecoder::new(RtspCodec::H264)?;
    unsafe {
        if let Ok(cur) = enc.GetOutputCurrentType(0) {
            if let Ok(sz) = cur.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                let mut blob = vec![0u8; sz as usize];
                if cur.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).is_ok() {
                    // Encoders may store concatenated Annex-B here instead of AVCC.
                    if blob.first() == Some(&0x01) {
                        dec.prime_avcc_blob(&blob);
                    } else {
                        for nalu in split_annexb(&blob) {
                            dec.ingest_params(&nalu);
                        }
                    }
                    dbg_log(format!(
                        "roundtrip: after prime sps={}B pps={}B",
                        dec.sps.len(),
                        dec.pps.len()
                    ));
                }
            }
        }
    }
    let mut nv12 = Vec::new();
    let mut rgba = Vec::new();
    let mut frames = 0usize;
    let mut dims = (0u32, 0u32);
    // Split normalized stream into individual NALUs for feeding.
    let mut stream = Vec::new();
    for p in &packets {
        packet_to_annexb(p, &mut stream);
    }
    for nalu in split_annexb(&stream) {
        match dec.decode_into_target(&nalu, W, H, &mut nv12, &mut rgba, true)? {
            Some(r) => {
                frames += 1;
                if dims == (0, 0) {
                    dims = (r.src_w, r.src_h);
                }
            }
            None => {}
        }
    }
    // Drain any trailing frames.
    for _ in 0..20 {
        match dec.drain_decoder()? {
            Some(raw) => {
                if let Some(r) = dec.finish_frame(raw, W, H, &mut nv12, &mut rgba, true)? {
                    frames += 1;
                    if dims == (0, 0) {
                        dims = (r.src_w, r.src_h);
                    }
                }
            }
            None => break,
        }
    }
    Ok((packets.len(), frames, dims))
}

/// Drain one MFT output (1D buffer) into packet vecs. Returns packets produced.
fn drain_mft_to_packets(mft: &IMFTransform, packets: &mut Vec<Vec<u8>>) -> Result<usize, String> {
    unsafe {
        let buf = MFCreateMemoryBuffer(1 << 20)
            .map_err(|e| format!("enc out buffer failed: {e:?}"))?;
        let sample =
            MFCreateSample().map_err(|e| format!("enc out sample failed: {e:?}"))?;
        sample
            .AddBuffer(&buf)
            .map_err(|e| format!("enc AddBuffer failed: {e:?}"))?;
        let mut ob: MFT_OUTPUT_DATA_BUFFER = MaybeUninit::zeroed().assume_init();
        ob.dwStreamID = 0;
        ob.pSample = ManuallyDrop::new(Some(sample));
        let mut status = 0u32;
        let res = mft.ProcessOutput(0, std::slice::from_mut(&mut ob), &mut status);
        ManuallyDrop::drop(&mut ob.pSample);
        match res {
            Ok(()) => {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut maxlen = 0u32;
                let mut curlen = 0u32;
                buf.Lock(&mut ptr, Some(&mut maxlen), Some(&mut curlen))
                    .map_err(|e| format!("enc out lock failed: {e:?}"))?;
                let bytes =
                    std::slice::from_raw_parts(ptr, curlen as usize).to_vec();
                let _ = buf.Unlock();
                if !bytes.is_empty() {
                    packets.push(bytes);
                    return Ok(1);
                }
                Ok(0)
            }
            Err(e)
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT
                    || e.code() == MF_E_TRANSFORM_STREAM_CHANGE =>
            {
                if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    dbg_log("roundtrip: encoder stream-change (ignored)".to_string());
                }
                Ok(0)
            }
            Err(e) => Err(format!("enc ProcessOutput failed: {e:?}")),
        }
    }
}

/// Split an Annex-B byte stream into individual NALUs (each WITH its start code).
fn split_annexb(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 < stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && (stream[i + 2] == 1 || (stream[i + 2] == 0 && stream[i + 3] == 1)) {
            starts.push(i);
            i += if stream[i + 2] == 1 { 3 } else { 4 };
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let e = if k + 1 < starts.len() {
            starts[k + 1]
        } else {
            stream.len()
        };
        if e > s {
            out.push(stream[s..e].to_vec());
        }
    }
    out
}

/// One bufferless drain attempt; returns true if a frame was produced.
fn drain_once(pipe: &MftPipe) -> Result<bool, String> {
    unsafe {
        let osample =
            MFCreateSample().map_err(|e| format!("outsample failed: {e:?}"))?;
        let mut ob: MFT_OUTPUT_DATA_BUFFER = MaybeUninit::zeroed().assume_init();
        ob.dwStreamID = 0;
        ob.pSample = ManuallyDrop::new(Some(osample));
        let mut st = 0u32;
        let res = pipe
            .mft
            .ProcessOutput(0, std::slice::from_mut(&mut ob), &mut st);
        ManuallyDrop::drop(&mut ob.pSample);
        match res {
            Ok(()) => Ok(true),
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(false),
            Err(e) => Err(format!("drain failed: {e:?}")),
        }
    }
}

/// Verbatim port of the known-good C++ recipe: feed 1500B chunks with NO
/// timestamps, bufferless-probe drain, offered NV12 output. Used to isolate
/// behavior on identical bytes. Returns (chunks fed, frames out).
pub fn mf_replay_bytes(data: &[u8]) -> Result<(usize, usize), String> {
    mf_ensure_started()?;
    com_ensure_init();
    let pipe = MftPipe::create_decoder(RtspCodec::H264)?;
    // MF_BLOB_REPLAY=1: set AVCC blob extracted from the file's own SPS/PPS.
    // Tests whether the sequence header itself poisons the MFT.
    let in_mt = if std::env::var("MF_BLOB_REPLAY").is_ok() {
        let mut sps = Vec::new();
        let mut pps = Vec::new();
        for nalu in split_annexb(&data[..data.len().min(65536)]) {
            let off = annexb_offset(&nalu).unwrap_or(0);
            if off < nalu.len() {
                let body = &nalu[off..];
                if !body.is_empty() {
                    match body[0] & 0x1F {
                        7 if sps.is_empty() => sps = body.to_vec(),
                        8 if pps.is_empty() => pps = body.to_vec(),
                        _ => {}
                    }
                }
            }
            if !sps.is_empty() && !pps.is_empty() {
                break;
            }
        }
        match build_avcc(&sps, &pps) {
            Some(blob) => {
                dbg_log(format!("replay: setting AVCC blob {}B", blob.len()));
                new_video_type(&MFVideoFormat_H264, 0, 0, Some(&blob))?
            }
            None => {
                dbg_log("replay: no SPS/PPS found, no blob".to_string());
                new_video_type(&MFVideoFormat_H264, 0, 0, None)?
            }
        }
    } else {
        new_video_type(&MFVideoFormat_H264, 0, 0, None)?
    };
    pipe.set_input(&in_mt)?;
    let _out_mt = pipe.set_output_prefer_nv12()?;
    // MF_NALU_ALIGN=1: feed NALU-aligned samples instead of 1500B chunks.
    // Isolates whether chunk alignment is the operative factor.
    let owned_nalus: Vec<Vec<u8>> = if std::env::var("MF_NALU_ALIGN").is_ok() {
        split_annexb(data)
    } else {
        Vec::new()
    };
    let mut pieces: Vec<&[u8]> = Vec::new();
    if !owned_nalus.is_empty() {
        for n in &owned_nalus {
            pieces.push(n.as_slice());
        }
    } else {
        let mut off = 0usize;
        while off < data.len() {
            let end = (off + 1500).min(data.len());
            pieces.push(&data[off..end]);
            off = end;
        }
    }
    let mut fed = 0usize;
    let mut frames = 0usize;
    let mut sample_size = 0u32;
    for piece in pieces {
        if frames >= 10 {
            break;
        }
        unsafe {
            let buf = MFCreateMemoryBuffer(piece.len() as u32)
                .map_err(|e| format!("inbuf failed: {e:?}"))?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None)
                .map_err(|e| format!("inlock failed: {e:?}"))?;
            std::ptr::copy_nonoverlapping(piece.as_ptr(), ptr, piece.len());
            buf.SetCurrentLength(piece.len() as u32)
                .map_err(|e| format!("curLen failed: {e:?}"))?;
            buf.Unlock().map_err(|e| format!("inunlock failed: {e:?}"))?;
            let sample =
                MFCreateSample().map_err(|e| format!("insample failed: {e:?}"))?;
            sample
                .AddBuffer(&buf)
                .map_err(|e| format!("inadd failed: {e:?}"))?;
            // Mirror the C++ reference exactly: NOTACCEPTING is tolerated
            // (drain attempt, then move on — the chunk is dropped, like C++).
            match pipe.mft.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    let _ = drain_once(&pipe);
                }
                Err(e) => return Err(format!("ProcessInput failed at chunk {fed}: {e:?}")),
            }
        }
        fed += 1;
        // Drain attempt with bufferless sample until size known.
        unsafe {
            let osample =
                MFCreateSample().map_err(|e| format!("outsample failed: {e:?}"))?;
            if sample_size > 0 {
                let obuf = MFCreateMemoryBuffer(sample_size)
                    .map_err(|e| format!("outbuf failed: {e:?}"))?;
                osample
                    .AddBuffer(&obuf)
                    .map_err(|e| format!("outadd failed: {e:?}"))?;
            }
            let mut ob: MFT_OUTPUT_DATA_BUFFER = MaybeUninit::zeroed().assume_init();
            ob.dwStreamID = 0;
            ob.pSample = ManuallyDrop::new(Some(osample));
            let mut st = 0u32;
            match pipe
                .mft
                .ProcessOutput(0, std::slice::from_mut(&mut ob), &mut st)
            {
                Ok(()) => {
                    ManuallyDrop::drop(&mut ob.pSample);
                    frames += 1;
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    ManuallyDrop::drop(&mut ob.pSample);
                    let mt = pipe.refresh_output_type()?;
                    if let Ok(cur) = pipe.mft.GetOutputCurrentType(0) {
                        if let Ok(sz) = cur.GetUINT32(&MF_MT_SAMPLE_SIZE) {
                            sample_size = sz;
                            dbg_log(format!("replay stream-change sample_size={sz}"));
                        }
                    }
                    let _ = mt;
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    ManuallyDrop::drop(&mut ob.pSample);
                }
                Err(e) => {
                    ManuallyDrop::drop(&mut ob.pSample);
                    return Err(format!("ProcessOutput failed: {e:?}"));
                }
            }
        }
    }
    // Final drain storm (mirrors end-of-stream behavior).
    let mut storm_frames = 0usize;
    for _ in 0..50 {
        match drain_once(&pipe)? {
            true => {
                frames += 1;
                storm_frames += 1;
            }
            false => break,
        }
    }
    dbg_log(format!(
        "replay: {frames} total frames ({storm_frames} from final storm)"
    ));
    Ok((fed, frames))
}

/// File-backed variant of [`mf_replay_bytes`].
pub fn mf_file_replay(path: &str) -> Result<(usize, usize), String> {
    let data = std::fs::read(path).map_err(|e| format!("read {path} failed: {e}"))?;
    mf_replay_bytes(&data)
}

/// Crop codec-padded NV12 (e.g. 1920x1088) down to display dims (1920x1080).
/// Assumes contiguous rows of width w.
fn crop_nv12_padded(src: &[u8], w: usize, coded_h: usize, h: usize) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || coded_h <= h || src.len() != w * coded_h * 3 / 2 {
        return None;
    }
    let mut out = vec![0u8; w * h * 3 / 2];
    out[..w * h].copy_from_slice(&src[..w * h]);
    out[w * h..].copy_from_slice(&src[w * coded_h..w * coded_h + w * h / 2]);
    Some(out)
}

/// Standalone Video Processor MFT loopback used by tests: scale a synthetic
/// 64x64 NV12 gradient down to 32x32 NV12 and return the raw bytes.
pub fn mf_selftest_processor() -> Result<Vec<u8>, String> {
    mf_ensure_started()?;
    com_ensure_init();
    let proc = MftPipe::create_processor()?;
    let in_mt = new_video_type(&MFVideoFormat_NV12, 64, 64, None)?;
    proc.set_input(&in_mt)?;
    let out_mt = new_video_type(&MFVideoFormat_NV12, 32, 32, None)?;
    unsafe {
        proc.mft
            .SetOutputType(0, &out_mt, 0)
            .map_err(|e| format!("processor SetOutputType failed: {e:?}"))?;
    }
    // Synthetic gradient: Y ramps with x+y, UV neutral.
    let mut src = vec![0u8; 64 * 64 * 3 / 2];
    for y in 0..64usize {
        for x in 0..64usize {
            src[y * 64 + x] = ((x + y) & 0xFF) as u8;
        }
    }
    for b in src[64 * 64..].iter_mut() {
        *b = 128;
    }
    let in_sample = make_2d_nv12_sample(64, 64)?;
    fill_2d_nv12_from_contiguous(&in_sample, 64, 64, &src)?;
    unsafe {
        in_sample
            .SetSampleTime(0)
            .map_err(|e| format!("SetSampleTime failed: {e:?}"))?;
        in_sample
            .SetSampleDuration(333_333)
            .map_err(|e| format!("SetSampleDuration failed: {e:?}"))?;
    }
    proc.feed(&in_sample)?;
    let out_sample = make_2d_nv12_sample(32, 32)?;
    for _ in 0..50 {
        match proc.drain_into(&out_sample)? {
            true => {
                let mut out = Vec::new();
                copy_2d_nv12_to_vec(&out_sample, 32, 32, &mut out)?;
                return Ok(out);
            }
            false => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    Err("processor produced no output".into())
}

/// Time N rescale+convert operations (NV12 src -> NV12 dst) through the video
/// processor MFT. Returns (iters, avg_ms_per_frame, effective GB/s).
pub fn mf_processor_throughput(
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    iters: usize,
) -> Result<(usize, f64, f64), String> {
    mf_ensure_started()?;
    com_ensure_init();
    let proc = MftPipe::create_processor()?;
    let in_mt = new_video_type(&MFVideoFormat_NV12, src_w, src_h, None)?;
    proc.set_input(&in_mt)?;
    let out_mt = new_video_type(&MFVideoFormat_NV12, dst_w, dst_h, None)?;
    unsafe {
        proc.mft
            .SetOutputType(0, &out_mt, 0)
            .map_err(|e| format!("processor SetOutputType failed: {e:?}"))?;
    }
    let mut src = vec![0u8; src_w as usize * src_h as usize * 3 / 2];
    for y in 0..src_h as usize {
        for x in 0..src_w as usize {
            src[y * src_w as usize + x] = ((x * 3 + y * 5) & 0xFF) as u8;
        }
    }
    for b in src[src_w as usize * src_h as usize..].iter_mut() {
        *b = 128;
    }
    let mut total = 0f64;
    let mut out = Vec::new();
    let mut ts = 0i64;
    for _ in 0..iters {
        let in_sample = make_2d_nv12_sample(src_w, src_h)?;
        fill_2d_nv12_from_contiguous(&in_sample, src_w, src_h, &src)?;
        ts += TS_STEP_100NS;
        unsafe {
            in_sample
                .SetSampleTime(ts)
                .map_err(|e| format!("SetSampleTime failed: {e:?}"))?;
            in_sample
                .SetSampleDuration(TS_STEP_100NS)
                .map_err(|e| format!("SetSampleDuration failed: {e:?}"))?;
        }
        let t0 = std::time::Instant::now();
        proc.feed(&in_sample)?;
        let out_sample = make_2d_nv12_sample(dst_w, dst_h)?;
        let mut got = false;
        for _ in 0..20 {
            match proc.drain_into(&out_sample)? {
                true => {
                    got = true;
                    break;
                }
                false => std::thread::sleep(std::time::Duration::from_micros(200)),
            }
        }
        if !got {
            return Err("processor produced no output during throughput test".into());
        }
        total += t0.elapsed().as_secs_f64() * 1000.0;
        copy_2d_nv12_to_vec(&out_sample, dst_w, dst_h, &mut out)?;
    }
    let avg = total / iters as f64;
    let bytes_per_frame = (src_w as f64 * src_h as f64 * 1.5) + (dst_w as f64 * dst_h as f64 * 1.5);
    let gbps = bytes_per_frame / (avg / 1000.0) / 1e9;
    Ok((iters, avg, gbps))
}

/// Hardware (Media Foundation MFT) RTSP decoder with the same interface as the
/// software decoder, so the stream worker can swap it in transparently.
/// NOTE: experimental — the decoder MFT stays silent on the live NALU feed
/// path (see test_mf_debug.rs investigation); production uses software decode
/// with MF_ENABLE_HW=1 opting into this path.
pub struct MfRtspDecoder {
    codec: RtspCodec,
    decoder: Option<MftPipe>,
    processor: Option<MftPipe>,
    proc_staging: Vec<u8>,
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    header_set: bool,
    out_subtype: GUID,
    out_w: u32,
    out_h: u32,
    /// cbSize from GetOutputStreamInfo (may differ from NV12 math; size drains to it).
    out_cbsize: u32,
    /// MF_MT_SAMPLE_SIZE from the current output type: the EXACT output buffer
    /// size the MFT wants (CopyDecodedFrame fails otherwise). Refreshed on
    /// stream-change. A cached sample is rebuilt when it changes.
    out_sample_size: u32,
    out_sample: Option<IMFSample>,
    proc_in_w: u32,
    proc_in_h: u32,
    proc_out_w: u32,
    proc_out_h: u32,
    ts: i64,
    fed_total: u64,
    /// True once the stored parameter sets have been fed inline on the current
    /// pipeline. The MS decoder requires SPS/PPS IN THE BYTE STREAM (it skips
    /// bytes until found) and ignores the header blob for this purpose.
    params_fed: bool,
    avcc_scratch: Vec<u8>,
    /// Max-size reusable drain sample. The decoder only publishes real output dims
    /// via STREAM_CHANGE / first frame, so we drain into a 4K-capable buffer until
    /// the true size is known (avoids a size-known-before-first-frame deadlock).
    drain_sample: Option<IMFSample>,
    /// Lazily created D3D11 video device + DXGI manager (MF_D3D experiment).
    d3d: Option<D3DState>,
}

const DRAIN_MAX_W: u32 = 3840;
const DRAIN_MAX_H: u32 = 2160;

impl MfRtspDecoder {
    pub fn new(codec: RtspCodec) -> Result<Self, String> {
        mf_ensure_started()?;
        com_ensure_init();
        // Probe: fail fast here so the caller can fall back to software decode.
        let _probe = MftPipe::create_decoder(codec)?;
        Ok(Self {
            codec,
            decoder: None,
            processor: None,
            proc_staging: Vec::new(),
            vps: Vec::new(),
            sps: Vec::new(),
            pps: Vec::new(),
            header_set: false,
            out_subtype: GUID::zeroed(),
            out_w: 0,
            out_h: 0,
            out_cbsize: 0,
            out_sample_size: 0,
            out_sample: None,
            proc_in_w: 0,
            proc_in_h: 0,
            proc_out_w: 0,
            proc_out_h: 0,
            ts: 0,
            fed_total: 0,
            params_fed: false,
            avcc_scratch: Vec::new(),
            drain_sample: None,
            d3d: None,
        })
    }

    #[allow(dead_code)]
    pub fn codec(&self) -> RtspCodec {
        self.codec
    }

    /// Pre-seed parameter sets (e.g. from the SDP) before the first slice arrives.
    pub fn prime(&mut self, sps_pps: &[Vec<u8>]) {
        for nalu in sps_pps {
            self.ingest_params(nalu);
        }
    }

    /// Pre-seed from an AVCCDecoderConfigurationRecord (e.g. an encoder MFT's
    /// MF_MT_MPEG_SEQUENCE_HEADER output blob).
    pub fn prime_avcc_blob(&mut self, blob: &[u8]) {
        if blob.len() < 7 || blob[0] != 0x01 {
            return;
        }
        let mut off = 5usize;
        let nsps = (blob[off] & 0x1F) as usize;
        off += 1;
        for _ in 0..nsps {
            if off + 2 > blob.len() {
                return;
            }
            let len = u16::from_be_bytes([blob[off], blob[off + 1]]) as usize;
            off += 2;
            if off + len > blob.len() {
                return;
            }
            let mut nalu = vec![0x00, 0x00, 0x00, 0x01];
            nalu.extend_from_slice(&blob[off..off + len]);
            off += len;
            self.ingest_params(&nalu);
        }
        if off >= blob.len() {
            return;
        }
        let npps = blob[off] as usize;
        off += 1;
        for _ in 0..npps {
            if off + 2 > blob.len() {
                return;
            }
            let len = u16::from_be_bytes([blob[off], blob[off + 1]]) as usize;
            off += 2;
            if off + len > blob.len() {
                return;
            }
            let mut nalu = vec![0x00, 0x00, 0x00, 0x01];
            nalu.extend_from_slice(&blob[off..off + len]);
            off += len;
            self.ingest_params(&nalu);
        }
    }

    fn ingest_params(&mut self, nalu: &[u8]) -> bool {
        let off = match annexb_offset(nalu) {
            Some(o) if o < nalu.len() => o,
            _ => return false,
        };
        let body = &nalu[off..];
        if body.is_empty() {
            return false;
        }
        let changed = match self.codec {
            RtspCodec::H264 => {
                let t = body[0] & 0x1F;
                if t == 7 && self.sps.as_slice() != body {
                    self.sps = body.to_vec();
                    true
                } else if t == 8 && self.pps.as_slice() != body {
                    self.pps = body.to_vec();
                    true
                } else {
                    false
                }
            }
            RtspCodec::H265 => {
                let t = (body[0] >> 1) & 0x3F;
                if t == 32 && self.vps.as_slice() != body {
                    self.vps = body.to_vec();
                    true
                } else if t == 33 && self.sps.as_slice() != body {
                    self.sps = body.to_vec();
                    true
                } else if t == 34 && self.pps.as_slice() != body {
                    self.pps = body.to_vec();
                    true
                } else {
                    false
                }
            }
        };
        if changed {
            // New parameter sets (e.g. resolution change) invalidate the pipeline.
            self.header_set = false;
            self.decoder = None;
            self.processor = None;
            self.params_fed = false;
        }
        changed
    }

    fn params_complete(&self) -> bool {
        match self.codec {
            RtspCodec::H264 => !self.sps.is_empty() && !self.pps.is_empty(),
            RtspCodec::H265 => !self.vps.is_empty() && !self.sps.is_empty() && !self.pps.is_empty(),
        }
    }

    /// Feed one payload (AVCC or Annex-B per mode handling by caller) as a sample.
    fn feed_bytes(&mut self, payload: &[u8]) -> Result<(), String> {
        self.ts += TS_STEP_100NS;
        let sample = make_input_sample(payload, self.ts)?;
        self.fed_total += 1;
        let dec = self.decoder.as_ref().ok_or("no decoder")?;
        match dec.feed(&sample) {
            Ok(()) => Ok(()),
            Err(e) if e == "not-accepting" => {
                let _ = self.drain_decoder()?;
                let dec = self.decoder.as_ref().ok_or("no decoder")?;
                match dec.feed(&sample) {
                    Ok(()) => Ok(()),
                    Err(e2) => {
                        if std::env::var("MF_NO_GATE").is_ok() {
                            dbg_log(format!("dropping payload after retry ({e2}; first: {e})"));
                            Ok(())
                        } else {
                            Err(e2)
                        }
                    }
                }
            }
            Err(e) => {
                if std::env::var("MF_NO_GATE").is_ok() {
                    dbg_log(format!("dropping payload on feed error: {e}"));
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Feed stored parameter sets inline (Annex-B on the wire or AVCC per mode).
    /// Must precede the first slice after every pipeline (re)build.
    fn feed_stored_params(&mut self, annexb_mode: bool) -> Result<(), String> {
        if self.params_fed || !self.params_complete() {
            dbg_log(format!(
                "stored-params skipped (fed={} complete={})",
                self.params_fed,
                self.params_complete()
            ));
            return Ok(());
        }
        dbg_log("feeding stored params inline".to_string());
        let sets: Vec<Vec<u8>> = match self.codec {
            RtspCodec::H264 => vec![self.sps.clone(), self.pps.clone()],
            RtspCodec::H265 => vec![self.vps.clone(), self.sps.clone(), self.pps.clone()],
        };
        for ps in &sets {
            if ps.is_empty() {
                continue;
            }
            if annexb_mode {
                let p = ps.clone();
                self.feed_bytes(&p)?;
            } else {
                let mut avcc = Vec::new();
                if to_avcc(ps, &mut avcc) {
                    self.feed_bytes(&avcc)?;
                }
            }
        }
        self.params_fed = true;
        Ok(())
    }

    fn header_blob(&self) -> Option<Vec<u8>> {
        // The phone encoder declares Level 1.0 in SPS even for 1080p+ streams (see ACodec
        // "level: Level1" in device logs). Strict decoders size the DPB from that level and
        // silently drop everything. MF_PATCH_LEVEL=1 rewrites the level to 5.1 (plenty for
        // 4K30) in our header copy only — it changes capability metadata, not bitstream parsing.
        let patch = std::env::var("MF_PATCH_LEVEL").is_ok();
        match self.codec {
            RtspCodec::H264 => {
                let sps = self.sps.clone();
                let sps = if patch && sps.len() > 3 {
                    let mut s = sps;
                    s[3] = 0x33;
                    s
                } else {
                    sps
                };
                build_avcc(&sps, &self.pps)
            }
            RtspCodec::H265 => {
                let sps = self.sps.clone();
                let sps = if patch && sps.len() > 13 {
                    let mut s = sps;
                    s[13] = 0x99; // general_level_idc 5.1
                    s
                } else {
                    sps
                };
                build_hvcc(&self.vps, &sps, &self.pps)
            }
        }
    }

    fn subtype(&self) -> &'static GUID {
        match self.codec {
            RtspCodec::H264 => &MFVideoFormat_H264,
            RtspCodec::H265 => &MFVideoFormat_HEVC,
        }
    }

    /// MF_CODECAPI=1 experiment: low-latency mode + H264 DXVA acceleration via
    /// ICodecAPI (mirrors a known-working configuration for this MFT).
    fn apply_codecapi(&self) {
        if std::env::var("MF_CODECAPI").is_err() {
            return;
        }
        let dec = match self.decoder.as_ref() {
            Some(d) => d,
            None => return,
        };
        unsafe {
            let api: ICodecAPI = match dec.mft.cast() {
                Ok(a) => a,
                Err(e) => {
                    dbg_log(format!("ICodecAPI not supported: {e:?}"));
                    return;
                }
            };
            for (name, guid) in [
                ("AVLowLatencyMode", &CODECAPI_AVLowLatencyMode),
                ("AVDecVideoAcceleration_H264", &CODECAPI_AVDecVideoAcceleration_H264),
            ] {
                let mut v = VARIANT::default();
                unsafe {
                    (*v.Anonymous.Anonymous).vt = VT_UI4;
                    (*v.Anonymous.Anonymous).Anonymous.ulVal = 1;
                }
                match api.SetValue(guid, &v) {
                    Ok(()) => dbg_log(format!("CODECAPI {name}=1 ok")),
                    Err(e) => dbg_log(format!("CODECAPI {name} failed: {e:?}")),
                }
            }
        }
    }

    fn ensure_pipeline(&mut self) -> Result<(), String> {
        if self.header_set && self.decoder.is_some() {
            return Ok(());
        }
        // MF_ANNEXB=1 experiment: no sequence header, feed raw Annex-B instead.
        let annexb_mode = std::env::var("MF_ANNEXB").is_ok();
        let blob = if annexb_mode {
            None
        } else {
            Some(self.header_blob().ok_or("waiting for SPS/PPS")?)
        };
        let pipe = MftPipe::create_decoder(self.codec)?;
        // Seed MF_MT_SAMPLE_SIZE early when the offered type carries it.
        // (Set again later from the current type via adopt_output_dims.)
        // MF_D3D=1 experiment: give the MFT a DXVA device manager. Without it the
        // decoder may silently refuse to emit (observed: accepts input, no output).
        if std::env::var("MF_D3D").is_ok() {
            if self.d3d.is_none() {
                match create_d3d_state() {
                    Ok(st) => {
                        dbg_log("D3D11 video device created".to_string());
                        self.d3d = Some(st);
                    }
                    Err(e) => dbg_log(format!("D3D device creation failed: {e}")),
                }
            }
            if let Some(st) = self.d3d.as_ref() {
                match attach_d3d_manager(&pipe.mft, st) {
                    Ok(()) => dbg_log("D3D manager attached to decoder".to_string()),
                    Err(e) => dbg_log(format!("D3D manager attach failed: {e}")),
                }
            }
        }
        let in_mt = new_video_type(self.subtype(), 0, 0, blob.as_deref())?;
        pipe.set_input(&in_mt)?;
        let mut out_mt = pipe.set_output_prefer_nv12()?;
        if std::env::var("MF_EXP_ATTRS").is_ok() {
            // Phase 2: re-set a FULL input type now that dims are known.
            if let Ok((w, h)) = frame_size_of(&out_mt) {
                if w > 0 && h > 0 {
                    let in_full =
                        new_video_type(self.subtype(), 0, 0, blob.as_deref())?;
                    enrich_input_type(&in_full, w, h)?;
                    unsafe {
                        let _ = pipe.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                    }
                    pipe.set_input(&in_full)?;
                    out_mt = pipe.set_output_prefer_nv12()?;
                    dbg_log("phase-2 full input type set".to_string());
                }
            }
        }
        let (w, h) = frame_size_of(&out_mt).unwrap_or((0, 0));
        let st = unsafe {
            out_mt
                .GetGUID(&MF_MT_SUBTYPE)
                .map_err(|e| format!("output subtype missing: {e:?}"))?
        };
        // Seed the exact output sample size early (may be padded, e.g. 1920x1088:
        // CopyDecodedFrame fails on wrong-sized buffers).
        if let Ok(sz) = unsafe { out_mt.GetUINT32(&MF_MT_SAMPLE_SIZE) } {
            if sz > 0 {
                self.out_sample_size = sz;
                self.out_sample = None;
                dbg_log(format!("seeded output sample_size={sz}"));
            }
        }
        self.out_subtype = st;
        self.out_w = w;
        self.out_h = h;
        self.decoder = Some(pipe);
        self.processor = None;
        self.header_set = true;
        self.fed_total = 0;
        // Fresh pipeline: stored params must be (re)fed inline before slices.
        self.params_fed = false;
        if let Some(dec) = self.decoder.as_ref() {
            unsafe {
                if let Ok(oi) = dec.mft.GetOutputStreamInfo(0) {
                    self.out_cbsize = oi.cbSize;
                    dbg_log(format!("decoder out_cbsize={}", oi.cbSize));
                }
            }
        }
        if let Some(dec) = self.decoder.as_ref() {
            dec.log_stream_info();
            unsafe {
                let mut icount = 0u32;
                let mut ocount = 0u32;
                if dec.mft.GetStreamCount(&mut icount, &mut ocount).is_ok() {
                    dbg_log(format!("stream counts: in={icount} out={ocount}"));
                }
            }
        }
        self.apply_codecapi();
        // Probe async behavior: if the MFT exposes IMFMediaEventGenerator it must
        // be pumped; log once for diagnostics.
        if let Some(dec) = self.decoder.as_ref() {
            unsafe {
                use windows::Win32::Media::MediaFoundation::IMFMediaEventGenerator as EvGen;
                match dec.mft.cast::<EvGen>() {
                    Ok(_) => dbg_log("decoder exposes IMFMediaEventGenerator (ASYNC)".to_string()),
                    Err(_) => dbg_log("decoder is synchronous (no event generator)".to_string()),
                }
            }
        }
        dbg_log(format!(
            "pipeline ready: codec={:?} header={}B {:02X?} out={}x{} subtype={:?}",
            self.codec,
            blob.as_ref().map(|b| b.len()).unwrap_or(0),
            blob.as_ref().map(|b| b[..b.len().min(24)].to_vec()).unwrap_or_default(),
            w,
            h,
            st
        ));
        if std::env::var("MF_BEGIN_STREAMING").is_ok() {
            if let Some(dec) = self.decoder.as_ref() {
                unsafe {
                    let r = dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
                    dbg_log(format!("NOTIFY_BEGIN_STREAMING -> {r:?}"));
                    let r2 = dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
                    dbg_log(format!("NOTIFY_START_OF_STREAM -> {r2:?}"));
                }
            }
        }
        Ok(())
    }

    fn ensure_processor(&mut self, dst_w: u32, dst_h: u32) -> Result<(), String> {
        if self.processor.is_some()
            && self.proc_in_w == self.out_w
            && self.proc_in_h == self.out_h
            && self.proc_out_w == dst_w
            && self.proc_out_h == dst_h
        {
            return Ok(());
        }
        let proc = MftPipe::create_processor()?;
        let in_mt = new_video_type(&self.out_subtype, self.out_w, self.out_h, None)?;
        proc.set_input(&in_mt)?;
        let out_mt = new_video_type(&MFVideoFormat_NV12, dst_w, dst_h, None)?;
        unsafe {
            proc.mft
                .SetOutputType(0, &out_mt, 0)
                .map_err(|e| format!("processor SetOutputType failed: {e:?}"))?;
        }
        self.processor = Some(proc);
        self.proc_in_w = self.out_w;
        self.proc_in_h = self.out_h;
        self.proc_out_w = dst_w;
        self.proc_out_h = dst_h;
        Ok(())
    }

    fn is_param_set(nalu_body: &[u8], codec: RtspCodec) -> bool {
        if nalu_body.is_empty() {
            return false;
        }
        match codec {
            RtspCodec::H264 => matches!(nalu_body[0] & 0x1F, 7 | 8),
            RtspCodec::H265 => matches!((nalu_body[0] >> 1) & 0x3F, 32 | 33 | 34),
        }
    }

    fn is_slice(nalu_body: &[u8], codec: RtspCodec) -> bool {
        if nalu_body.is_empty() {
            return false;
        }
        match codec {
            RtspCodec::H264 => {
                let t = nalu_body[0] & 0x1F;
                // Slices (1,5 + partitions), exclude AUD(9)/SEI(6)/EOSeq(10)/EOStream(11)/filler(12)/SPS/PPS.
                t == 1 || t == 5 || t == 19 || t == 20 || t == 21 || t == 2 || t == 3 || t == 4
            }
            RtspCodec::H265 => {
                let t = (nalu_body[0] >> 1) & 0x3F;
                // Trail/RASL/RADL/IDR/CRA/BLA slices; exclude VPS/SPS/PPS/AUD(35)/SEI(39,40).
                t <= 21
            }
        }
    }

    fn adopt_output_dims(&mut self) {
        if let Some(dec) = self.decoder.as_ref() {
            if let Ok(mt) = unsafe { dec.mft.GetOutputCurrentType(0) } {
                if let Ok((w, h)) = frame_size_of(&mt) {
                    if w > 0 && h > 0 {
                        self.out_w = w;
                        self.out_h = h;
                    }
                }
                if let Ok(sz) = unsafe { mt.GetUINT32(&MF_MT_SAMPLE_SIZE) } {
                    if sz > 0 && sz != self.out_sample_size {
                        self.out_sample_size = sz;
                        self.out_sample = None;
                        dbg_log(format!("adopted output sample_size={sz}"));
                    }
                }
            }
        }
    }

    /// Build (or reuse) the output sample. Size = max(MF_MT_SAMPLE_SIZE,
    /// macroblock-padded NV12): H.264 codes height to multiples of 16 (e.g. the
    /// offered type says 3110400 for 1080p while the MFT really wants 3133440 =
    /// 1920x1088). Wrong-sized output buffers keep the MFT mute.
    fn output_sample(&mut self) -> Result<IMFSample, String> {
        let padded = if self.out_w > 0 && self.out_h > 0 {
            let coded_h = (self.out_h + 15) & !15;
            (self.out_w * coded_h * 3 / 2) as usize
        } else {
            0
        };
        let want = (self.out_sample_size as usize)
            .max(padded)
            .max(if padded == 0 {
                (DRAIN_MAX_W * DRAIN_MAX_H * 3 / 2) as usize
            } else {
                0
            });
        // MF_FRESH_OUT=1: never reuse an output sample across drains (the MFT
        // may hold submitted samples; resubmission can wedge it mute).
        let same = if std::env::var("MF_FRESH_OUT").is_ok() {
            false
        } else {
            match self.out_sample.as_ref() {
                Some(s) => unsafe {
                    s.GetBufferByIndex(0)
                        .and_then(|b| b.GetMaxLength())
                        .map(|m| m as usize == want)
                        .unwrap_or(false)
                },
                None => false,
            }
        };
        if !same {
            unsafe {
                let buf = MFCreateMemoryBuffer(want as u32)
                    .map_err(|e| format!("out buffer failed: {e:?}"))?;
                let s = MFCreateSample().map_err(|e| format!("sample failed: {e:?}"))?;
                s.AddBuffer(&buf)
                    .map_err(|e| format!("AddBuffer failed: {e:?}"))?;
                self.out_sample = Some(s);
            }
        }
        Ok(self.out_sample.as_ref().unwrap().clone())
    }

    /// MF_1DOUT experiment: drain into a 1D buffer sized exactly to the MFT's
    /// cbSize (falls back to NV12 math, then max). Some MFT builds appear
    /// sensitive to output buffer sizing.
    fn drain_decoder(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.decoder.is_none() {
            return Err("no decoder".into());
        }
        // Output sample sized exactly to MF_MT_SAMPLE_SIZE (CopyDecodedFrame
        // fails otherwise — observed as E_FAIL on undersized/missing buffers).
        let sample = self.output_sample()?;
        let dec = self.decoder.as_ref().ok_or("no decoder")?;
        match dec.drain_into(&sample) {
            Ok(false) => Ok(None),
            Ok(true) => {
                // Adopt real dims/sample-size from the current output type.
                self.adopt_output_dims();
                let raw = unsafe {
                    let buf = sample
                        .GetBufferByIndex(0)
                        .map_err(|e| format!("GetBufferByIndex failed: {e:?}"))?;
                    let mut ptr: *mut u8 = std::ptr::null_mut();
                    let mut maxlen = 0u32;
                    let mut curlen = 0u32;
                    buf.Lock(&mut ptr, Some(&mut maxlen), Some(&mut curlen))
                        .map_err(|e| format!("out lock failed: {e:?}"))?;
                    let n = curlen as usize;
                    let v = std::slice::from_raw_parts(ptr, n).to_vec();
                    let _ = buf.Unlock();
                    v
                };
                Ok(Some(raw))
            }
            Err(e) if e == "stream-change" => {
                let mt = dec.refresh_output_type()?;
                if let Ok((w, h)) = frame_size_of(&mt) {
                    if w > 0 && h > 0 {
                        self.out_w = w;
                        self.out_h = h;
                    }
                }
                self.adopt_output_dims();
                self.processor = None;
                Ok(None)
            }
            Err(e) => {
                // Recovery: adopt fresh type info (dims + sample size); the next
                // drain rebuilds the output sample accordingly.
                self.adopt_output_dims();
                self.out_sample = None;
                dbg_log(format!(
                    "drain recovered after {e}; adopted size={}",
                    self.out_sample_size
                ));
                Ok(None)
            }
        }
    }

    #[allow(dead_code)]
    pub fn decode_nalu_ignore(&mut self, nalu: &[u8]) {
        self.ingest_params(nalu);
    }

    pub fn decode_into_target(
        &mut self,
        nalu: &[u8],
        target_w: u32,
        target_h: u32,
        out_nv12: &mut Vec<u8>,
        out_rgba: &mut Vec<u8>,
        skip_preview: bool,
    ) -> Result<Option<DecodeResult>, String> {
        let off = match annexb_offset(nalu) {
            Some(o) if o < nalu.len() => o,
            _ => return Ok(None),
        };
        let body = &nalu[off..];
        // MSDN: the decoder "skips bytes until it finds a valid SPS and PPS IN THE
        // BYTE STREAM" — the header blob alone does not suffice, so in-band
        // parameter sets are FED (not skipped) and stored sets are pre-fed
        // before the first slice of every pipeline.
        let annexb_mode = std::env::var("MF_ANNEXB").is_ok();
        self.ingest_params(nalu);
        let is_param = Self::is_param_set(body, self.codec);
        let is_sl = Self::is_slice(body, self.codec);
        if !is_param && !is_sl && !annexb_mode {
            return Ok(None);
        }
        // In AVCC mode the header blob needs complete params; wait quietly.
        if !annexb_mode && !self.params_complete() {
            return Ok(None);
        }
        self.ensure_pipeline()?;
        self.feed_stored_params(annexb_mode)?;

        self.avcc_scratch.clear();
        if std::env::var("MF_ANNEXB").is_ok() {
            self.avcc_scratch.extend_from_slice(nalu);
        } else if !to_avcc(nalu, &mut self.avcc_scratch) {
            return Ok(None);
        }
        // MF_AUD=1 experiment: prefix every fed slice with an AUD so the MFT
        // sees explicit access-unit boundaries (some builds never emit without them).
        if std::env::var("MF_AUD").is_ok() {
            let mut prefixed = if std::env::var("MF_ANNEXB").is_ok() {
                vec![0x00, 0x00, 0x00, 0x01, 0x09, 0xF0]
            } else {
                vec![0x00, 0x00, 0x00, 0x02, 0x09, 0xF0]
            };
            prefixed.extend_from_slice(&self.avcc_scratch);
            self.avcc_scratch = prefixed;
        }
        // MF_CHUNK=N experiment: split the payload into N-byte input samples
        // (mirrors the working reference, which feeds 1500B file chunks — the
        // MFT advertises FIXED_SAMPLE_SIZE with cbSize=4096 on its input).
        let chunk_size: usize = std::env::var("MF_CHUNK")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if chunk_size > 0 {
            let payload = self.avcc_scratch.clone();
            let mut off = 0usize;
            while off < payload.len() {
                let end = (off + chunk_size).min(payload.len());
                self.ts += TS_STEP_100NS / 10;
                let sample = make_input_sample(&payload[off..end], self.ts)?;
                self.fed_total += 1;
                let dec = self.decoder.as_ref().ok_or("no decoder")?;
                match dec.feed(&sample) {
                    Ok(()) => {}
                    Err(e) if e == "not-accepting" => {
                        let _ = self.drain_decoder()?;
                        let dec = self.decoder.as_ref().ok_or("no decoder")?;
                        dec.feed(&sample)?;
                    }
                    Err(e) => return Err(e),
                }
                off = end;
                if let Some(r) = self.drain_decoder()? {
                    return self.finish_frame(
                        r,
                        target_w,
                        target_h,
                        out_nv12,
                        out_rgba,
                        skip_preview,
                    );
                }
            }
            return Ok(None);
        }

        self.ts += TS_STEP_100NS;
        let sample = make_input_sample(&self.avcc_scratch, self.ts)?;
        // MF_DISCO=1 experiment: mark the very first sample discontinuous.
        if std::env::var("MF_DISCO").is_ok() && self.fed_total == 0 {
            let _ = unsafe { sample.SetUINT32(&MFSampleExtension_Discontinuity, 1) };
        }
        self.fed_total += 1;
        let dec = self.decoder.as_ref().ok_or("no decoder")?;
        match dec.feed(&sample) {
            Ok(()) => {
                static FED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = FED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < 8 || n % 100 == 0 {
                    dbg_log(format!(
                        "fed NALU #{n} h264type={} len={} head={:02X?} ts={}",
                        body[0] & 0x1F,
                        self.avcc_scratch.len(),
                        self.avcc_scratch[..self.avcc_scratch.len().min(12)].to_vec(),
                        self.ts
                    ));
                }
            }
            Err(e) if e == "not-accepting" => {
                // Decoder full: pull a frame, then feed again.
                let _ = self.drain_decoder()?;
                let dec = self.decoder.as_ref().ok_or("no decoder")?;
                if let Err(e2) = dec.feed(&sample) {
                    // MF_NO_GATE mode mirrors the reference: tolerate and drop.
                    if std::env::var("MF_NO_GATE").is_ok() {
                        dbg_log(format!("dropping NALU after retry ({e2}; first: {e})"));
                        return Ok(None);
                    }
                    return Err(e2);
                }
            }
            Err(e) => {
                if std::env::var("MF_NO_GATE").is_ok() {
                    dbg_log(format!("dropping NALU on feed error: {e}"));
                    return Ok(None);
                }
                return Err(e);
            }
        }

        static DRAINED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let raw = match self.drain_decoder()? {
            Some(r) => r,
            None => {
                let n = DRAINED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < 5 || n % 200 == 0 {
                    // Also sample GetOutputStatus to see if the MFT claims output readiness.
                    let ost = self
                        .decoder
                        .as_ref()
                        .and_then(|d| unsafe { d.mft.GetOutputStatus().ok() })
                        .unwrap_or(0);
                    dbg_log(format!("drain #{n}: need-more-input (output_status={ost})"));
                }
                // MF_DRAIN_PROBE: periodically ask the MFT to emit everything buffered.
                // Distinguishes "can't parse input" (still nothing) from "buffering policy".
                if std::env::var("MF_DRAIN_PROBE").is_ok() && n > 0 && n % 120 == 0 {
                    if let Some(dec) = self.decoder.as_ref() {
                        unsafe {
                            let r = dec.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
                            dbg_log(format!("COMMAND_DRAIN -> {r:?}"));
                        }
                        for _ in 0..10 {
                            match self.drain_decoder() {
                                Ok(Some(r)) => {
                                    dbg_log(format!("DRAIN-PROBE yielded {}B", r.len()));
                                    return self.finish_frame(r, target_w, target_h, out_nv12, out_rgba, skip_preview);
                                }
                                _ => {}
                            }
                        }
                        dbg_log("DRAIN-PROBE yielded nothing".to_string());
                    }
                }
                return Ok(None);
            }
        };

        self.finish_frame(raw, target_w, target_h, out_nv12, out_rgba, skip_preview)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_frame(
        &mut self,
        raw: Vec<u8>,
        target_w: u32,
        target_h: u32,
        out_nv12: &mut Vec<u8>,
        out_rgba: &mut Vec<u8>,
        skip_preview: bool,
    ) -> Result<Option<DecodeResult>, String> {
        let (src_w, src_h) = (self.out_w, self.out_h);
        if src_w == 0 || src_h == 0 {
            return Ok(None);
        }

        // Codec-native padding crop (e.g. MFT emits 1920x1088 for a 1920x1080
        // stream): keep only the display rows before converting/rescaling.
        let cropped: Vec<u8>;
        let frame: &[u8] = {
            let expect = (src_w * src_h * 3 / 2) as usize;
            if raw.len() != expect && src_w > 0 {
                let coded_h = raw.len() * 2 / (src_w as usize * 3);
                match crop_nv12_padded(&raw, src_w as usize, coded_h, src_h as usize) {
                    Some(c) => {
                        cropped = c;
                        &cropped
                    }
                    None => &raw,
                }
            } else {
                &raw
            }
        };

        if src_w == target_w && src_h == target_h {
            let expect = (target_w * target_h * 3 / 2) as usize;
            if out_nv12.len() != expect {
                out_nv12.resize(expect, 0);
            }
            let n = frame.len().min(expect);
            out_nv12[..n].copy_from_slice(&frame[..n]);
        } else {
            self.ensure_processor(target_w, target_h)?;
            let proc = self.processor.as_ref().ok_or("no processor")?;
            if self.proc_staging.len() != frame.len() {
                self.proc_staging.resize(frame.len(), 0);
            }
            self.proc_staging.copy_from_slice(frame);
            let in_sample = make_2d_nv12_sample(src_w, src_h)?;
            fill_2d_nv12_from_contiguous(&in_sample, src_w, src_h, &self.proc_staging)?;
            proc.feed(&in_sample)?;
            let out_sample = make_2d_nv12_sample(target_w, target_h)?;
            match proc.drain_into(&out_sample)? {
                true => copy_2d_nv12_to_vec(&out_sample, target_w, target_h, out_nv12)?,
                false => return Ok(None),
            }
        }

        let mut prev_w = 0u32;
        let mut prev_h = 0u32;
        if !skip_preview {
            let (pw, ph) =
                crate::stream::rtsp::calculate_preview_dimensions(src_w as usize, src_h as usize);
            let rgba_size = pw * ph * 4;
            if out_rgba.len() != rgba_size {
                out_rgba.resize(rgba_size, 0);
            }
            crate::stream::rtsp::nv12_to_sampled_rgba(
                out_nv12,
                target_w as usize,
                target_h as usize,
                out_rgba,
                pw,
                ph,
            );
            prev_w = pw as u32;
            prev_h = ph as u32;
        }

        Ok(Some(DecodeResult {
            src_w,
            src_h,
            prev_w,
            prev_h,
        }))
    }
}
