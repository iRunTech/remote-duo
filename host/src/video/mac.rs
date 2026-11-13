//! macOS: ScreenCaptureKit delivers NV12 frames as CVPixelBuffers, which go
//! straight into a VideoToolbox hardware H.264 session (no copies).

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryContainsKey, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGDisplay;
use screencapturekit::prelude::*;
use tokio::sync::mpsc;

use super::{Control, Packet};

const FPS: i32 = 60;
const QUEUE: usize = 4;

pub fn start(max_width: u32, display_id: u32) -> anyhow::Result<(mpsc::Receiver<Packet>, Arc<Control>, Box<dyn Send>)> {
    let content = SCShareableContent::get().map_err(|e| {
        anyhow::anyhow!(
            "can't capture the screen ({e:?}). Grant Screen Recording permission to this app \
             in System Settings → Privacy & Security, then restart the host"
        )
    })?;
    let display = content
        .displays()
        .into_iter()
        .min_by_key(|d| d.display_id() != display_id)
        .ok_or_else(|| anyhow::anyhow!("no displays found"))?;

    // Capture at native pixels, scaled down to fit max_width. H.264 wants even sizes.
    // (CGDisplayPixelsWide reports points on Retina displays; the mode knows the real pixels.)
    let native = CGDisplay::new(display.display_id());
    let (pw, ph) = match (crate::fit::pixel_size(display.display_id()), native.display_mode()) {
        (Some((w, h)), _) => (w as f64, h as f64),
        (None, Some(m)) => (m.pixel_width() as f64, m.pixel_height() as f64),
        (None, None) => (native.pixels_wide() as f64, native.pixels_high() as f64),
    };
    let scale = (max_width as f64 / pw).min(1.0);
    let even = |v: f64| ((v * scale) as u32) & !1;
    let (width, height) = (even(pw), even(ph));

    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build()?;
    let config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::YCbCr_420v)
        .with_minimum_frame_interval(&CMTime::new(1, FPS))
        .with_shows_cursor(true)
        .with_queue_depth(5);

    let (tx, rx) = mpsc::channel(QUEUE);
    let ctl = Arc::new(Control::default());
    let encoder = Arc::new(Encoder::new(width, height, Sink {
        tx,
        ctl: ctl.clone(),
        need_key: AtomicBool::new(false),
    })?);

    let mut stream = SCStream::new(&filter, &config)?;
    let enc = encoder.clone();
    let epoch = Instant::now();
    stream.add_output_handler(
        move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // Idle frames (nothing changed on screen) carry no image.
            let pixels = sample.image_buffer_ptr_borrowed();
            if !pixels.is_null() {
                enc.encode(pixels, epoch.elapsed().as_micros() as i64);
            }
        },
        SCStreamOutputType::Screen,
    )?;
    stream.start_capture()?;
    println!("screen capture started at {width}x{height}");

    Ok((rx, ctl, Box::new(Running { stream: Some(stream), _encoder: encoder })))
}

struct Running {
    stream: Option<SCStream>,
    _encoder: Arc<Encoder>,
}

// SCStream is a handle to an Objective-C object that's safe to stop and
// release from any thread.
unsafe impl Send for Running {}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
        println!("screen capture stopped");
    }
}

// MARK: VideoToolbox

struct Sink {
    tx: mpsc::Sender<Packet>,
    ctl: Arc<Control>,
    /// Set after we drop a frame: deltas are useless until the next keyframe.
    need_key: AtomicBool,
}

impl Sink {
    fn push(&self, packet: Packet) -> bool {
        if self.tx.try_send(packet).is_ok() {
            return true;
        }
        // Network can't keep up: skip ahead to a fresh keyframe.
        self.need_key.store(true, Ordering::Relaxed);
        self.ctl.force_key.store(true, Ordering::Relaxed);
        false
    }
}

struct Encoder {
    session: VTSession,
    sink: *mut Sink,
}

// VTCompressionSession is thread-safe; the sink is only read by its callback.
unsafe impl Send for Encoder {}
unsafe impl Sync for Encoder {}

