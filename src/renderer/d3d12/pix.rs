use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::PathBuf};

use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    },
    core::{Interface, PCSTR, PCWSTR},
};

type BeginEventOnCommandList = unsafe extern "system" fn(*mut c_void, u64, *const i8);
type EndEventOnCommandList = unsafe extern "system" fn(*mut c_void);

/// Optional WinPixEventRuntime bridge. Calling ID3D12GraphicsCommandList's
/// internal BeginEvent/EndEvent directly produces validation errors; when the
/// runtime DLL is unavailable we therefore emit no internal calls.
pub struct PixEventRuntime {
    module: Option<HMODULE>,
    begin_event: Option<BeginEventOnCommandList>,
    end_event: Option<EndEventOnCommandList>,
}

impl PixEventRuntime {
    pub fn load() -> Self {
        let Some(runtime_path) = runtime_path() else {
            eprintln!("PIX：无法定位当前可执行文件，GPU event 标记不可用");
            return Self::unavailable();
        };
        let wide_path = runtime_path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // Load by an absolute executable-adjacent path. This is both
        // reproducible and avoids resolving an unrelated DLL from PATH/CWD.
        let module = unsafe { LoadLibraryW(PCWSTR(wide_path.as_ptr())) }.ok();
        let Some(module) = module else {
            eprintln!(
                "PIX：未找到 {}，GPU event 标记不可用",
                runtime_path.display()
            );
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
            let _ = unsafe { FreeLibrary(module) };
            return Self::unavailable();
        }
        eprintln!("PIX：已加载 {}，GPU event 标记可用", runtime_path.display());
        Self {
            module: Some(module),
            begin_event,
            end_event,
        }
    }

    pub fn is_available(&self) -> bool {
        self.begin_event.is_some() && self.end_event.is_some()
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
            module: None,
            begin_event: None,
            end_event: None,
        }
    }
}

impl Drop for PixEventRuntime {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            let _ = unsafe { FreeLibrary(module) };
        }
    }
}

fn runtime_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|directory| directory.join("WinPixEventRuntime.dll"))
}
