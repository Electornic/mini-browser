// The Event object every dispatched listener receives. Carries the type
// string + target wrapper, plus the four "control surface" pieces real
// browser handlers reach for: `currentTarget` (the ancestor whose
// listener is currently running), `defaultPrevented` / `preventDefault`,
// `stopPropagation`, and `stopImmediatePropagation`.
//
// The mutable bits live in a `Rc<RefCell<EventState>>` shared between the
// closures bolted onto the JS object and the dispatcher in `mod.rs`. The
// dispatcher updates `current_target` as it walks the bubble chain and
// reads `propagation_stopped` / `immediate_propagation_stopped` between
// handlers to decide whether to keep going. After dispatch returns, the
// caller (BrowserState) reads `default_prevented` to decide whether to
// run the default action (e.g. link navigation).

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsObject, JsString, JsValue, NativeFunction, js_string,
    object::ObjectInitializer, property::Attribute,
};

use crate::dom::{Document, NodeId};

use super::ListenerMap;
use super::element::make_element;

#[derive(Debug, Default)]
pub(super) struct EventState {
    // Set by `stopPropagation` / `stopImmediatePropagation`. Read by the
    // dispatcher between ancestors to break out of the bubble loop.
    pub propagation_stopped: bool,
    // Only set by `stopImmediatePropagation`. Read by the dispatcher
    // between handlers within the same ancestor — the rest of that
    // ancestor's listeners are skipped, then the propagation flag (also
    // set) prevents moving to the next ancestor.
    pub immediate_propagation_stopped: bool,
    // Set by `preventDefault`. Returned to the dispatch caller so it can
    // decide whether to run the default action (link navigate, form
    // submit, …). The Event API does not allow un-preventing.
    pub default_prevented: bool,
    // Updated by the dispatcher to the ancestor whose listener is
    // currently running. Read by the `currentTarget` accessor on each
    // access so a fresh Element wrapper is built against the latest
    // value (matches real browsers, which mutate `currentTarget` as the
    // bubble walks).
    pub current_target: Option<NodeId>,
}

pub(super) fn build_event_object(
    event_type: &str,
    target: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> (JsObject, Rc<RefCell<EventState>>) {
    let state = Rc::new(RefCell::new(EventState::default()));
    let target_wrapper = make_element(target, dom.clone(), listeners.clone(), context);

    let state_ct = state.clone();
    let dom_ct = dom.clone();
    let listeners_ct = listeners.clone();
    let current_target_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            // `current_target` is set by the dispatcher before each
            // ancestor's handlers run. It returns null between
            // dispatches and after dispatch finishes — matches the
            // post-dispatch behaviour real browsers expose.
            let node_id = state_ct.borrow().current_target;
            match node_id {
                Some(id) => Ok(JsValue::from(make_element(
                    id,
                    dom_ct.clone(),
                    listeners_ct.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    }
    .to_js_function(context.realm());

    let state_dp = state.clone();
    let default_prevented_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            Ok(JsValue::from(state_dp.borrow().default_prevented))
        })
    }
    .to_js_function(context.realm());

    let state_pd = state.clone();
    let prevent_default = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            state_pd.borrow_mut().default_prevented = true;
            Ok(JsValue::undefined())
        })
    };

    let state_sp = state.clone();
    let stop_propagation = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            state_sp.borrow_mut().propagation_stopped = true;
            Ok(JsValue::undefined())
        })
    };

    let state_sip = state.clone();
    let stop_immediate_propagation = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            // The spec says stopImmediatePropagation also sets the
            // ordinary stop-propagation flag. That couples cleanly with
            // the dispatcher loop: one check at the top of the outer
            // loop handles "skip remaining ancestors", another at the
            // top of the inner loop handles "skip remaining handlers
            // on this ancestor".
            let mut s = state_sip.borrow_mut();
            s.propagation_stopped = true;
            s.immediate_propagation_stopped = true;
            Ok(JsValue::undefined())
        })
    };

    let event_obj = ObjectInitializer::new(context)
        .property(
            js_string!("type"),
            JsString::from(event_type),
            Attribute::all(),
        )
        .property(
            js_string!("target"),
            JsValue::from(target_wrapper),
            Attribute::all(),
        )
        .accessor(
            js_string!("currentTarget"),
            Some(current_target_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("defaultPrevented"),
            Some(default_prevented_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .function(prevent_default, js_string!("preventDefault"), 0)
        .function(stop_propagation, js_string!("stopPropagation"), 0)
        .function(
            stop_immediate_propagation,
            js_string!("stopImmediatePropagation"),
            0,
        )
        .build();

    (event_obj, state)
}
