//! What dexdec reports while it decompiles one class, through its public
//! [`AnalysisObserver`] hook: the methods it gave up on, the classes whose
//! code it went through, and whether to stop.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dexdec::ir::{AnalysisEvent, AnalysisEventKind, AnalysisObserver};

/// A request's cancellation, raised by `notifications/cancelled`.
#[derive(Debug, Clone, Default)]
pub(crate) struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A method dexdec printed as a failure stub.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FailedMethod {
    /// Descriptor of the declaring class.
    pub(crate) class: String,
    pub(crate) name: String,
    /// `(Args)Ret`.
    pub(crate) descriptor: String,
    /// `<stage>: <cause>`.
    pub(crate) reason: String,
}

/// What one class's generation reported.
#[derive(Debug, Default)]
pub(crate) struct ClassReport {
    /// Each failed method once, in the order first reported.
    pub(crate) failures: Vec<FailedMethod>,
    /// Descriptors of every class whose declaration or methods were processed.
    pub(crate) processed: BTreeSet<String>,
}

/// Observes one class's generation; dexdec checks [`is_cancelled`](AnalysisObserver::is_cancelled)
/// between methods and pipeline stages.
#[derive(Debug)]
pub(crate) struct ClassObserver {
    cancel: CancelFlag,
    report: Mutex<ClassReport>,
}

impl ClassObserver {
    pub(crate) fn new(cancel: CancelFlag) -> Self {
        Self {
            cancel,
            report: Mutex::default(),
        }
    }

    pub(crate) fn take_report(&self) -> ClassReport {
        std::mem::take(&mut *self.report.lock().expect("class report mutex poisoned"))
    }
}

impl AnalysisObserver for ClassObserver {
    fn is_enabled(&self, kind: AnalysisEventKind) -> bool {
        matches!(
            kind,
            AnalysisEventKind::ClassStage
                | AnalysisEventKind::MethodStage
                | AnalysisEventKind::MethodFailure
        )
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn observe(&self, event: AnalysisEvent<'_>) {
        let mut report = self.report.lock().expect("class report mutex poisoned");
        match event {
            AnalysisEvent::ClassStage { class, .. } | AnalysisEvent::MethodStage { class, .. } => {
                if !report.processed.contains(class) {
                    report.processed.insert(type_descriptor(class));
                }
            }
            AnalysisEvent::MethodFailure {
                class,
                method,
                descriptor,
                stage,
                reason,
            } => {
                let class = type_descriptor(class);
                let known = report.failures.iter().any(|failed| {
                    failed.class == class
                        && failed.name == method
                        && failed.descriptor == descriptor
                });
                if !known {
                    report.failures.push(FailedMethod {
                        class: class.clone(),
                        name: method.to_owned(),
                        descriptor: descriptor.to_owned(),
                        reason: format!("{stage}: {reason}"),
                    });
                }
                report.processed.insert(class);
            }
            // Not enabled above.
            _ => {}
        }
    }
}

/// `class` as a type descriptor. dexdec's events mostly carry descriptors
/// (`Lpkg/A$B;`) but fall back to binary names (`pkg/A$B`, `pkg.A$B`).
pub(crate) fn type_descriptor(class: &str) -> String {
    if class.starts_with('L') && class.ends_with(';') {
        class.to_owned()
    } else {
        format!("L{};", class.replace('.', "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_names_become_descriptors() {
        assert_eq!(type_descriptor("Lpkg/A$B;"), "Lpkg/A$B;");
        assert_eq!(type_descriptor("pkg/A$B"), "Lpkg/A$B;");
        assert_eq!(type_descriptor("pkg.A$B"), "Lpkg/A$B;");
    }

    #[test]
    fn failures_are_reported_once() {
        let observer = ClassObserver::new(CancelFlag::default());
        for _ in 0..2 {
            observer.observe(AnalysisEvent::MethodFailure {
                class: "LA;",
                method: "f",
                descriptor: "()V",
                stage: "semantic recovery",
                reason: "boom",
            });
        }
        let report = observer.take_report();
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "semantic recovery: boom");
        assert!(report.processed.contains("LA;"));
        assert!(
            observer.take_report().failures.is_empty(),
            "taking the report resets it"
        );
    }
}
