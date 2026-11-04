use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{declare_class, mutability, sel, ClassType, DeclaredClass};
use objc2_foundation::{CGPoint, CGRect, CGSize, MainThreadMarker, NSObject, NSRange, NSString};
use objc2_ui_kit::{
    UIReturnKeyType, UIScrollViewDelegate, UITextAutocapitalizationType, UITextAutocorrectionType,
    UITextInput, UITextInputTraits, UITextView, UITextViewDelegate, UIView,
};

use super::app_state::{self, EventWrapper};
use super::window::WinitUIWindow;
use crate::dpi::{Position, Size};
use crate::event::{
    ime_byte_offset, ime_utf16_offset, Event, Ime as ImeEvent, ImeAction, ImeTextState, WindowEvent,
};
use crate::window::{ImeOptions, ImePurpose, WindowId};

pub(crate) struct Ime {
    view: Retained<UITextView>,
    delegate: Retained<ImeDelegate>,
}

struct ImeState {
    updating: Cell<bool>,
    action: Cell<ImeAction>,
    selection: Cell<(usize, usize)>,
    area: Cell<Option<CGRect>>,
}

declare_class!(
    struct ImeDelegate;

    unsafe impl ClassType for ImeDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "WinitImeDelegate";
    }

    impl DeclaredClass for ImeDelegate {
        type Ivars = ImeState;
    }

    unsafe impl NSObjectProtocol for ImeDelegate {}
    unsafe impl UIScrollViewDelegate for ImeDelegate {}

    unsafe impl UITextViewDelegate for ImeDelegate {
        #[method(textViewDidBeginEditing:)]
        fn began(&self, view: &UITextView) {
            self.emit(view, ImeEvent::Enabled);
        }

        #[method(textViewDidEndEditing:)]
        fn ended(&self, view: &UITextView) {
            self.emit(view, ImeEvent::Disabled);
        }

        #[method(textViewDidChange:)]
        fn changed(&self, view: &UITextView) {
            self.publish(view);
        }

        #[method(textViewDidChangeSelection:)]
        fn selected(&self, view: &UITextView) {
            self.publish(view);
        }

        #[method(textView:shouldChangeTextInRange:replacementText:)]
        fn should_change(&self, view: &UITextView, _range: NSRange, text: &NSString) -> bool {
            let submit = !self.ivars().updating.get()
                && text.to_string() == "\n"
                && self.ivars().action.get() != ImeAction::Newline
                && unsafe { view.markedTextRange().is_none() };
            if submit {
                self.emit(view, ImeEvent::Action(self.ivars().action.get()));
            }
            !submit
        }
    }
);

impl ImeDelegate {
    fn emit(&self, view: &UITextView, event: ImeEvent) {
        let Some(window) = view.window() else {
            return;
        };
        // Every native editor is attached to its owning winit window.
        let window: Retained<WinitUIWindow> = unsafe { Retained::cast(window) };
        app_state::handle_nonuser_event(
            MainThreadMarker::from(self),
            EventWrapper::StaticEvent(Event::WindowEvent {
                window_id: WindowId(window.id()),
                event: WindowEvent::Ime(event),
            }),
        );
    }

    fn publish(&self, view: &UITextView) {
        if self.ivars().updating.get() || !unsafe { view.isFirstResponder() } {
            return;
        }
        let text = unsafe { view.text() }.to_string();
        let selected = unsafe { view.selectedRange() };
        let start = selected.location;
        let end = start.saturating_add(selected.length);
        let (anchor, caret) = self.ivars().selection.get();
        let selection = if start == anchor.min(caret) && end == anchor.max(caret) {
            (anchor, caret)
        } else if end == anchor.max(caret) {
            (end, start)
        } else {
            (start, end)
        };
        self.ivars().selection.set(selection);
        let composition = unsafe {
            view.markedTextRange().map(|range| {
                let origin = view.beginningOfDocument();
                let start =
                    view.offsetFromPosition_toPosition(&origin, &range.start()).max(0) as usize;
                let end = view.offsetFromPosition_toPosition(&origin, &range.end()).max(0) as usize;
                ime_byte_offset(&text, start)..ime_byte_offset(&text, end)
            })
        };
        self.align(view);
        self.emit(
            view,
            ImeEvent::TextState(ImeTextState {
                selection: ime_byte_offset(&text, selection.0)..ime_byte_offset(&text, selection.1),
                composition,
                text,
            }),
        );
    }

    fn align(&self, view: &UITextView) {
        let Some(area) = self.ivars().area.get() else {
            return;
        };
        let Some(owner) = view.superview() else {
            return;
        };
        let Some(selected) = (unsafe { view.selectedTextRange() }) else {
            return;
        };
        let updating = self.ivars().updating.replace(true);
        unsafe {
            view.layoutIfNeeded();
            let caret =
                view.convertRect_toView(view.caretRectForPosition(&selected.end()), Some(&owner));
            let mut frame = view.frame();
            frame.origin.x += area.origin.x - caret.origin.x;
            frame.origin.y += area.origin.y + area.size.height - caret.origin.y - caret.size.height;
            view.setFrame(frame);
        }
        self.ivars().updating.set(updating);
    }
}

