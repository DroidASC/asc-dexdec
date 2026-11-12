//! One `program/open` session: a dexdec [`Decompiler`] over the program and
//! the protocol units it produces.

use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::Arc;

use asc_decompile_protocol::methods::{ClassUnit, MethodReport, MethodStatus, UnitOutcome};
use dexdec::ir::NullAnalysisObserver;
use dexdec::{DecompileError, DecompileOptions, Decompiler, SourceLanguage, SourceUnit};

use crate::fault::Fault;
use crate::observer::{CancelFlag, ClassObserver, ClassReport, FailedMethod};
use crate::pool;

/// The request was cancelled before its answer was complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cancelled;

/// How the classes of one `decompile/classes` request are generated.
pub(crate) struct ClassRequest<'a> {
    pub(crate) options: DecompileOptions,
    pub(crate) language: SourceLanguage,
    /// Fold nested classes into their outer class (§6.3 `nested`).
    pub(crate) nested: bool,
    pub(crate) cancel: &'a CancelFlag,
    pub(crate) fault: &'a Fault,
}

/// A program the worker keeps open, its classes loaded on demand.
pub(crate) struct Session {
    decompiler: Decompiler,
}

impl Session {
    /// Opens one APK, jar or DEX file; the error is dexdec's reason.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        Ok(Self {
            decompiler: Decompiler::open(path).map_err(|err| err.to_string())?,
        })
    }

    /// The units of `classes`, one per class, in order. Outer classes
    /// generated to answer for nested ones are reused within the batch.
    ///
    /// `on_class` runs after each class, with the number done so far.
    pub(crate) fn units(
        &mut self,
        classes: &[String],
        request: &ClassRequest<'_>,
        mut on_class: impl FnMut(usize, &str),
    ) -> Result<Vec<ClassUnit>, Cancelled> {
        let mut outers = HashMap::new();
        let mut units = Vec::with_capacity(classes.len());
        for (index, class) in classes.iter().enumerate() {
            if request.cancel.is_cancelled() {
                return Err(Cancelled);
            }
            units.push(self.unit(class, request, &mut outers)?);
            on_class(index + 1, class);
        }
        // The last observer holds nothing worth keeping across requests.
        self.decompiler.set_observer(Arc::new(NullAnalysisObserver));
        Ok(units)
    }

    /// The unit of DEX class `class`. With `nested`, a nested class answers
    /// with the unit of the top-level class it is folded into (§6.3), unless
    /// that one fails or leaves it out; then it is decompiled on its own.
    fn unit(
        &mut self,
        class: &str,
        request: &ClassRequest<'_>,
        outers: &mut HashMap<String, Option<ClassUnit>>,
    ) -> Result<ClassUnit, Cancelled> {
        if request.nested
            && let Some(outer) = self.top_level(class)
        {
            if !outers.contains_key(&outer) {
                let unit = match self.generate(&outer, class, request)? {
                    Ok(unit) => Some(unit),
                    Err(reason) => {
                        eprintln!("asc-dexdec-worker: {outer}, holding {class}, failed ({reason})");
                        None
                    }
                };
                outers.insert(outer.clone(), unit);
            }
            match &outers[&outer] {
                Some(unit) if unit.covers(class) => return Ok(unit.clone()),
                Some(_) => eprintln!(
                    "asc-dexdec-worker: {class} is not folded into {outer}; decompiling it on its own"
                ),
                None => {}
            }
        }
        Ok(self
            .generate(class, class, request)?
            .unwrap_or_else(|reason| ClassUnit::new(class, UnitOutcome::Failed { reason })))
    }

    /// The outermost lexical owner of `class`, if it is nested.
    fn top_level(&self, class: &str) -> Option<String> {
        let reader = self.decompiler.reader();
        let mut outer = reader.lexical_parent(class)?;
        // Malformed metadata could make the chain a cycle; it is never deeper
        // than the number of classes.
        for _ in 0..reader.class_count() {
            match reader.lexical_parent(outer) {
                Some(parent) if parent != class => outer = parent,
                _ => break,
            }
        }
        (outer != class).then(|| outer.to_owned())
    }

    /// Generates the unit of `class` on the large-stack pool; the inner
    /// result is the failure reason. A panic fails this class only. Faults
    /// injected for DEX class `requested` fire here.
    fn generate(
        &mut self,
        class: &str,
        requested: &str,
        request: &ClassRequest<'_>,
    ) -> Result<Result<ClassUnit, String>, Cancelled> {
        let observer = Arc::new(ClassObserver::new(request.cancel.clone()));
        self.decompiler.set_observer(observer.clone());
        self.decompiler.set_options(
            request
                .options
                .clone()
                .with_language(request.language)
                .with_nested(request.nested),
        );
        let decompiler = &mut self.decompiler;
        let generated = pool::install(|| {
            pool::catch_panic(|| {
                request.fault.inject(requested, request.cancel);
                // The error as its reason here: dexdec's error type is large.
                decompiler.class(class).map_err(|error| match error {
                    DecompileError::Cancelled(_) => Err(Cancelled),
                    error => Ok(error.to_string()),
                })
            })
        });
        let report = observer.take_report();
        match generated {
            Ok(Ok(unit)) => Ok(Ok(self.protocol_unit(unit, report, request.nested))),
            Ok(Err(reason)) => Ok(Err(reason?)),
            Err(message) => {
                // The panic may have left request-local caches half-built.
                self.decompiler.clear_analysis_scope();
                Ok(Err(format!("generation panicked: {message}")))
            }
        }
    }

    /// `unit` as the protocol unit of its class, with the failed methods and
    /// folded nested classes `report` found.
    fn protocol_unit(&self, unit: SourceUnit, report: ClassReport, nested: bool) -> ClassUnit {
        let methods = report
            .failures
            .iter()
            .map(|failed| method_report(&unit.class, failed))
            .collect::<Vec<_>>();
        // A nested class whose code dexdec went through while generating
        // the unit is folded into its text.
        let nested_classes = if nested {
            report
                .processed
                .into_iter()
                .filter(|other| self.is_nested_in(other, &unit.class))
                .collect()
        } else {
            Vec::new()
        };
        let path = Some(slash_path(&unit.path));
        let text = unit.source;
        let outcome = if methods.is_empty() {
            UnitOutcome::Ok { path, text }
        } else {
            UnitOutcome::Partial { path, text }
        };
        ClassUnit::new(unit.class, outcome)
            .methods(methods)
            .nested_classes(nested_classes)
    }

    /// Whether `class` is lexically nested, at any depth, in `outer`.
    fn is_nested_in(&self, class: &str, outer: &str) -> bool {
        let reader = self.decompiler.reader();
        let mut current = class;
        for _ in 0..reader.class_count() {
            match reader.lexical_parent(current) {
                Some(parent) if parent == outer => return true,
                Some(parent) if parent != class => current = parent,
                _ => return false,
            }
        }
        false
    }
}

