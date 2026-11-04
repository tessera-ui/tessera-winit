use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{
    CompositionEvent, Document, Event, FocusOptions, HtmlCanvasElement, HtmlElement,
    HtmlInputElement, HtmlTextAreaElement, InputEvent,
};

use crate::dpi::{Position, Size};
use crate::event::{ime_byte_offset, ime_utf16_offset, Ime as ImeEvent, ImeAction, ImeTextState};
use crate::window::{ImeOptions, ImePurpose};

use super::EventListenerHandle;

pub struct Ime {
    state: Rc<State>,
    _listeners: Vec<EventListenerHandle<dyn FnMut(Event)>>,
}

struct State {
    input: RefCell<NativeInput>,
    action: Cell<ImeAction>,
    canvas: HtmlCanvasElement,
    document: Document,
    enabled: Cell<bool>,
    updating: Cell<bool>,
    composition: Cell<Option<(usize, usize)>>,
    emit: Box<dyn Fn(ImeEvent)>,
}

enum NativeInput {
    Text(HtmlTextAreaElement),
    Password(HtmlInputElement),
}

impl NativeInput {
    fn new(document: &Document, password: bool, id: &str) -> Self {
        let input = if password {
            let input: HtmlInputElement =
                document.create_element("input").unwrap().unchecked_into();
            input.set_type("password");
            Self::Password(input)
        } else {
            Self::Text(document.create_element("textarea").unwrap().unchecked_into())
        };
        let element = input.element();
        element.set_tab_index(-1);
        element.set_attribute("data-winit-ime", id).unwrap();
        element.set_attribute("autocomplete", "off").unwrap();
        element.set_attribute("autocapitalize", "off").unwrap();
        element.set_spellcheck(false);
        element.style().set_css_text(
            "position:fixed;left:0;top:0;opacity:0;pointer-events:none;width:1px;height:20px;\
             padding:0;border:0;overflow:hidden;font-size:16px;white-space:pre;",
        );
        input
    }

    fn element(&self) -> &HtmlElement {
        match self {
            Self::Text(input) => input,
            Self::Password(input) => input,
        }
    }

    fn value(&self) -> String {
        match self {
            Self::Text(input) => input.value(),
            Self::Password(input) => input.value(),
        }
    }

    fn set_value(&self, text: &str) {
        match self {
            Self::Text(input) => input.set_value(text),
            Self::Password(input) => input.set_value(text),
        }
    }

    fn selection(&self) -> (usize, usize, bool) {
        let (start, end, direction) = match self {
            Self::Text(input) => {
                (input.selection_start(), input.selection_end(), input.selection_direction())
            },
            Self::Password(input) => {
                (input.selection_start(), input.selection_end(), input.selection_direction())
            },
        };
        (
            start.ok().flatten().unwrap_or(0) as usize,
            end.ok().flatten().unwrap_or(0) as usize,
            direction.ok().flatten().as_deref() == Some("backward"),
        )
    }

    fn select(&self, start: u32, end: u32) {
        let direction = if start > end { "backward" } else { "forward" };
        let _ = match self {
            Self::Text(input) => {
                input.set_selection_range_with_direction(start.min(end), start.max(end), direction)
            },
            Self::Password(input) => {
                input.set_selection_range_with_direction(start.min(end), start.max(end), direction)
            },
        };
    }
}

impl State {
    fn is_input(&self, target: Option<web_sys::EventTarget>) -> bool {
        target.as_ref() == Some(self.input.borrow().element().as_ref())
    }

    fn publish(&self) {
        if !self.enabled.get() || self.updating.get() {
            return;
        }
        let (text, start, end, backward) = {
            let input = self.input.borrow();
            let (start, end, backward) = input.selection();
            (input.value(), start, end, backward)
        };
        let start = ime_byte_offset(&text, start);
        let end = ime_byte_offset(&text, end);
        let selection = if backward { end..start } else { start..end };
        let composition = self.composition.get().and_then(|(start, end)| {
            (start != end).then(|| ime_byte_offset(&text, start)..ime_byte_offset(&text, end))
        });
        (self.emit)(ImeEvent::TextState(ImeTextState { text, selection, composition }));
    }
}

fn focus(element: &HtmlElement) {
    let options = FocusOptions::new();
    options.set_prevent_scroll(true);
    let _ = element.focus_with_options(&options);
}

