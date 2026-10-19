// SPDX-License-Identifier: MPL-2.0

//! merman's engine, configured once for the process.
//!
//! The engine carries the **site config**, which is the host's authority over
//! what a diagram may say about its own rendering. A Mermaid text can carry a
//! `%%{init: …}%%` directive or a YAML front-matter `config:` block, and merman
//! merges either over the site config — except for the keys the site config
//! names in `secure`, which it drops silently, at any depth
//! (`MermaidConfig::source_filtered_overrides`). Koru paints with its own
//! theme and fonts and lays out with the one engine it compiles, so everything
//! a directive could say about the theme, the fonts, the look, the layout
//! engine or HTML labels is in that list, and the answer of `inspect` or of a
//! layout is a function of the diagram's content alone. Naming `secure`
//! **replaces** merman's own hardened list rather than extending it, which is
//! why that list is repeated here first.
//!
//! The values set beside it pin what merman's default leaves to chance. Its
//! pinned Mermaid config names `layout: "elk"`, and with the ELK port not
//! compiled (its licence is not one this workspace allows) merman falls back
//! to dagre without saying so, so dagre is named. `look: "classic"` is the
//! shape set Koru draws. `htmlLabels: false` keeps labels as text, set at the
//! root and under `flowchart` because merman reads both. `securityLevel`
//! stays at merman's `strict`.

use std::sync::OnceLock;

use merman::{Engine, MermaidConfig};
use serde_json::json;

/// Every config key a diagram's own directive or front matter may not set:
/// merman's hardened seven, then what Koru owns.
pub(crate) const SECURE_KEYS: [&str; 18] = [
    // merman's own list. Naming `secure` replaces it, so it is repeated.
    "secure",
    "securityLevel",
    "startOnLoad",
    "maxTextSize",
    "suppressErrorRendering",
    "maxEdges",
    "themeCSS",
    // Koru's: the theme and fonts it paints with, the look and the layout
    // engine it compiles, HTML labels, the per-family sections and wrapping.
    "theme",
    "themeVariables",
    "fontFamily",
    "fontSize",
    "fontWeight",
    "look",
    "layout",
    "htmlLabels",
    "flowchart",
    "sequence",
    "wrap",
];

/// The site config described above, as merman takes it.
pub(crate) fn site_config() -> MermaidConfig {
    MermaidConfig::from_value(json!({
        "secure": SECURE_KEYS,
        "layout": "dagre",
        "look": "classic",
        "htmlLabels": false,
        "flowchart": { "htmlLabels": false },
    }))
}

/// The engine every parse and layout in this crate goes through, built on
/// first use. An `Engine` is read-only once built and is shared by every
/// thread that asks.
pub(crate) fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| Engine::new().with_site_config(site_config()))
}