/// A failed method as a `methods` entry. Methods of the unit's own class are
/// named `name(Args)Ret`, as §6.3 asks; methods of nested classes folded into
/// the unit keep their owner (`Lpkg/A$B;->name(Args)Ret`).
fn method_report(unit_class: &str, failed: &FailedMethod) -> MethodReport {
    let signature = if failed.class == unit_class {
        format!("{}{}", failed.name, failed.descriptor)
    } else {
        format!("{}->{}{}", failed.class, failed.name, failed.descriptor)
    };
    MethodReport::new(signature, MethodStatus::Failed).reason(Some(failed.reason.clone()))
}

/// `path` with `/` separators whatever the platform.
fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_methods_keep_their_owner() {
        let failed = |class: &str| FailedMethod {
            class: class.into(),
            name: "f".into(),
            descriptor: "()V".into(),
            reason: "semantic recovery: boom".into(),
        };
        let own = method_report("LA;", &failed("LA;"));
        assert_eq!(own.signature, "f()V");
        assert_eq!(own.reason.as_deref(), Some("semantic recovery: boom"));
        assert_eq!(
            method_report("LA;", &failed("LA$B;")).signature,
            "LA$B;->f()V"
        );
    }

    #[test]
    fn paths_use_slashes() {
        assert_eq!(slash_path(Path::new("a/b/C.java")), "a/b/C.java");
    }
}
