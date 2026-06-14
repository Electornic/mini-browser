// WHATWG-shaped constructor globals and their prototype chains.
//
// Polyfills, frameworks (React, Vue, jQuery), and ad-tech bundles routinely
// touch `Element.prototype.foo`, `HTMLDivElement.prototype.bar`,
// `Event.prototype.preventDefault`, etc. — they assume the standard
// constructor ladder is wired up on the global object even when the
// browser provides only stub semantics. Without these globals, bundle
// initialisation throws `ReferenceError: X is not defined` and the rest
// of the page never gets a chance to render.
//
// What this module does NOT do (yet):
//   - Make our element wrappers *inherit* from these prototypes. Today
//     wrappers are plain JS objects with per-instance `defineProperty`
//     accessors (`element.rs`). A polyfill that patches
//     `Element.prototype.closest = fn` will install the method on this
//     module's empty prototype, but our wrappers won't see it because
//     they're not on that prototype chain. Fixing that is Step 2 of the
//     SPA push (`element` factory refactor).
//   - Provide spec-correct constructor semantics (`new Element()` doesn't
//     actually create anything meaningful here). The constructors are
//     chain anchors, not factories.
//
// What this module DOES do:
//   - Stops `bare Element` / `bare Node` / etc. ReferenceErrors so
//     polyfill bundles can run to completion.
//   - Lays down the inheritance chain (`Text` → `CharacterData` → `Node`
//     → `EventTarget`, etc.) so any polyfill that walks the chain (e.g.
//     `Object.getPrototypeOf(Text.prototype) === CharacterData.prototype`)
//     observes the right relationships.
//   - Owns `Event` / `CustomEvent` too — they used to live in `event.rs`,
//     but they belong with the rest of the constructor zoo.

use rquickjs::{Ctx, Result};

pub(super) fn register_dom_globals(ctx: &Ctx<'_>) -> Result<()> {
    ctx.eval::<(), _>(DOM_GLOBALS_BOOT)
}

