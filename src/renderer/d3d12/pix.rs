use std::ffi::c_void;

use windows::{
    Win32::{
        Foundation::HMODULE,
        System::LibraryLoader::{GetProcAddress, LoadLibraryA},
    },
    core::{Interface, PCSTR},
};

type BeginEventOnCommandList = unsafe extern "system" fn(*mut c_void, u64, *const i8);
type EndEventOnCommandList = unsafe extern "system" fn(*mut c_void);

/// Optional WinPixEventRuntime bridge. Calling ID3D12GraphicsCommandList's
/// internal BeginEvent/EndEvent directly produces validation errors; when the
/// runtime DLL is unavailable we therefore emit no internal calls.
pub struct PixEventRuntime {
    _module: Option<HMODULE>,
    begin_event: Option<BeginEventOnCommandList>,
    end_event: Option<EndEventOnCommandList>,
}

impl PixEventRuntime {
    pub fn load() -> Self {
        let module = unsafe { LoadLibraryA(PCSTR(c"WinPixEventRuntime.dll".as_ptr().cast())) }.ok();
        let Some(module) = module else {
            eprintln!("PIX：未找到 WinPixEventRuntime.dll，GPU event 标记不可用");
            return Self::unavailable();
        };

        let begin_event = unsafe {
            GetProcAddress(module, PCSTR(c"PIXBeginEventOnCommandList".as_ptr().cast())).map(
                |proc| {
                    std::mem::transmute::<
                        unsafe extern "system" fn() -> isize,
                        BeginEventOnCommandList,
                    >(proc)
                },
            )
        };
        let end_event = unsafe {
            GetProcAddress(module, PCSTR(c"PIXEndEventOnCommandList".as_ptr().cast())).map(|proc| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, EndEventOnCommandList>(
                    proc,
                )
            })
        };
        if begin_event.is_none() || end_event.is_none() {
            eprintln!(
                "PIX：WinPixEventRuntime.dll 缺少 command-list event 导出，GPU event 标记不可用"
            );
            return Self::unavailable();
        }
        Self {
            _module: Some(module),
            begin_event,
            end_event,
        }
    }

    pub fn begin(
        &self,
        command_list: &windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList,
        label: &'static [u8],
    ) {
        let Some(begin_event) = self.begin_event else {
            return;
        };
        unsafe { begin_event(command_list.as_raw(), 0, label.as_ptr().cast()) };
    }

    pub fn end(
        &self,
        command_list: &windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList,
    ) {
        let Some(end_event) = self.end_event else {
            return;
        };
        unsafe { end_event(command_list.as_raw()) };
    }

    fn unavailable() -> Self {
        Self {
            _module: None,
            begin_event: None,
            end_event: None,
        }
    }
}
