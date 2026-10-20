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

fn message(from: &str, text: &str, to: &str) -> NarrationItem {
    NarrationItem::Message {
        from: from.to_owned(),
        text: text.to_owned(),
        to: to.to_owned(),
    }
}

#[test]
fn the_sequence_showcase_narrates_its_twelve_messages_by_name() {
    let scene = layout(
        SEQUENCE_SHOWCASE,
        &stub(),
        &Limits::default(),
        &Cancel::new(),
    )
    .unwrap();
    let narration = narrate(&scene);
    assert_eq!(narration.title.as_deref(), Some("Sign-in with a passkey"));
    assert!(
        narration
            .description
            .as_deref()
            .unwrap()
            .starts_with("The app asks")
    );
    assert_eq!(
        narration.counts,
        Counts::Sequence {
            participants: 4,
            messages: 12,
            notes: 2,
        }
    );
    // Participants by the name a person sees, never by id: `P` is *Person*,
    // `App` is *Koru app*, `DB` is *Session store*.
    assert_eq!(
        narration.items,
        [
            message("Person", "Sign in", "Koru app"),
            message("Koru app", "Request challenge", "Server"),
            message("Server", "Challenge", "Koru app"),
            message("Koru app", "Ask for passkey", "Person"),
            message("Person", "Touch sensor", "Koru app"),
            message("Koru app", "Signed challenge", "Server"),
            message("Server", "Create session", "Session store"),
            message("Session store", "Session id", "Server"),
            message("Server", "Session token", "Koru app"),
            message("Server", "Rejected", "Koru app"),
            message("Koru app", "Register device", "Server"),
            message("Koru app", "Signed in", "Person"),
        ]
    );
}