impl Encoder {
    fn new(width: u32, height: u32, sink: Sink) -> anyhow::Result<Self> {
        let sink = Box::into_raw(Box::new(sink));
        let spec = unsafe {
            CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kVTVideoEncoderSpecification_EnableLowLatencyRateControl),
                CFBoolean::true_value().as_CFType(),
            )])
        };
        let mut session: VTSession = ptr::null_mut();
        let status = unsafe {
            VTCompressionSessionCreate(
                ptr::null(),
                width as i32,
                height as i32,
                CODEC_H264,
                spec.as_concrete_TypeRef(),
                ptr::null(),
                ptr::null(),
                Some(on_encoded),
                sink.cast(),
                &mut session,
            )
        };
        if status != 0 || session.is_null() {
            drop(unsafe { Box::from_raw(sink) });
            anyhow::bail!("couldn't create H.264 encoder (OSStatus {status})");
        }
        let encoder = Self { session, sink };
        unsafe {
            encoder.set(kVTCompressionPropertyKey_RealTime, CFBoolean::true_value().as_CFTypeRef());
            encoder.set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::false_value().as_CFTypeRef());
            encoder.set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel.cast());
            let bitrate = CFNumber::from(bitrate_for(width, height));
            encoder.set(kVTCompressionPropertyKey_AverageBitRate, bitrate.as_CFTypeRef());
            let fps = CFNumber::from(FPS);
            encoder.set(kVTCompressionPropertyKey_ExpectedFrameRate, fps.as_CFTypeRef());
            let key_interval = CFNumber::from(2.0f64);
            encoder.set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, key_interval.as_CFTypeRef());
            VTCompressionSessionPrepareToEncodeFrames(session);
        }
        Ok(encoder)
    }

    unsafe fn set(&self, key: CFStringRef, value: CFTypeRef) {
        let status = unsafe { VTSessionSetProperty(self.session, key, value) };
        if status != 0 {
            let name = unsafe { CFString::wrap_under_get_rule(key) };
            eprintln!("encoder: couldn't set {name} (OSStatus {status})");
        }
    }

    fn encode(&self, pixels: *mut c_void, pts_us: i64) {
        let sink = unsafe { &*self.sink };
        let force = sink.ctl.force_key.swap(false, Ordering::Relaxed);
        let props = force.then(|| unsafe {
            CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kVTEncodeFrameOptionKey_ForceKeyFrame),
                CFBoolean::true_value().as_CFType(),
            )])
        });
        unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pixels,
                CMTimeRaw::micros(pts_us),
                CMTimeRaw::INVALID,
                props.as_ref().map_or(ptr::null(), |p| p.as_concrete_TypeRef()),
                ptr::null_mut(),
                ptr::null_mut(),
            );
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            // Flush so no callback can touch the sink after we free it.
            VTCompressionSessionCompleteFrames(self.session, CMTimeRaw::INVALID);
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session as CFTypeRef);
            drop(Box::from_raw(self.sink));
        }
    }
}

/// Roughly 10 Mbps at 1080p, scaled by pixel count.
fn bitrate_for(width: u32, height: u32) -> i32 {
    let pixels = (width * height) as f64;
    (10_000_000.0 * pixels / (1920.0 * 1080.0)).clamp(3_000_000.0, 20_000_000.0) as i32
}

unsafe extern "C" fn on_encoded(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: OSStatus,
    _flags: u32,
    sample: *mut c_void,
) {
    if status != 0 || sample.is_null() || refcon.is_null() {
        return;
    }
    let sink = unsafe { &*(refcon as *const Sink) };
    let key = unsafe { is_keyframe(sample) };
    if key {
        sink.need_key.store(false, Ordering::Relaxed);
        if let Some(config) = unsafe { config_packet(sample) } {
            if !sink.push(config) {
                return;
            }
        }
    } else if sink.need_key.load(Ordering::Relaxed) {
        return;
    }
    let Some(data) = (unsafe { sample_bytes(sample) }) else { return };
    let pts = unsafe { CMSampleBufferGetPresentationTimeStamp(sample) };
    let pts_us = if pts.timescale > 0 {
        (pts.value as i128 * 1_000_000 / pts.timescale as i128) as u64
    } else {
        0
    };
    sink.push(Packet::Frame { key, pts_us, data });
}

