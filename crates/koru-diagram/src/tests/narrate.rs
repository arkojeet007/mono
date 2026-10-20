// SPDX-License-Identifier: MPL-2.0

//! `narrate` on the showcases: the arrows in source order, by label.

use super::{FLOWCHART_SHOWCASE, SEQUENCE_SHOWCASE, stub};
use crate::{Cancel, Counts, Limits, NarrationItem, layout, narrate};

fn arrow(from: &str, label: Option<&str>, to: &str) -> NarrationItem {
    NarrationItem::Arrow {
        from: from.to_owned(),
        label: label.map(str::to_owned),
        to: to.to_owned(),
    }
}

#[test]
fn the_flowchart_showcase_narrates_its_twelve_arrows_by_label() {
    let scene = layout(
        FLOWCHART_SHOWCASE,
        &stub(),
        &Limits::default(),
        &Cancel::new(),
    )
    .unwrap();
    let narration = narrate(&scene);
    assert_eq!(narration.title.as_deref(), Some("Checkout flow"));
    assert!(
        narration
            .description
            .as_deref()
            .unwrap()
            .starts_with("How an order")
    );
    assert_eq!(
        narration.counts,
        Counts::Flowchart {
            nodes: 10,
            edges: 12,
            groups: 2,
        }
    );
    assert_eq!(
        narration.items,
        [
            arrow("Cart", None, "Valid order?"),
            arrow("Valid order?", Some("no"), "Fix the order"),
            arrow("Fix the order", None, "Cart"),
            arrow("Valid order?", Some("yes"), "Charge card"),
            arrow("Charge card", None, "Payment gateway"),
            arrow("Payment gateway", Some("declined"), "Retry"),
            arrow("Retry", None, "Charge card"),
            arrow("Payment gateway", Some("timeout"), "Second provider (EU)"),
            arrow("Payment gateway", Some("approved"), "Order confirmed"),
            arrow("Second provider (EU)", None, "Order confirmed"),
            arrow("Order confirmed", None, "Email & push — café ☕"),
            // An arrow from a group names the group's title.
            arrow("Payment service", None, "Audit log"),
        ]
    );
}

#[test]
fn the_sequence_seam_narrates_its_counts_and_no_messages_yet() {
    let scene = layout(
        SEQUENCE_SHOWCASE,
        &stub(),
        &Limits::default(),
        &Cancel::new(),
    )
    .unwrap();
    let narration = narrate(&scene);
    assert_eq!(narration.title.as_deref(), Some("Sign-in with a passkey"));
    assert_eq!(
        narration.counts,
        Counts::Sequence {
            participants: 4,
            messages: 12,
            notes: 2,
        }
    );
    assert!(
        narration.items.is_empty(),
        "the sequence builder fills the messages"
    );
}
