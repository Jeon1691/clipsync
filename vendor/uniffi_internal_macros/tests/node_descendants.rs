/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
// ClipSync regression contracts for deterministic Node derives.

use std::collections::HashSet;
use uniffi_pipeline::Node;

#[derive(Debug, Node)]
struct Leaf {
    number: u32,
}

#[derive(Debug, Node)]
struct Root {
    first: Leaf,
    repeated_type: Leaf,
    sequence: Vec<Leaf>,
    choice: Choice,
    enabled: bool,
}

#[derive(Debug, Node)]
enum Choice {
    Named { leaf: Leaf, label: String },
    Tuple(Leaf, bool),
    Empty,
}

#[derive(Debug, Node)]
struct Recursive {
    child: Option<Box<Recursive>>,
    leaf: Leaf,
}

#[test]
fn struct_and_enum_descendants_keep_every_field_and_repeated_value() {
    let root = Root {
        first: Leaf { number: 1 },
        repeated_type: Leaf { number: 2 },
        sequence: vec![Leaf { number: 3 }, Leaf { number: 4 }],
        choice: Choice::Named {
            leaf: Leaf { number: 5 },
            label: "named".into(),
        },
        enabled: true,
    };
    assert!(Root::has_descendant_type::<Leaf>(&mut HashSet::new()));
    assert!(Root::has_descendant_type::<Choice>(&mut HashSet::new()));
    assert!(Root::has_descendant_type::<Vec<Leaf>>(&mut HashSet::new()));
    assert!(!Root::has_descendant_type::<f64>(&mut HashSet::new()));
    let mut leaves = Vec::new();
    root.visit::<Leaf>(|leaf| leaves.push(leaf.number));
    assert_eq!(leaves, [1, 2, 3, 4, 5]);
    let mut labels = Vec::new();
    root.try_visit_descendants(&mut |node| {
        if let Some(label) = node.as_any().downcast_ref::<String>() {
            labels.push(label.clone());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(labels, ["named"]);
    assert!(root.enabled);

    let tuple = Choice::Tuple(Leaf { number: 6 }, false);
    let mut tuple_leaves = Vec::new();
    tuple.visit::<Leaf>(|leaf| tuple_leaves.push(leaf.number));
    assert_eq!(tuple_leaves, [6]);
    let mut flags = Vec::new();
    tuple
        .try_visit_descendants(&mut |node| {
            if let Some(flag) = node.as_any().downcast_ref::<bool>() {
                flags.push(*flag);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(flags, [false]);
    assert!(Choice::has_descendant_type::<Leaf>(&mut HashSet::new()));
    let mut empty_leaves = Vec::new();
    Choice::Empty.visit::<Leaf>(|leaf| empty_leaves.push(leaf.number));
    assert!(empty_leaves.is_empty());
}

#[test]
fn recursive_descendant_detection_terminates_and_reaches_later_fields() {
    assert!(Recursive::has_descendant_type::<Leaf>(&mut HashSet::new()));
    assert!(Recursive::has_descendant_type::<Recursive>(
        &mut HashSet::new()
    ));
    assert!(!Recursive::has_descendant_type::<f64>(&mut HashSet::new()));
    let root = Recursive {
        child: Some(Box::new(Recursive {
            child: None,
            leaf: Leaf { number: 7 },
        })),
        leaf: Leaf { number: 8 },
    };
    let mut leaves = Vec::new();
    root.visit::<Leaf>(|leaf| leaves.push(leaf.number));
    assert_eq!(leaves, [7, 8]);
}
