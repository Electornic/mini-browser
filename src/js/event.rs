// Minimal Event object passed to every dispatched listener. Carries the
// event type string and a wrapper for the original target Element. Future
// commits will round it out with `currentTarget`, `preventDefault`, and
// `stopPropagation` — the toy bridge skips them since clicks always
// bubble fully through and the only side-effect a handler can suppress
// today is link navigation, which Step 6 explicitly leaves running.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsObject, JsString, JsValue, js_string, object::ObjectInitializer, property::Attribute,
};

use crate::dom::{Document, NodeId};

use super::ListenerMap;
use super::element::make_element;

pub(super) fn build_event_object(
    event_type: &str,
    target: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> JsObject {
    let target_wrapper = make_element(target, dom, listeners, context);
    ObjectInitializer::new(context)
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
        .build()
}
