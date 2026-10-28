// SPDX-License-Identifier: MPL-2.0

//! What a [`Kind::Callout`](crate::Kind::Callout) carries beyond its children.
//!
//! A callout is a box: a container like a list block, with no text of its
//! own, whose children are the lines inside it — paragraphs, and lists with
//! their items. The box's look lives on the container: the **icon** in
//! [`NodeMeta::icon`] (one emoji; absent means [`DEFAULT_ICON`]) and the
//! **tint** in [`NodeMeta::colour`] (an [`AccentColor`] token; absent means
//! the default tint). Both are keys every build already reads, so neither
//! needed a schema step of its own beyond the one `Kind::Callout` took.
//!
//! `WorkspaceDoc::wrap_in_callout_many` puts lines into a new box and
//! `dissolve_callout` takes the box away and keeps them; Enter inside is an
//! ordinary paragraph split, and the editor's ways out are Enter on the empty
//! last line and Backspace at the start of the first.

use crate::attributes::AccentColor;
use crate::node::NodeMeta;

/// The icon a callout shows when none is set.
pub const DEFAULT_ICON: &str = "💡";

/// The icon `meta` draws, with the default filled in.
pub fn icon(meta: &NodeMeta) -> &str {
    icon_of(meta.icon.as_deref())
}

/// The tint `meta` draws, or `None` for the default tint. A token from a newer
/// build's palette reads as the default rather than as an error.
pub fn tint(meta: &NodeMeta) -> Option<AccentColor> {
    tint_of(meta.colour.as_deref())
}

/// [`icon`] over the stored value alone, for a row that copied it out of the
/// node.
pub fn icon_of(icon: Option<&str>) -> &str {
    icon.filter(|icon| !icon.is_empty()).unwrap_or(DEFAULT_ICON)
}

/// [`tint`] over the stored value alone.
pub fn tint_of(color: Option<&str>) -> Option<AccentColor> {
    color.and_then(AccentColor::parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(icon: Option<&str>, color: Option<&str>) -> NodeMeta {
        NodeMeta {
            icon: icon.map(str::to_owned),
            colour: color.map(str::to_owned),
            ..NodeMeta::default()
        }
    }

    #[test]
    fn absent_means_the_default_and_an_unknown_token_does_too() {
        assert_eq!(icon(&meta(None, None)), DEFAULT_ICON);
        assert_eq!(icon(&meta(Some(""), None)), DEFAULT_ICON);
        assert_eq!(icon(&meta(Some("⚠️"), None)), "⚠️");
        assert_eq!(tint(&meta(None, None)), None);
        assert_eq!(tint(&meta(None, Some("red"))), Some(AccentColor::Red));
        // A token from a newer palette is the default tint, not an error.
        assert_eq!(tint(&meta(None, Some("ultraviolet"))), None);
    }
}
