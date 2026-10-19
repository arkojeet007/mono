// SPDX-License-Identifier: MPL-2.0

//! The site config holds: a diagram's own directive or front matter cannot
//! change what Koru owns, and does not change `inspect`'s answer.

use merman::{Engine, MermaidConfig};

use super::{FLOWCHART_SHOWCASE, FRONT_MATTER};
use crate::engine::{SECURE_KEYS, engine};
use crate::{Limits, inspect};

const PLAIN: &str = "flowchart LR\n    A --> B\n";

/// Everything a directive could try to take over, in one.
const DIRECTIVE: &str = concat!(
    "%%{init: {\"theme\": \"forest\", \"layout\": \"elk\", \"look\": \"handDrawn\", ",
    "\"fontFamily\": \"Comic Sans MS\", \"fontSize\": 40, \"htmlLabels\": true, ",
    "\"securityLevel\": \"loose\", \"themeVariables\": {\"primaryColor\": \"#ff0000\"}, ",
    "\"flowchart\": {\"htmlLabels\": true, \"curve\": \"linear\"}}}%%\n",
    "flowchart LR\n    A --> B\n",
);

/// The config merman will parse and lay `text` out with.
fn effective(engine: &Engine, text: &str) -> MermaidConfig {
    engine
        .parse_metadata_sync(text)
        .expect("the header parses")
        .effective_config
}

#[test]
fn a_directive_or_front_matter_cannot_change_what_koru_owns() {
    let koru = engine();
    let baseline = effective(koru, PLAIN);
    // The theme is merman's own default for the family — Mermaid 12 gives a
    // flowchart `redux-color` unless the site says otherwise, and Koru paints
    // with its own theme, so it does not — but never the diagram's.
    assert_ne!(baseline.get_str("theme"), Some("forest"));
    for text in [PLAIN, DIRECTIVE, FRONT_MATTER, FLOWCHART_SHOWCASE] {
        let config = effective(koru, text);
        assert_eq!(
            config.get_str("theme"),
            baseline.get_str("theme"),
            "{text:?}"
        );
        assert_eq!(config.get_str("layout"), Some("dagre"), "{text:?}");
        assert_eq!(config.get_str("look"), Some("classic"), "{text:?}");
        assert_eq!(config.get_bool("htmlLabels"), Some(false), "{text:?}");
        assert_eq!(
            config.get_bool("flowchart.htmlLabels"),
            Some(false),
            "{text:?}"
        );
        assert_eq!(config.get_str("securityLevel"), Some("strict"), "{text:?}");
        // The diagram's `flowchart` section is dropped whole, and the fonts
        // and theme variables stay the site's.
        for key in [
            "flowchart.curve",
            "fontFamily",
            "fontSize",
            "themeVariables.primaryColor",
        ] {
            assert_eq!(
                config
                    .as_value()
                    .pointer(&format!("/{}", key.replace('.', "/"))),
                baseline
                    .as_value()
                    .pointer(&format!("/{}", key.replace('.', "/"))),
                "{key} in {text:?}"
            );
        }
        let secure: Vec<&str> = config.as_value()["secure"]
            .as_array()
            .expect("the secure list")
            .iter()
            .map(|key| key.as_str().expect("a key"))
            .collect();
        assert_eq!(secure, SECURE_KEYS, "{text:?}");
    }

    // The control: merman's own default lets the same directive through, so
    // the fixture does exercise the list.
    let unguarded = effective(&Engine::new(), DIRECTIVE);
    assert_eq!(unguarded.get_str("theme"), Some("forest"));
    assert_eq!(unguarded.get_str("look"), Some("handDrawn"));
    assert_eq!(unguarded.get_str("flowchart.curve"), Some("linear"));
    // …but not what merman itself hardens.
    assert_eq!(unguarded.get_str("securityLevel"), Some("strict"));
}

#[test]
fn a_directive_does_not_change_the_answer() {
    let limits = Limits::default();
    assert_eq!(inspect(DIRECTIVE, &limits), inspect(PLAIN, &limits));
    assert_eq!(
        inspect(FRONT_MATTER, &limits),
        inspect(FLOWCHART_SHOWCASE, &limits)
    );
}
