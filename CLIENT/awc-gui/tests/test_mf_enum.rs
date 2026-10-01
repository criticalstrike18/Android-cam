//! Enumerate registered Media Foundation video decoder MFTs (H264 + HEVC).
//! Run with: cargo test --test test_mf_enum -- --nocapture

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

fn list_decoders(subtype: &windows::core::GUID, label: &str) {
    unsafe {
        let info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: *subtype,
        };
        for (flags, tag) in [
            (
                MFT_ENUM_FLAG(
                    MFT_ENUM_FLAG_SYNCMFT.0
                        | MFT_ENUM_FLAG_ASYNCMFT.0
                        | MFT_ENUM_FLAG_HARDWARE.0,
                ),
                "all",
            ),
            (MFT_ENUM_FLAG_HARDWARE, "hardware-only"),
        ] {
            let mut ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            let hr = MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                flags,
                Some(&info),
                None,
                &mut ptr,
                &mut count,
            );
            println!("[enum] {label} [{tag}]: hr={hr:?} count={count}");
            if hr.is_ok() {
                for i in 0..count {
                    let slot = &*ptr.add(i as usize);
                    if let Some(act) = slot {
                        let mut pwsz = windows::core::PWSTR::null();
                        let mut len = 0u32;
                        let name = match act.GetAllocatedString(
                            &MFT_FRIENDLY_NAME_Attribute,
                            &mut pwsz,
                            &mut len,
                        ) {
                            Ok(()) => unsafe { pwsz.to_string().unwrap_or_default() },
                            Err(_) => "?".to_string(),
                        };
                        let clsid = act
                            .GetGUID(&MFT_TRANSFORM_CLSID_Attribute)
                            .map(|g| format!("{g:?}"))
                            .unwrap_or_else(|_| "?".into());
                        println!("[enum]   - {name} [{clsid}]");
                        let _ = act.ShutdownObject();
                    }
                }
                CoTaskMemFree(Some(ptr as _));
            }
        }
    }
}

#[test]
fn test_mf_enum_decoders() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).expect("MFStartup");
    }
    list_decoders(&MFVideoFormat_H264, "H264");
    list_decoders(&MFVideoFormat_HEVC, "HEVC");
    println!("[enum] done");
}
