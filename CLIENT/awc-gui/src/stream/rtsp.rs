use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use crate::stream::pipeline::i420_to_nv12;

pub struct InProcessRtspDecoder {
    decoder: Decoder,
}

impl InProcessRtspDecoder {
    pub fn new() -> Result<Self, String> {
        let decoder = Decoder::new().map_err(|e| format!("Failed to create OpenH264 decoder: {}", e))?;
        Ok(Self { decoder })
    }

    /// Decodes NALU directly into reusable NV12 (for virtual camera) and RGBA8 (for UI preview).
    /// Zero heap allocations per frame.
    /// Returns Some((width, height)) if a new video frame was produced.
    pub fn decode_into(
        &mut self,
        nalu: &[u8],
        out_nv12: &mut Vec<u8>,
        out_rgba: &mut Vec<u8>,
    ) -> Result<Option<(u32, u32)>, String> {
        match self.decoder.decode(nalu) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();

                let nv12_size = w * h * 3 / 2;
                if out_nv12.len() != nv12_size {
                    out_nv12.resize(nv12_size, 0);
                }
                i420_to_nv12(&yuv, out_nv12);

                let rgba_size = w * h * 4;
                if out_rgba.len() != rgba_size {
                    out_rgba.resize(rgba_size, 0);
                }
                yuv.write_rgba8(out_rgba);

                Ok(Some((w as u32, h as u32)))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("OpenH264 decode error: {}", e)),
        }
    }

    pub fn decode_nalu_ignore(&mut self, nalu: &[u8]) {
        let _ = self.decoder.decode(nalu);
    }
}
