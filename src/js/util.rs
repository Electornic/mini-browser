// Argument-shape helpers used across every host-API closure: argument
// stringification (which all DOM string args funnel through) and recovery of
// a NodeId from any Element wrapper (which mutation methods like
// `appendChild` rely on to identify their argument).

use boa_engine::{
    Context, JsNativeError, JsResult, JsValue, js_string,
};

use crate::dom::NodeId;

use super::NODE_ID_PROP;

pub(super) fn first_arg_as_string(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    nth_arg_as_string(args, 0, context)
}

pub(super) fn nth_arg_as_string(
    args: &[JsValue],
    n: usize,
    context: &mut Context,
) -> JsResult<String> {
    let arg = args.get(n).cloned().unwrap_or_default();
    Ok(arg.to_string(context)?.to_std_string_escaped())
}

// Recovers a NodeId from any Element wrapper by reading the hidden `_nodeId`
// data property the wrapper factory stored. Returns Err for non-Element
// arguments (foreign objects, primitives) — that's the TypeError the DOM
// methods report.
pub(super) fn read_node_id(arg: &JsValue, context: &mut Context) -> JsResult<NodeId> {
    let object = arg.as_object().ok_or_else(|| {
        JsNativeError::typ().with_message("expected an Element-like argument")
    })?;
    let raw = object
        .get(js_string!(NODE_ID_PROP), context)?
        .to_u32(context)?;
    Ok(NodeId::from_raw(raw))
}