impl Ime {
    pub(crate) fn new(mtm: MainThreadMarker, frame: CGRect) -> Self {
        let delegate = mtm.alloc().set_ivars(ImeState {
            updating: Cell::new(false),
            action: Cell::new(ImeAction::Submit),
            selection: Cell::new((0, 0)),
            area: Cell::new(None),
        });
        let delegate: Retained<ImeDelegate> = unsafe { objc2::msg_send_id![super(delegate), init] };
        let view = unsafe { UITextView::initWithFrame(mtm.alloc(), frame) };
        view.setHidden(true);
        unsafe {
            view.setScrollEnabled(false);
            view.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        Self { view, delegate }
    }

    pub(crate) fn attach(&self, owner: &UIView) {
        unsafe {
            owner.addSubview(&self.view);
        }
    }

    pub(crate) fn set_allowed(&self, allowed: bool) {
        self.delegate.ivars().updating.set(true);
        unsafe {
            if allowed {
                self.view.becomeFirstResponder();
            } else {
                self.view.resignFirstResponder();
            }
        }
        self.delegate.ivars().updating.set(false);
    }

    pub(crate) fn set_text_state(&self, state: ImeTextState) {
        let ivars = self.delegate.ivars();
        ivars.updating.set(true);
        let start = ime_utf16_offset(&state.text, state.selection.start);
        let end = ime_utf16_offset(&state.text, state.selection.end);
        unsafe {
            if self.view.text().to_string() != state.text {
                self.view.setText(Some(&NSString::from_str(&state.text)));
            }
            if let Some(range) = state.composition.filter(|range| !range.is_empty()) {
                if let Some(text) = state.text.get(range.clone()) {
                    let from = ime_utf16_offset(&state.text, range.start);
                    let to = ime_utf16_offset(&state.text, range.end);
                    self.view.setSelectedRange(NSRange::new(from, to - from));
                    self.view.setMarkedText_selectedRange(
                        Some(&NSString::from_str(text)),
                        NSRange::new(start.min(end).saturating_sub(from), start.abs_diff(end)),
                    );
                }
            } else {
                self.view.unmarkText();
            }
            self.view.setSelectedRange(NSRange::new(start.min(end), start.abs_diff(end)));
        }
        ivars.selection.set((start, end));
        self.delegate.align(&self.view);
        ivars.updating.set(false);
    }

    pub(crate) fn set_purpose(&self, purpose: ImePurpose) {
        self.set_options(ImeOptions { purpose, action: self.delegate.ivars().action.get() });
    }

    pub(crate) fn set_options(&self, options: ImeOptions) {
        self.delegate.ivars().updating.set(true);
        self.delegate.ivars().action.set(options.action);
        unsafe {
            self.view.setSecureTextEntry(options.purpose == ImePurpose::Password);
            // These void(NSInteger) setters use forwarding, which objc2's method-table check cannot
            // follow.
            let set_trait = |selector, value| {
                let send: unsafe extern "C-unwind" fn(&UITextView, Sel, isize) =
                    std::mem::transmute(objc2::ffi::objc_msgSend as *const ());
                send(&self.view, selector, value);
            };
            let (correction, capitalization) = if options.purpose == ImePurpose::Normal {
                (UITextAutocorrectionType::Default, UITextAutocapitalizationType::Sentences)
            } else {
                (UITextAutocorrectionType::No, UITextAutocapitalizationType::None)
            };
            set_trait(sel!(setAutocorrectionType:), correction.0);
            set_trait(sel!(setAutocapitalizationType:), capitalization.0);
            set_trait(
                sel!(setReturnKeyType:),
                match options.action {
                    ImeAction::Newline | ImeAction::Previous => UIReturnKeyType::UIReturnKeyDefault,
                    ImeAction::Submit => UIReturnKeyType::UIReturnKeyDone,
                    ImeAction::Next => UIReturnKeyType::UIReturnKeyNext,
                }
                .0,
            );
            self.view.reloadInputViews();
        }
        self.delegate.ivars().updating.set(false);
    }

    pub(crate) fn set_cursor_area(&self, position: Position, size: Size) {
        let owner = self.view.superview().expect("IME editor must be attached");
        let scale = owner.contentScaleFactor() as f64;
        let position = position.to_logical::<f64>(scale);
        let size = size.to_logical::<f64>(scale);
        self.delegate.ivars().area.set(Some(CGRect {
            origin: CGPoint { x: position.x as _, y: position.y as _ },
            size: CGSize { width: size.width as _, height: size.height as _ },
        }));
        self.delegate.align(&self.view);
    }
}

impl Drop for Ime {
    fn drop(&mut self) {
        unsafe {
            self.view.setDelegate(None);
        }
        unsafe {
            self.view.removeFromSuperview();
        }
    }
}
