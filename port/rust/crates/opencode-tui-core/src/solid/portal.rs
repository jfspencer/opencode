//! `<Portal>` (`@opentui/solid`'s `src/elements/extras.ts`): the children render into a box appended to the mount
//! node (the renderer's root by default) instead of where the portal sits; the portal itself leaves nothing in its
//! parent. The box is removed when the portal's owner is disposed.
//!
//! Divergence: the mount is fixed at creation (no reactive `mount` prop; every caller in the TUI uses the root), so
//! one render effect stands for TS's memo of the mount and its insertion effect.
use std::rc::Rc;

use crate::reactive::{create_render_effect, get_owner, on_cleanup, run_with_owner};
use crate::solid::{At, View, insert, remove_node};
use crate::tree::{self, Element};

/// `<Portal mount ref>{children}</Portal>`.
pub fn portal(mount: Option<Element>, on_ref: Option<Rc<dyn Fn(Element)>>, children: Rc<dyn Fn() -> View>) -> View {
    let owner = get_owner();
    let mut content: Option<View> = None;
    create_render_effect(move || {
        let Some(el) = mount.or_else(tree::root) else { return };
        let view = content.get_or_insert_with(|| run_with_owner(owner.clone(), || children())).clone();
        let container = crate::elements::box_();
        insert(container, view, At::Only);
        el.add(container, None);
        on_cleanup(move || remove_node(el, container));
        if let Some(on_ref) = &on_ref {
            on_ref(container);
        }
    });
    View::Empty
}
