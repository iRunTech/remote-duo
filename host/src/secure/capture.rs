//! DXGI desktop duplication, which (unlike Windows Graphics Capture) can see
//! the sign-in screen when run as SYSTEM on that desktop. Written here rather
//! than taken from xcap, whose DXGI recorder never lets go of the output.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
    IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

pub enum Capture {
    Frame { width: u32, height: u32, rgba: Vec<u8> },
    /// Nothing new on screen before the timeout.
    Unchanged,
    /// The duplication died (usually a desktop switch); make a new one.
    Lost,
}

pub struct Duplicator {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    first: bool,
}

impl Duplicator {
    /// Duplicates the output showing `monitor` (an HMONITOR, as in display
    /// ids), or the first output if it's gone.
    pub fn new(monitor: u32) -> windows::core::Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut chosen: Option<(IDXGIAdapter1, IDXGIOutput)> = None;
            'search: for a in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(a) else { break };
                for o in 0.. {
                    let Ok(output) = adapter.EnumOutputs(o) else { break };
                    let exact = output.GetDesc().is_ok_and(|d| d.Monitor.0 as usize as u32 == monitor);
                    if exact || chosen.is_none() {
                        chosen = Some((adapter.clone(), output));
                    }
                    if exact {
                        break 'search;
                    }
                }
            }
            let (adapter, output) = chosen.ok_or_else(|| windows::core::Error::from(DXGI_ERROR_NOT_FOUND))?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.ok_or_else(|| windows::core::Error::from(DXGI_ERROR_NOT_FOUND))?;
            let context = context.ok_or_else(|| windows::core::Error::from(DXGI_ERROR_NOT_FOUND))?;
            let dup = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)?;
            Ok(Self { device, context, dup, staging: None, first: true })
        }
    }

    pub fn next(&mut self, timeout_ms: u32) -> Capture {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { self.dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Capture::Unchanged,
            Err(_) => return Capture::Lost,
        }
        // A zero present time means only the pointer moved.
        let frame = if info.LastPresentTime != 0 || self.first { self.read(resource) } else { None };
        unsafe {
            let _ = self.dup.ReleaseFrame();
        }
        match frame {
            Some((width, height, rgba)) => {
                self.first = false;
                Capture::Frame { width, height, rgba }
            }
            None => Capture::Unchanged,
        }
    }

    /// Copies the frame to CPU memory as RGBA.
    fn read(&mut self, resource: Option<IDXGIResource>) -> Option<(u32, u32, Vec<u8>)> {
        unsafe {
            let texture: ID3D11Texture2D = resource?.cast().ok()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            let (width, height) = (desc.Width, desc.Height);
            if self.staging.as_ref().is_none_or(|(_, w, h)| (*w, *h) != (width, height)) {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    MipLevels: 1,
                    ArraySize: 1,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..desc
                };
                let mut staging = None;
                self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).ok()?;
                self.staging = Some((staging?, width, height));
            }
            let staging = &self.staging.as_ref()?.0;
            self.context.CopyResource(staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
            let (w, h, pitch) = (width as usize, height as usize, mapped.RowPitch as usize);
            let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h);
            let mut rgba = vec![0u8; w * h * 4];
            // Desktop duplication hands out BGRA.
            for (row, out) in rgba.chunks_exact_mut(w * 4).enumerate() {
                let line = &src[row * pitch..row * pitch + w * 4];
                for (o, i) in out.as_chunks_mut::<4>().0.iter_mut().zip(line.as_chunks::<4>().0) {
                    o.copy_from_slice(&[i[2], i[1], i[0], 255]);
                }
            }
            self.context.Unmap(staging, 0);
            Some((width, height, rgba))
        }
    }
}