impl Ime {
    pub fn new(
        document: &Document,
        canvas: &HtmlCanvasElement,
        id: &str,
        emit: impl Fn(ImeEvent) + 'static,
    ) -> Self {
        let input = NativeInput::new(document, false, id);
        document.body().expect("Missing document body").append_child(input.element()).unwrap();
        let state = Rc::new(State {
            input: RefCell::new(input),
            action: Cell::new(ImeAction::Submit),
            canvas: canvas.clone(),
            document: document.clone(),
            enabled: Cell::new(false),
            updating: Cell::new(false),
            composition: Cell::new(None),
            emit: Box::new(emit),
        });
        let mut listeners = Vec::new();
        for name in [
            "beforeinput",
            "input",
            "select",
            "compositionstart",
            "compositionupdate",
            "compositionend",
        ] {
            let state = state.clone();
            listeners.push(EventListenerHandle::new(
                document.clone(),
                name,
                Closure::new(move |event: Event| {
                    if state.updating.get()
                        || !state.enabled.get()
                        || !state.is_input(event.target())
                    {
                        return;
                    }
                    match name {
                        "beforeinput" => {
                            let input = event.unchecked_ref::<InputEvent>();
                            if matches!(
                                input.input_type().as_str(),
                                "insertLineBreak" | "insertParagraph"
                            ) && state.action.get() != ImeAction::Newline
                            {
                                event.prevent_default();
                                (state.emit)(ImeEvent::Action(state.action.get()));
                            }
                            return;
                        },
                        "compositionstart" => {
                            let start = state.input.borrow().selection().0;
                            state.composition.set(Some((start, start)));
                            return;
                        },
                        "compositionupdate" => {
                            if let Some((start, _)) = state.composition.get() {
                                let length = event
                                    .unchecked_ref::<CompositionEvent>()
                                    .data()
                                    .unwrap_or_default()
                                    .encode_utf16()
                                    .count();
                                state.composition.set(Some((start, start + length)));
                            }
                            return;
                        },
                        "compositionend" => state.composition.set(None),
                        "input" => {
                            let input = event.unchecked_ref::<InputEvent>();
                            if !input.is_composing() {
                                state.composition.set(None);
                            }
                        },
                        _ => {},
                    }
                    state.publish();
                }),
            ));
        }
        let selection = state.clone();
        listeners.push(EventListenerHandle::new(
            document.clone(),
            "selectionchange",
            Closure::new(move |_: Event| {
                if selection.is_input(selection.document.active_element().map(Into::into)) {
                    selection.publish();
                }
            }),
        ));
        Self { state, _listeners: listeners }
    }

    pub fn set_allowed(&self, allowed: bool) {
        let state = &self.state;
        if state.enabled.replace(allowed) == allowed {
            if allowed {
                focus(state.input.borrow().element());
            }
            return;
        }
        state.updating.set(true);
        if allowed {
            focus(state.input.borrow().element());
        } else {
            if state.is_input(state.document.active_element().map(Into::into)) {
                focus(&state.canvas);
            }
            state.composition.set(None);
        }
        state.updating.set(false);
        (state.emit)(if allowed { ImeEvent::Enabled } else { ImeEvent::Disabled });
    }

    pub fn set_text_state(&self, snapshot: ImeTextState) {
        let state = &self.state;
        state.updating.set(true);
        let changed = state.input.borrow().value() != snapshot.text;
        if state.composition.get().is_some_and(|(start, end)| start != end)
            && (changed || snapshot.composition.is_none())
        {
            focus(&state.canvas);
            state.composition.set(None);
        }
        if changed {
            state.input.borrow().set_value(&snapshot.text);
        }
        let start = ime_utf16_offset(&snapshot.text, snapshot.selection.start) as u32;
        let end = ime_utf16_offset(&snapshot.text, snapshot.selection.end) as u32;
        state.input.borrow().select(start, end);
        if state.enabled.get() && !state.is_input(state.document.active_element().map(Into::into)) {
            focus(state.input.borrow().element());
        }
        state.updating.set(false);
    }

    pub fn set_purpose(&self, purpose: ImePurpose) {
        self.set_options(ImeOptions { purpose, action: self.state.action.get() });
    }

    pub fn set_options(&self, options: ImeOptions) {
        let state = &self.state;
        state.updating.set(true);
        let password = options.purpose == ImePurpose::Password;
        if password != matches!(*state.input.borrow(), NativeInput::Password(_)) {
            let (text, selection, id, style) = {
                let input = state.input.borrow();
                (
                    input.value(),
                    input.selection(),
                    input.element().get_attribute("data-winit-ime").unwrap(),
                    input.element().style().css_text(),
                )
            };
            let input = NativeInput::new(&state.document, password, &id);
            input.element().style().set_css_text(&style);
            input.set_value(&text);
            let (start, end, backward) = selection;
            if backward {
                input.select(end as u32, start as u32);
            } else {
                input.select(start as u32, end as u32);
            }
            state.document.body().unwrap().append_child(input.element()).unwrap();
            if state.enabled.get() {
                focus(&state.canvas);
            }
            let previous = state.input.replace(input);
            previous.element().remove();
            state.composition.set(None);
            if state.enabled.get() {
                focus(state.input.borrow().element());
            }
        }
        state.action.set(options.action);
        state
            .input
            .borrow()
            .element()
            .set_attribute("enterkeyhint", match options.action {
                ImeAction::Newline => "enter",
                ImeAction::Submit => "done",
                ImeAction::Next => "next",
                ImeAction::Previous => "previous",
            })
            .unwrap();
        state.updating.set(false);
    }

    pub fn set_cursor_area(&self, position: Position, size: Size) {
        let scale = self.state.document.default_view().unwrap().device_pixel_ratio();
        let position = position.to_logical::<f64>(scale);
        let size = size.to_logical::<f64>(scale);
        let bounds = self.state.canvas.get_bounding_client_rect();
        let input = self.state.input.borrow();
        let style = input.element().style();
        let _ = style.set_property("left", &format!("{}px", bounds.left() + position.x));
        let _ = style.set_property("top", &format!("{}px", bounds.top() + position.y));
        let _ = style.set_property("height", &format!("{}px", size.height.max(1.0)));
    }
}

impl Drop for Ime {
    fn drop(&mut self) {
        self.state.input.borrow().element().remove();
    }
}
