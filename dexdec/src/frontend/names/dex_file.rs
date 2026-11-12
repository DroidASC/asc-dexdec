//! [`ProgramNames`] applied to rusty-dex's parsed DEX files.
//!
//! Descriptors and member references live in the type, prototype, field and
//! method tables, which class declarations, member declarations, annotation
//! types and values, `throws` clauses, catch types, local variable types and
//! instruction operands are all read through: renaming the tables and
//! decoding the class directory again renames every one of them. What comes
//! from the string table instead (generic signatures, `InnerClass` simple
//! names, annotation element names) is renamed on each decoded class
//! ([`rename_class`]): once when the program is opened, and again each time
//! a class's members are decoded.

use std::collections::HashMap;

use rayon::prelude::*;
use rusty_dex::dex::classes::{ClassDefItem, DexAnnotation};
use rusty_dex::dex::declarations::{DexMemberDeclaration, DexMemberKind, DexMemberScanner};
use rusty_dex::dex::encoded_value::{EncodedAnnotation, EncodedValue};
use rusty_dex::dex::file::DexFile;

use super::{NameMappings, ProgramClass, ProgramNames};
use crate::frontend::DexResult;

/// Rewrites `dex_files` (as opened, declarations only) under the names of
/// `mappings`. `None`, leaving them untouched, when `mappings` is empty.
pub(crate) fn apply(
    dex_files: &mut [DexFile],
    mappings: &NameMappings,
) -> DexResult<Option<ProgramNames>> {
    if mappings.is_empty() {
        return Ok(None);
    }
    let names = ProgramNames::new(program_classes(dex_files)?, mappings);
    dex_files
        .par_iter_mut()
        .try_for_each(|dex| rename_dex(&names, dex))?;
    Ok(Some(names))
}

/// Every class `dex_files` define, in load order, with the members it
/// declares, under the DEX's names.
fn program_classes(dex_files: &[DexFile]) -> DexResult<Vec<(String, ProgramClass)>> {
    let mut classes = Vec::<(String, ProgramClass)>::new();
    // Where the first definition of each class is in `classes`.
    let mut first = HashMap::<String, usize>::new();
    for dex in dex_files {
        let start = classes.len();
        for class in &dex.classes.items {
            let descriptor = class.get_class_name().clone();
            first.entry(descriptor.clone()).or_insert(classes.len());
            let declaration = ProgramClass {
                superclass: class.get_superclass().map(str::to_string),
                interfaces: class.get_interfaces().to_vec(),
                ..ProgramClass::default()
            };
            classes.push((descriptor, declaration));
        }
        DexMemberScanner::new(dex).scan(&mut |declaration: DexMemberDeclaration<'_>| {
            // A class an earlier DEX file defines already has its members.
            let Some(&at) = first.get(declaration.owner).filter(|&&at| at >= start) else {
                return;
            };
            let class = &mut classes[at].1;
            let members = match declaration.kind {
                DexMemberKind::Field => &mut class.fields,
                DexMemberKind::Method => &mut class.methods,
            };
            members.push(declaration.reference.to_string());
        })?;
    }
    Ok(classes)
}

fn rename_dex(names: &ProgramNames, dex: &mut DexFile) -> DexResult<()> {
    rename_all(&mut dex.types.items, |descriptor| {
        names.descriptors_in(descriptor)
    });
    rename_all(&mut dex.protos.items, |proto| names.descriptors_in(proto));
    rename_all(&mut dex.fields.items, |reference| {
        names.field_reference(reference)
    });
    rename_all(&mut dex.methods.items, |reference| {
        names.method_reference(reference)
    });
    dex.rebuild_declarations()?;
    for class in &mut dex.classes.items {
        rename_class(names, class);
    }
    Ok(())
}

fn rename_all(items: &mut [String], rename: impl Fn(&str) -> Option<String> + Sync) {
    items.par_iter_mut().for_each(|item| {
        if let Some(new) = rename(item) {
            *item = new;
        }
    });
}

/// Renames what `class` (decoded from the renamed tables) read from the
/// string table: its generic signatures, its `InnerClass` simple name and
/// the element names of its annotations. Run once per decoding.
pub(crate) fn rename_class(names: &ProgramNames, class: &mut ClassDefItem) {
    if let Some(signature) = &mut class.signature {
        rename_signature(names, signature);
    }
    // An anonymous class has no name and keeps none.
    if class
        .inner_class
        .as_ref()
        .is_some_and(|inner| inner.name.is_some())
    {
        let dex_class = names.dex_class(class.get_class_name()).to_string();
        let outer = class
            .get_enclosing_class()
            .or_else(|| {
                class
                    .get_enclosing_method()
                    .and_then(|method| method.split_once("->"))
                    .map(|(owner, _)| owner)
            })
            .map(|outer| names.dex_class(outer).to_string());
        if let (Some(simple_name), Some(inner)) = (
            names.nested_simple_name(&dex_class, outer.as_deref()),
            &mut class.inner_class,
        ) {
            inner.name = Some(simple_name);
        }
    }
    rename_annotations(names, &mut class.annotations);
    for field in class.get_fields_mut() {
        if let Some(signature) = &mut field.signature {
            rename_signature(names, signature);
        }
        rename_annotations(names, &mut field.annotations);
    }
    for method in class.get_methods_mut() {
        if let Some(signature) = &mut method.signature {
            rename_signature(names, signature);
        }
        rename_annotations(names, &mut method.annotations);
        for annotations in &mut method.parameter_annotations {
            rename_annotations(names, annotations);
        }
    }
}

fn rename_signature(names: &ProgramNames, signature: &mut String) {
    if let Some(new) = names.signature(signature) {
        *signature = new;
    }
}

fn rename_annotations(names: &ProgramNames, annotations: &mut [DexAnnotation]) {
    for annotation in annotations {
        rename_annotation(names, &mut annotation.annotation);
    }
}

/// Element names are the methods of the annotation interface; its type and
/// the values came renamed from the tables.
fn rename_annotation(names: &ProgramNames, annotation: &mut EncodedAnnotation) {
    let annotation_type = names.dex_class(&annotation.annotation_type).to_string();
    for element in &mut annotation.elements {
        if let Some(new) = names.annotation_element(&annotation_type, &element.name) {
            element.name = new;
        }
        rename_value(names, &mut element.value);
    }
}

fn rename_value(names: &ProgramNames, value: &mut EncodedValue) {
    match value {
        EncodedValue::Array(values) => values
            .iter_mut()
            .for_each(|value| rename_value(names, value)),
        EncodedValue::Annotation(annotation) => rename_annotation(names, annotation),
        _ => {}
    }
}