const DOM_GLOBALS_BOOT: &str = r#"
(function () {
    // ---- Inheritance helper ------------------------------------------
    //
    // Define `Child` as a no-op constructor whose prototype chains to
    // `parentProto`. `name` is set on the constructor so `Element.name
    // === 'Element'` for the type-introspection patterns polyfills lean
    // on. We guard with `typeof` so repeated invocations (e.g. across
    // unit tests that share a process) don't clobber a constructor a
    // host program may already have installed.
    function defineCtor(name, parentProto) {
        if (typeof globalThis[name] === 'function') {
            return globalThis[name];
        }
        var ctor = function () {};
        if (parentProto) {
            ctor.prototype = Object.create(parentProto);
        }
        ctor.prototype.constructor = ctor;
        try {
            Object.defineProperty(ctor, 'name', { value: name, configurable: true });
        } catch (_) { /* engine refusing rename is fine */ }
        globalThis[name] = ctor;
        return ctor;
    }

    // ---- DOM constructor ladder --------------------------------------
    //
    //  EventTarget
    //    ├── Node
    //    │     ├── Element ── HTMLElement ── HTML*Element (many)
    //    │     ├── CharacterData ── Text / Comment
    //    │     ├── Document ── HTMLDocument
    //    │     └── DocumentFragment ── ShadowRoot
    //    └── Window  (window === globalThis)

    var EventTarget = defineCtor('EventTarget', null);
    // EventTarget's three canonical methods. These are stub stand-ins so
    // `someTarget.addEventListener(...)` from a polyfill doesn't throw;
    // the real listener registry lives in `event.rs` and is reached
    // through the wrapper-level accessors, not through this prototype.
    EventTarget.prototype.addEventListener = function () {};
    EventTarget.prototype.removeEventListener = function () {};
    EventTarget.prototype.dispatchEvent = function () { return true; };

    var Node = defineCtor('Node', EventTarget.prototype);
    // Node.* type-code constants. Polyfills routinely branch on
    // `node.nodeType === Node.ELEMENT_NODE`; without these, the branch
    // compares against `undefined` and silently does nothing.
    Node.ELEMENT_NODE                = 1;
    Node.ATTRIBUTE_NODE              = 2;
    Node.TEXT_NODE                   = 3;
    Node.CDATA_SECTION_NODE          = 4;
    Node.PROCESSING_INSTRUCTION_NODE = 7;
    Node.COMMENT_NODE                = 8;
    Node.DOCUMENT_NODE               = 9;
    Node.DOCUMENT_TYPE_NODE          = 10;
    Node.DOCUMENT_FRAGMENT_NODE      = 11;
    // DocumentPosition flags from compareDocumentPosition. Not used by
    // many bundles but cheap to include and a few popular libs check
    // (`Node.DOCUMENT_POSITION_CONTAINED_BY`).
    Node.DOCUMENT_POSITION_DISCONNECTED            = 0x01;
    Node.DOCUMENT_POSITION_PRECEDING               = 0x02;
    Node.DOCUMENT_POSITION_FOLLOWING               = 0x04;
    Node.DOCUMENT_POSITION_CONTAINS                = 0x08;
    Node.DOCUMENT_POSITION_CONTAINED_BY            = 0x10;
    Node.DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC = 0x20;

    var CharacterData = defineCtor('CharacterData', Node.prototype);
    /* var Text = */ defineCtor('Text', CharacterData.prototype);
    /* var Comment = */ defineCtor('Comment', CharacterData.prototype);
    // DocumentType (`<!DOCTYPE html>` node). Polyfills sweep its
    // prototype together with Element + CharacterData when patching
    // `.remove()` and friends, so it has to exist on the chain even
    // though we don't expose a real doctype to script today.
    /* var DocumentType = */ defineCtor('DocumentType', Node.prototype);

    var Element = defineCtor('Element', Node.prototype);
    var HTMLElement = defineCtor('HTMLElement', Element.prototype);

    // HTML* concrete element constructors. Many polyfills and feature-
    // detection libraries `instanceof` against these. Each one is a
    // chain-only ctor on top of `HTMLElement` — we never `new` them
    // ourselves, but having them be `typeof === 'function'` with the
    // right inheritance is what unblocks the bundles.
    var htmlElementTypes = [
        'HTMLAnchorElement', 'HTMLAreaElement', 'HTMLAudioElement',
        'HTMLBaseElement', 'HTMLBodyElement', 'HTMLBRElement',
        'HTMLButtonElement', 'HTMLCanvasElement', 'HTMLDataElement',
        'HTMLDataListElement', 'HTMLDetailsElement', 'HTMLDialogElement',
        'HTMLDivElement', 'HTMLDListElement', 'HTMLEmbedElement',
        'HTMLFieldSetElement', 'HTMLFormElement', 'HTMLHeadElement',
        'HTMLHeadingElement', 'HTMLHRElement', 'HTMLHtmlElement',
        'HTMLIFrameElement', 'HTMLImageElement', 'HTMLInputElement',
        'HTMLLabelElement', 'HTMLLegendElement', 'HTMLLIElement',
        'HTMLLinkElement', 'HTMLMapElement', 'HTMLMetaElement',
        'HTMLMeterElement', 'HTMLModElement', 'HTMLObjectElement',
        'HTMLOListElement', 'HTMLOptGroupElement', 'HTMLOptionElement',
        'HTMLOutputElement', 'HTMLParagraphElement', 'HTMLParamElement',
        'HTMLPictureElement', 'HTMLPreElement', 'HTMLProgressElement',
        'HTMLQuoteElement', 'HTMLScriptElement', 'HTMLSelectElement',
        'HTMLSlotElement', 'HTMLSourceElement', 'HTMLSpanElement',
        'HTMLStyleElement', 'HTMLTableCaptionElement', 'HTMLTableCellElement',
        'HTMLTableColElement', 'HTMLTableElement', 'HTMLTableRowElement',
        'HTMLTableSectionElement', 'HTMLTemplateElement', 'HTMLTextAreaElement',
        'HTMLTimeElement', 'HTMLTitleElement', 'HTMLTrackElement',
        'HTMLUListElement', 'HTMLUnknownElement', 'HTMLVideoElement',
    ];
    for (var i = 0; i < htmlElementTypes.length; i++) {
        defineCtor(htmlElementTypes[i], HTMLElement.prototype);
    }

    var Document = defineCtor('Document', Node.prototype);
    // Pages still ship under the legacy `HTMLDocument` name. Same
    // prototype contents; the second constructor exists so
    // `instanceof HTMLDocument` works against the same object.
    /* var HTMLDocument = */ defineCtor('HTMLDocument', Document.prototype);
    void Document;
    /* var DocumentFragment = */ defineCtor('DocumentFragment', Node.prototype);

    // Window — used by some scripts as `instanceof Window` even though
    // `window === globalThis`. Chain to EventTarget so the listener
    // methods land naturally. We do NOT swap `globalThis`'s prototype;
    // that's a deep change that breaks unrelated globals.
    /* var WindowCtor = */ defineCtor('Window', EventTarget.prototype);

    // ---- Event ladder ------------------------------------------------
    //
    // Used to live in event.rs; moved here so all constructor globals
    // are in one place. The dispatcher (`__mb_dispatch_chain`) still
    // builds plain-object events — these are anchors for polyfill
    // prototype-chain walks (`r.prototype = window.Event.prototype`).

    if (typeof globalThis.Event !== 'function') {
        globalThis.Event = function Event(type, init) {
            this.type = type || '';
            this.bubbles = !!(init && init.bubbles);
            this.cancelable = !!(init && init.cancelable);
            this.composed = !!(init && init.composed);
            this.defaultPrevented = false;
            this.target = null;
            this.currentTarget = null;
            this.timeStamp = 0;
            this.eventPhase = 0;
        };
        globalThis.Event.prototype.constructor = globalThis.Event;
        globalThis.Event.prototype.preventDefault = function () {
            if (this.cancelable) { this.defaultPrevented = true; }
        };
        globalThis.Event.prototype.stopPropagation = function () {};
        globalThis.Event.prototype.stopImmediatePropagation = function () {};
        globalThis.Event.NONE             = 0;
        globalThis.Event.CAPTURING_PHASE  = 1;
        globalThis.Event.AT_TARGET        = 2;
        globalThis.Event.BUBBLING_PHASE   = 3;
    }
    if (typeof globalThis.CustomEvent !== 'function') {
        globalThis.CustomEvent = function CustomEvent(type, init) {
            globalThis.Event.call(this, type, init);
            this.detail = (init && init.detail !== undefined) ? init.detail : null;
        };
        globalThis.CustomEvent.prototype = Object.create(globalThis.Event.prototype);
        globalThis.CustomEvent.prototype.constructor = globalThis.CustomEvent;
    }
    // MouseEvent, KeyboardEvent, etc. — all chain to Event. Same chain-
    // anchor role as the HTML* constructors above.
    var eventTypes = [
        'UIEvent', 'MouseEvent', 'KeyboardEvent', 'WheelEvent',
        'FocusEvent', 'InputEvent', 'PointerEvent', 'TouchEvent',
        'DragEvent', 'CompositionEvent', 'ClipboardEvent',
        'ProgressEvent', 'StorageEvent', 'PopStateEvent',
        'HashChangeEvent', 'MessageEvent', 'ErrorEvent',
        'BeforeUnloadEvent', 'PageTransitionEvent',
    ];
    for (var j = 0; j < eventTypes.length; j++) {
        defineCtor(eventTypes[j], globalThis.Event.prototype);
    }

    // ---- MutationObserver stub ---------------------------------------
    //
    // The polyfill that died at `r.prototype = window.Event.prototype`
    // is wrapped by an IntersectionObserver/ResizeObserver shim that
    // also probes `typeof MutationObserver`. A constructor that records
    // nothing (no real DOM mutation pipe yet) keeps the shim's
    // `if (typeof MutationObserver !== 'undefined') { ... }` branch
    // satisfied without us pretending to deliver records.
    if (typeof globalThis.MutationObserver !== 'function') {
        globalThis.MutationObserver = function MutationObserver(/* callback */) {};
        globalThis.MutationObserver.prototype.observe    = function () {};
        globalThis.MutationObserver.prototype.disconnect = function () {};
        globalThis.MutationObserver.prototype.takeRecords = function () { return []; };
    }
})();
"#;
