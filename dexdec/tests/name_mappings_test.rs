use std::path::{Path, PathBuf};

use dexdec::{DecompileOptions, Decompiler, NameMappings, SourceLanguage};

fn classes_dex() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testcases/classes.dex")
}

fn renaming() -> NameMappings {
    let mut mappings = NameMappings::new();
    mappings
        .insert_class("LSynchronizedAdvanced;", "Lcom/example/Renamed;")
        .unwrap();
    mappings
        .insert_field("LSynchronizedAdvanced;->value:I", "counter")
        .unwrap();
    mappings
        .insert_method("LSynchronizedAdvanced;->helper(I)I", "assist")
        .unwrap();
    mappings
}

#[test]
fn declarations_and_references_take_the_new_names() {
    let mut decompiler = Decompiler::open_with_names(classes_dex(), &renaming())
        .unwrap()
        .with_options(DecompileOptions::default().with_language(SourceLanguage::Java));
    assert_eq!(
        decompiler.source_class("LSynchronizedAdvanced;"),
        "Lcom/example/Renamed;"
    );
    assert_eq!(
        decompiler.dex_class("Lcom/example/Renamed;"),
        "LSynchronizedAdvanced;"
    );
    assert!(decompiler.class("LSynchronizedAdvanced;").is_err());

    let unit = decompiler.class("Lcom/example/Renamed;").unwrap();
    assert_eq!(unit.class, "Lcom/example/Renamed;");
    assert_eq!(unit.path, Path::new("com/example/Renamed.java"));
    let source = &unit.source;
    assert!(source.contains("package com.example;"), "{source}");
    assert!(source.contains("class Renamed"), "{source}");
    assert!(source.contains("assist("), "{source}");
    assert!(source.contains("counter"), "{source}");
    assert!(!source.contains("helper"), "{source}");
    assert!(!source.contains("SynchronizedAdvanced"), "{source}");

    assert_eq!(
        decompiler.dex_method("Lcom/example/Renamed;", "assist", "(I)I"),
        (
            "LSynchronizedAdvanced;".into(),
            "helper".into(),
            "(I)I".into()
        )
    );
    assert!(decompiler.take_name_warnings().is_empty());
}

#[test]
fn a_rename_onto_another_class_is_dropped_with_a_warning() {
    let mut mappings = NameMappings::new();
    mappings.insert_class("LSimpleIf;", "LHelloWorld;").unwrap();
    let mut decompiler = Decompiler::open_with_names(classes_dex(), &mappings).unwrap();
    assert_eq!(
        decompiler.take_name_warnings(),
        ["class LSimpleIf; keeps its name: LHelloWorld; names another class"]
    );
    assert!(decompiler
        .class("LSimpleIf;")
        .unwrap()
        .source
        .contains("class SimpleIf"));
}

#[test]
fn without_mappings_the_output_is_unchanged() {
    let bytes = std::fs::read(classes_dex()).unwrap();
    let mut plain = Decompiler::from_bytes(&bytes).unwrap();
    let mut mapped = Decompiler::from_bytes_with_names(&bytes, &NameMappings::new()).unwrap();
    for class in ["LSynchronizedAdvanced;", "LHelloWorld;"] {
        assert_eq!(plain.class(class).unwrap(), mapped.class(class).unwrap());
    }
    assert_eq!(mapped.source_class("LHelloWorld;"), "LHelloWorld;");
}
