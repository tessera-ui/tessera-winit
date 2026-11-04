use objc2::msg_send;
use objc2_foundation::{NSRange, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{Ime, ImeChangeCause, ImeTextState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct App {
    window: Option<Window>,
    step: u8,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop.create_window(Window::default_attributes()).unwrap();
        window.set_ime_options(Default::default());
        window.set_ime_allowed(true);
        window
            .set_ime_text_state(
                ImeTextState { text: "a😃中z".into(), selection: 5..8, composition: None },
                ImeChangeCause::Other,
            )
            .unwrap();
        window.set_ime_cursor_area(PhysicalPosition::new(20, 20), PhysicalSize::new(1, 20));
        self.window = Some(window);
        native_edit(self.window.as_ref().unwrap(), 0);
    }

    fn window_event(&mut self, _: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let WindowEvent::Ime(Ime::TextState(state)) = event {
            assert_eq!(id, self.window.as_ref().unwrap().id());
            eprintln!("NATIVE_IME_STATE: {state:?}");
            let (text, composition, cursor) = match self.step {
                0 => ("a😃你z", Some(5..8), 8),
                1 => ("a😃你z", None, 8),
                2 => ("a😃你語z", None, 11),
                _ => unreachable!(),
            };
            if state.text != text || state.composition != composition {
                return;
            }
            assert_eq!(state.selection, cursor..cursor);
            if self.step == 2 {
                use std::io::Write;
                println!("NATIVE_IME_PASS");
                std::io::stdout().flush().unwrap();
                std::process::exit(0);
            }
            self.step += 1;
            native_edit(self.window.as_ref().unwrap(), self.step);
        }
    }
}

#[cfg(target_os = "macos")]
fn native_edit(window: &Window, step: u8) {
    let RawWindowHandle::AppKit(handle) = window.window_handle().unwrap().as_raw() else {
        panic!("expected an AppKit window");
    };
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<objc2::runtime::AnyObject>() };
    unsafe {
        match step {
            0 => {
                let _: () = msg_send![view, setMarkedText: &*NSString::from_str("你")
                selectedRange: NSRange::new(1, 0)
                replacementRange: NSRange::new(isize::MAX as usize, 0)];
            },
            1 => {
                let _: () = msg_send![view, unmarkText];
            },
            2 => {
                let _: () = msg_send![view, insertText: &*NSString::from_str("語")
                replacementRange: NSRange::new(isize::MAX as usize, 0)];
            },
            _ => unreachable!(),
        }
    }
}

#[cfg(target_os = "ios")]
fn native_edit(window: &Window, step: u8) {
    use objc2::rc::Retained;
    use objc2::ClassType;
    use objc2_ui_kit::UIView;

    fn responder(view: &UIView) -> Option<Retained<UIView>> {
        if unsafe { view.isFirstResponder() } {
            return Some(view.retain());
        }
        view.subviews().iter().find_map(|child| responder(child))
    }
    let RawWindowHandle::UiKit(handle) = window.window_handle().unwrap().as_raw() else {
        panic!("expected a UIKit window");
    };
    let root = unsafe { &*handle.ui_view.as_ptr().cast::<UIView>() };
    let view = responder(root).expect("native text input must become first responder");
    unsafe {
        match step {
            0 => {
                let _: () = msg_send![&*view, setMarkedText: &*NSString::from_str("你")
                selectedRange: NSRange::new(1, 0)];
            },
            1 => {
                let _: () = msg_send![&*view, unmarkText];
            },
            2 => {
                let _: () = msg_send![&*view, insertText: &*NSString::from_str("語")];
            },
            _ => unreachable!(),
        }
    }
}

fn main() {
    EventLoop::new().unwrap().run_app(&mut App::default()).unwrap();
}
