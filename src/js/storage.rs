// `localStorage` and `sessionStorage` — minimal in-memory Web Storage
// shims. Real browsers persist `localStorage` across page loads (per
// origin) and clear `sessionStorage` when the tab closes. Our toy keeps
// both lifetimes bound to the JsRuntime instance: every navigation
// creates a fresh runtime, so neither store survives a reload. That
// matches the contract scripts care about within a single page (read
// what you wrote earlier in the same script) without committing to a
// disk-backed store.
//
// The Storage interface implemented here is the spec subset that
// real-world boot scripts actually call: getItem / setItem / removeItem
// / clear / key / length. The exotic indexed access (`storage["key"]`)
// and `Object.keys(storage)` enumeration are skipped — they require
// proxy semantics that aren't worth the complexity for the small set
// of pages that rely on them.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsObject, JsString, JsValue, NativeFunction, js_string,
    object::ObjectInitializer, property::Attribute,
};

use super::util::{first_arg_as_string, nth_arg_as_string};

// Vec<(String, String)> preserves insertion order, which `key(n)` exposes
// to scripts. setItem on an existing key updates in place to keep the
// position stable. The performance is O(n) per op; with the handful of
// entries pages put in localStorage that's well below any noticeable
// cost.
type StorageBacking = Rc<RefCell<Vec<(String, String)>>>;

pub(super) fn register_storage(context: &mut Context) {
    let local = StorageBacking::default();
    let session = StorageBacking::default();

    let local_obj = build_storage_object(context, local);
    let session_obj = build_storage_object(context, session);

    let _ = context.register_global_property(
        js_string!("localStorage"),
        JsValue::from(local_obj),
        Attribute::all(),
    );
    let _ = context.register_global_property(
        js_string!("sessionStorage"),
        JsValue::from(session_obj),
        Attribute::all(),
    );
}

fn build_storage_object(context: &mut Context, store: StorageBacking) -> JsObject {
    let store_for_length = store.clone();
    let length_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            // Unsigned long in the spec; i32 is what JS sees on the
            // other side and there's no realistic risk of overflow at
            // toy scale.
            Ok(JsValue::from(store_for_length.borrow().len() as i32))
        })
    }
    .to_js_function(context.realm());

    let store_for_get = store.clone();
    let get_item = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let key = first_arg_as_string(args, ctx)?;
            let value = store_for_get
                .borrow()
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone());
            match value {
                Some(v) => Ok(JsValue::from(JsString::from(v.as_str()))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let store_for_set = store.clone();
    let set_item = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let key = first_arg_as_string(args, ctx)?;
            // Storage spec ToStrings every value, including primitives
            // like numbers, booleans, null, and undefined ("null",
            // "undefined"). nth_arg_as_string applies the same coercion
            // first_arg_as_string does, just at index 1.
            let value = nth_arg_as_string(args, 1, ctx)?;
            let mut store = store_for_set.borrow_mut();
            if let Some(slot) = store.iter_mut().find(|(k, _)| *k == key) {
                // Update in place so iteration order — and therefore
                // key(n) — stays stable across overwrites.
                slot.1 = value;
            } else {
                store.push((key, value));
            }
            Ok(JsValue::undefined())
        })
    };

    let store_for_remove = store.clone();
    let remove_item = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let key = first_arg_as_string(args, ctx)?;
            store_for_remove.borrow_mut().retain(|(k, _)| *k != key);
            Ok(JsValue::undefined())
        })
    };

    let store_for_clear = store.clone();
    let clear = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            store_for_clear.borrow_mut().clear();
            Ok(JsValue::undefined())
        })
    };

    let store_for_key = store.clone();
    let key_fn = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let idx = match args.first() {
                Some(v) => v.to_u32(ctx)? as usize,
                None => 0usize,
            };
            let store = store_for_key.borrow();
            match store.get(idx) {
                Some((k, _)) => Ok(JsValue::from(JsString::from(k.as_str()))),
                None => Ok(JsValue::null()),
            }
        })
    };

    ObjectInitializer::new(context)
        .accessor(
            js_string!("length"),
            Some(length_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .function(get_item, js_string!("getItem"), 1)
        .function(set_item, js_string!("setItem"), 2)
        .function(remove_item, js_string!("removeItem"), 1)
        .function(clear, js_string!("clear"), 0)
        .function(key_fn, js_string!("key"), 1)
        .build()
}
