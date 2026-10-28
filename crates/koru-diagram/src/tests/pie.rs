// SPDX-License-Identifier: MPL-2.0

//! A pie chart: its slices, legend and title, from merman's layout.

use std::f64::consts::TAU;

use super::{PIE, stub};
use crate::{Cancel, Counts, Kind, Limits, NarrationItem, SceneBody, layout, narrate};

#[test]
fn the_pie_sample_lays_out_two_slices_a_legend_and_a_title() {
    let scene = layout(PIE, &stub(), &Limits::default(), &Cancel::new()).unwrap();
    assert_eq!(scene.kind, Kind::Pie);
    assert_eq!(scene.counts, Counts::Pie { slices: 2 });
    let SceneBody::Pie(pie) = &scene.body else {
        panic!("a pie");
    };
    assert_eq!(pie.slices.len(), 2);
    assert_eq!(pie.slices[0].label, "Dogs");
    assert_eq!(pie.slices[1].label, "Cats");
    // Mermaid rounds the percentages: 386 of 471 is 82%, 85 is 18%.
    assert_eq!(
        pie.slices.iter().map(|s| s.percent).collect::<Vec<_>>(),
        vec![82, 18]
    );
    assert_eq!(pie.slices[0].text.text, "82%");
    // The arcs run clockwise from twelve o'clock and close the circle.
    assert_eq!(pie.slices[0].start, 0.0);
    assert!((pie.slices[0].end - pie.slices[1].start).abs() < 1e-9);
    assert!((pie.slices[1].end - TAU).abs() < 1e-9);
    assert_eq!(
        pie.slices.iter().map(|s| s.series).collect::<Vec<_>>(),
        vec![0, 1]
    );
    // A legend row per slice, its label after its swatch.
    assert_eq!(pie.legend.len(), 2);
    assert_eq!(pie.legend[0].label.text, "Dogs");
    assert!(pie.legend[0].label.rect.x0 > pie.legend[0].swatch.x1);
    assert!(
        pie.legend[1].swatch.y0 > pie.legend[0].swatch.y0,
        "rows go down"
    );
    // The title above the pie.
    let title = pie.title.as_ref().expect("Pets");
    assert_eq!(title.text, "Pets");
    assert!(title.rect.y1 <= pie.centre.y - pie.radius);
    // Everything inside the bounds, which start at the origin.
    assert!(pie.radius > 0.0);
    let b = scene.bounds;
    assert_eq!((b.x0, b.y0), (0.0, 0.0));
    for rect in [title.rect, pie.legend[1].label.rect, pie.legend[1].swatch] {
        assert!(
            rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= b.x1 && rect.y1 <= b.y1,
            "{rect:?} in {b:?}"
        );
    }
    assert!(pie.centre.x - pie.radius >= 0.0 && pie.centre.y - pie.radius >= 0.0);
}

#[test]
fn a_pie_narrates_its_slices_with_their_values_and_shares() {
    let scene = layout(PIE, &stub(), &Limits::default(), &Cancel::new()).unwrap();
    let narration = narrate(&scene);
    assert_eq!(narration.counts, Counts::Pie { slices: 2 });
    assert_eq!(
        narration.items,
        vec![
            NarrationItem::Slice {
                label: "Dogs".to_owned(),
                value: "386".to_owned(),
                percent: 82,
            },
            NarrationItem::Slice {
                label: "Cats".to_owned(),
                value: "85".to_owned(),
                percent: 18,
            },
        ]
    );
}

#[test]
fn a_single_section_is_the_whole_circle() {
    let scene = layout(
        "pie showData\n    \"All\" : 7.5",
        &stub(),
        &Limits::default(),
        &Cancel::new(),
    )
    .unwrap();
    let SceneBody::Pie(pie) = &scene.body else {
        panic!("a pie");
    };
    assert_eq!(pie.slices.len(), 1);
    assert!((pie.slices[0].end - pie.slices[0].start - TAU).abs() < 1e-9);
    assert_eq!(pie.slices[0].percent, 100);
    assert!(pie.title.is_none());
    // `showData` puts the value in the legend's label, as Mermaid does.
    assert!(
        pie.legend[0].label.text.contains("7.5"),
        "{}",
        pie.legend[0].label.text
    );
}