unsafe fn is_keyframe(sample: *mut c_void) -> bool {
    let attachments = unsafe { CMSampleBufferGetSampleAttachmentsArray(sample, 0) };
    if attachments.is_null() || unsafe { CFArrayGetCount(attachments) } == 0 {
        return true;
    }
    let dict = unsafe { CFArrayGetValueAtIndex(attachments, 0) } as CFDictionaryRef;
    unsafe { CFDictionaryContainsKey(dict, kCMSampleAttachmentKey_NotSync.cast()) == 0 }
}

unsafe fn config_packet(sample: *mut c_void) -> Option<Packet> {
    let desc = unsafe { CMSampleBufferGetFormatDescription(sample) };
    if desc.is_null() {
        return None;
    }
    let param = |index: usize| -> Option<Vec<u8>> {
        let (mut data, mut size, mut count, mut nal_len) = (ptr::null(), 0usize, 0usize, 0i32);
        let status = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                desc, index, &mut data, &mut size, &mut count, &mut nal_len,
            )
        };
        (status == 0 && !data.is_null()).then(|| unsafe { std::slice::from_raw_parts(data, size) }.to_vec())
    };
    let dims = unsafe { CMVideoFormatDescriptionGetDimensions(desc) };
    Some(Packet::Config {
        width: dims.width as u16,
        height: dims.height as u16,
        sps: param(0)?,
        pps: param(1)?,
    })
}

unsafe fn sample_bytes(sample: *mut c_void) -> Option<Vec<u8>> {
    let block = unsafe { CMSampleBufferGetDataBuffer(sample) };
    if block.is_null() {
        return None;
    }
    let len = unsafe { CMBlockBufferGetDataLength(block) };
    let mut out = vec![0u8; len];
    let status = unsafe { CMBlockBufferCopyDataBytes(block, 0, len, out.as_mut_ptr().cast()) };
    (status == 0).then_some(out)
}

// MARK: FFI

type OSStatus = i32;
type VTSession = *mut c_void;
type OutputCallback = unsafe extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void);

const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTimeRaw {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTimeRaw {
    const INVALID: Self = Self { value: 0, timescale: 0, flags: 0, epoch: 0 };

    fn micros(us: i64) -> Self {
        Self { value: us, timescale: 1_000_000, flags: 1, epoch: 0 }
    }
}

#[repr(C)]
struct CMVideoDimensions {
    width: i32,
    height: i32,
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: *const c_void,
        output_callback: Option<OutputCallback>,
        output_callback_refcon: *mut c_void,
        session_out: *mut VTSession,
    ) -> OSStatus;
    fn VTSessionSetProperty(session: VTSession, key: CFStringRef, value: CFTypeRef) -> OSStatus;
    fn VTCompressionSessionPrepareToEncodeFrames(session: VTSession) -> OSStatus;
    fn VTCompressionSessionEncodeFrame(
        session: VTSession,
        image_buffer: *mut c_void,
        pts: CMTimeRaw,
        duration: CMTimeRaw,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(session: VTSession, until: CMTimeRaw) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: VTSession);

    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: CFStringRef;
    static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetDataBuffer(sample: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetFormatDescription(sample: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(sample: *mut c_void) -> CMTimeRaw;
    fn CMSampleBufferGetSampleAttachmentsArray(sample: *mut c_void, create: u8) -> CFArrayRef;
    fn CMBlockBufferGetDataLength(block: *mut c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(block: *mut c_void, offset: usize, len: usize, dest: *mut c_void) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: *mut c_void,
        index: usize,
        parameter_set_out: *mut *const u8,
        size_out: *mut usize,
        count_out: *mut usize,
        nal_header_length_out: *mut i32,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetDimensions(desc: *mut c_void) -> CMVideoDimensions;

    static kCMSampleAttachmentKey_NotSync: CFStringRef;
}
