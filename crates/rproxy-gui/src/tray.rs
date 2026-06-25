#[derive(Debug, Clone, Copy)]
pub enum TrayEvent {
    ShowWindow,
    ToggleProxy,
    OpenPac,
    Quit,
}

#[cfg(windows)]
mod platform {
    use super::TrayEvent;
    use std::{
        ffi::c_void,
        io,
        ptr::null,
        sync::{mpsc::Sender, Mutex, OnceLock},
        thread::{self, JoinHandle},
    };
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW,
                PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TrackPopupMenu,
                TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HMENU, IDI_APPLICATION,
                MF_SEPARATOR, MF_STRING, MSG, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
                WM_CLOSE, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WM_USER, WNDCLASSW,
                WS_OVERLAPPED,
            },
        },
    };

    const TRAY_UID: u32 = 1;
    const TRAY_MESSAGE: u32 = WM_USER + 1;
    const CMD_SHOW: usize = 1001;
    const CMD_TOGGLE: usize = 1002;
    const CMD_OPEN_PAC: usize = 1003;
    const CMD_QUIT: usize = 1004;

    static EVENT_SENDER: OnceLock<Mutex<Option<Sender<TrayEvent>>>> = OnceLock::new();

    pub struct TrayHandle {
        hwnd: isize,
        thread: Option<JoinHandle<()>>,
    }

    impl TrayHandle {
        pub fn new(sender: Sender<TrayEvent>) -> io::Result<Self> {
            let sender_slot = EVENT_SENDER.get_or_init(|| Mutex::new(None));
            *sender_slot.lock().expect("tray sender mutex poisoned") = Some(sender);

            let (hwnd_tx, hwnd_rx) = std::sync::mpsc::channel::<io::Result<isize>>();
            let thread = thread::spawn(move || unsafe {
                match create_tray_window() {
                    Ok(hwnd) => {
                        let _ = hwnd_tx.send(Ok(hwnd as isize));
                        run_message_loop();
                    }
                    Err(error) => {
                        let _ = hwnd_tx.send(Err(error));
                    }
                }
            });

            match hwnd_rx
                .recv()
                .map_err(|_| io::Error::other("tray thread exited during startup"))?
            {
                Ok(hwnd) => Ok(Self {
                    hwnd,
                    thread: Some(thread),
                }),
                Err(error) => {
                    let _ = thread.join();
                    Err(error)
                }
            }
        }
    }

    impl Drop for TrayHandle {
        fn drop(&mut self) {
            if let Some(sender_slot) = EVENT_SENDER.get() {
                *sender_slot.lock().expect("tray sender mutex poisoned") = None;
            }

            if self.hwnd != 0 {
                unsafe {
                    let _ = PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }

            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    unsafe fn create_tray_window() -> io::Result<HWND> {
        let class_name = wide("RProxyTrayWindow");
        let instance = GetModuleHandleW(null());
        if instance.is_null() {
            return Err(io::Error::last_os_error());
        }

        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        if RegisterClassW(&window_class) == 0 {
            return Err(io::Error::last_os_error());
        }

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            null::<c_void>(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }

        add_tray_icon(hwnd)?;
        Ok(hwnd)
    }

    unsafe fn run_message_loop() {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            TRAY_MESSAGE => {
                match lparam as u32 {
                    WM_LBUTTONDBLCLK => send_event(TrayEvent::ShowWindow),
                    WM_RBUTTONUP => show_menu(hwnd),
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                delete_tray_icon(hwnd);
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn add_tray_icon(hwnd: HWND) -> io::Result<()> {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
            ..Default::default()
        };
        copy_wide("RProxy", &mut data.szTip);

        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    unsafe fn delete_tray_icon(hwnd: HWND) {
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }

    unsafe fn show_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        append_menu_item(menu, CMD_SHOW, "显示主窗口");
        append_menu_item(menu, CMD_TOGGLE, "启动/停止代理");
        append_menu_item(menu, CMD_OPEN_PAC, "打开 PAC");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, null());
        append_menu_item(menu, CMD_QUIT, "退出");

        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor) != 0 {
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
                cursor.x,
                cursor.y,
                0,
                hwnd,
                null(),
            );
            dispatch_command(command as usize);
        }

        DestroyMenu(menu);
    }

    unsafe fn append_menu_item(menu: HMENU, command: usize, label: &str) {
        let label = wide(label);
        let _ = AppendMenuW(menu, MF_STRING, command, label.as_ptr());
    }

    fn dispatch_command(command: usize) {
        match command {
            CMD_SHOW => send_event(TrayEvent::ShowWindow),
            CMD_TOGGLE => send_event(TrayEvent::ToggleProxy),
            CMD_OPEN_PAC => send_event(TrayEvent::OpenPac),
            CMD_QUIT => send_event(TrayEvent::Quit),
            _ => {}
        }
    }

    fn send_event(event: TrayEvent) {
        if let Some(sender_slot) = EVENT_SENDER.get() {
            if let Some(sender) = sender_slot
                .lock()
                .expect("tray sender mutex poisoned")
                .as_ref()
            {
                let _ = sender.send(event);
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn copy_wide(value: &str, target: &mut [u16]) {
        let value = wide(value);
        let len = value.len().min(target.len());
        target[..len].copy_from_slice(&value[..len]);
    }
}

#[cfg(not(windows))]
mod platform {
    use super::TrayEvent;
    use std::{io, sync::mpsc::Sender};

    pub struct TrayHandle;

    impl TrayHandle {
        pub fn new(_sender: Sender<TrayEvent>) -> io::Result<Self> {
            Ok(Self)
        }
    }
}

pub use platform::TrayHandle;
