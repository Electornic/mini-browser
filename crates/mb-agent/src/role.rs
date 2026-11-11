// Which elements an agent can act on, what kind they are, and what to call
// them. A deliberately small slice of HTML-AAM / accname — enough for forms,
// links and buttons, not the full ARIA spec (docs/concept.md "role/name 계산").

use std::fmt;

use mb_runtime::dom::NodeId;

use crate::index::DomIndex;
use crate::text::{collapse_whitespace, truncate_chars};

const MAX_NAME_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Link,
    Button,
    Textbox,
    Checkbox,
    Radio,
    Combobox,
    /// Not a native control, but the page made it clickable (`onclick`,
    /// `tabindex`, or an ARIA role we don't model separately).
    Clickable,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Link => "link",
            Role::Button => "button",
            Role::Textbox => "textbox",
            Role::Checkbox => "checkbox",
            Role::Radio => "radio",
            Role::Combobox => "combobox",
            Role::Clickable => "clickable",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `type` values that render as a single-line text field.
fn is_text_input_type(input_type: &str) -> bool {
    matches!(
        input_type,
        "" | "text" | "email" | "search" | "url" | "tel" | "number" | "password"
    )
}

pub(crate) fn input_type(index: &DomIndex<'_>, id: NodeId) -> String {
    index
        .attr(id, "type")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// `None` = not interactive (or hidden by type). Explicit ARIA roles win
/// over the tag, matching how assistive tech reads the page.
pub(crate) fn classify(index: &DomIndex<'_>, id: NodeId) -> Option<Role> {
    let tag = index.tag(id)?;

    if let Some(role) = index.attr(id, "role") {
        match role.trim().to_ascii_lowercase().as_str() {
            "link" => return Some(Role::Link),
            "button" => return Some(Role::Button),
            "textbox" | "searchbox" => return Some(Role::Textbox),
            "checkbox" | "switch" => return Some(Role::Checkbox),
            "radio" => return Some(Role::Radio),
            "combobox" | "listbox" => return Some(Role::Combobox),
            "tab" | "menuitem" | "option" => return Some(Role::Clickable),
            _ => {}
        }
    }

    match tag {
        "a" if index.has_attr(id, "href") => return Some(Role::Link),
        "button" => return Some(Role::Button),
        "textarea" => return Some(Role::Textbox),
        "select" => return Some(Role::Combobox),
        "input" => {
            let input_type = input_type(index, id);
            return match input_type.as_str() {
                "hidden" => None,
                "checkbox" => Some(Role::Checkbox),
                "radio" => Some(Role::Radio),
                "submit" | "button" | "reset" | "image" => Some(Role::Button),
                t if is_text_input_type(t) => Some(Role::Textbox),
                // date/color/range/file/...: not modelled yet, but still
                // something the agent can focus and click.
                _ => Some(Role::Clickable),
            };
        }
        _ => {}
    }

    let tabbable = index
        .attr(id, "tabindex")
        .and_then(|v| v.trim().parse::<i32>().ok())
        .is_some_and(|v| v >= 0);
    if index.has_attr(id, "onclick") || tabbable {
        return Some(Role::Clickable);
    }
    // TODO(G1): listeners added via addEventListener live in the JS runtime,
    // not the DOM; consult its listener registry here.
    None
}

/// Accessible name, in priority order: `aria-label`, `aria-labelledby`,
/// then role-specific sources, then `title`. Empty string if nothing fits.
pub(crate) fn accessible_name(index: &DomIndex<'_>, id: NodeId, role: Role) -> String {
    let name = aria_label(index, id)
        .or_else(|| aria_labelledby(index, id))
        .or_else(|| role_specific_name(index, id, role))
        .or_else(|| non_empty(index.attr(id, "title").unwrap_or("")))
        .unwrap_or_default();
    truncate_chars(&name, MAX_NAME_CHARS)
}

fn non_empty(raw: &str) -> Option<String> {
    let collapsed = collapse_whitespace(raw);
    (!collapsed.is_empty()).then_some(collapsed)
}

fn aria_label(index: &DomIndex<'_>, id: NodeId) -> Option<String> {
    non_empty(index.attr(id, "aria-label")?)
}

fn aria_labelledby(index: &DomIndex<'_>, id: NodeId) -> Option<String> {
    let refs = index.attr(id, "aria-labelledby")?;
    let joined: Vec<String> = refs
        .split_whitespace()
        .filter_map(|r| index.by_id_attr(r))
        .map(|target| index.text_content(target, &|_| false))
        .filter(|text| !text.is_empty())
        .collect();
    non_empty(&joined.join(" "))
}

fn role_specific_name(index: &DomIndex<'_>, id: NodeId, role: Role) -> Option<String> {
    let tag = index.tag(id).unwrap_or("");
    if tag == "input" {
        let input_type = input_type(index, id);
        match input_type.as_str() {
            "submit" | "button" | "reset" => {
                return non_empty(index.attr(id, "value").unwrap_or("")).or_else(
                    || match input_type.as_str() {
                        "submit" => Some("Submit".into()),
                        "reset" => Some("Reset".into()),
                        _ => None,
                    },
                );
            }
            "image" => return non_empty(index.attr(id, "alt").unwrap_or("")),
            _ => {}
        }
    }

    match role {
        Role::Textbox | Role::Checkbox | Role::Radio | Role::Combobox => label_text(index, id)
            .or_else(|| {
                if role == Role::Textbox {
                    non_empty(index.attr(id, "placeholder").unwrap_or(""))
                } else {
                    None
                }
            }),
        Role::Link | Role::Button | Role::Clickable => {
            non_empty(&index.text_content(id, &|_| false)).or_else(|| descendant_img_alt(index, id))
        }
    }
}

/// `<label for="x">` first, then an enclosing `<label>`. The control's own
/// subtree (and other controls inside the label) don't contribute text.
fn label_text(index: &DomIndex<'_>, id: NodeId) -> Option<String> {
    let skip_controls = |node: NodeId| {
        matches!(
            index.tag(node),
            Some("input" | "select" | "textarea" | "button")
        )
    };
    if let Some(label) = index
        .attr(id, "id")
        .and_then(|attr_id| index.label_for(attr_id))
        && let Some(text) = non_empty(&index.text_content(label, &skip_controls))
    {
        return Some(text);
    }
    let label = index.ancestor_with_tag(id, "label")?;
    non_empty(&index.text_content(label, &skip_controls))
}

fn descendant_img_alt(index: &DomIndex<'_>, id: NodeId) -> Option<String> {
    for &child in index.children(id) {
        if index.tag(child) == Some("img")
            && let Some(alt) = non_empty(index.attr(child, "alt").unwrap_or(""))
        {
            return Some(alt);
        }
        if let Some(alt) = descendant_img_alt(index, child) {
            return Some(alt);
        }
    }
    None
}
