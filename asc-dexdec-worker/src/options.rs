//! The `initialize` `options` table and `optionsHash`.

use std::time::UNIX_EPOCH;

use asc_decompile_protocol::{ErrorCode, ErrorObject};
use dexdec::{DecompileOptions, JavaDecompilerConfig, KotlinDecompilerConfig};
use serde_json::{Map, Value};

use crate::VERSION;
use crate::server::BACKEND_NAME;

/// Options asc can set for every request of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerOptions {
    /// Indentation of the generated source, Java and Kotlin alike.
    indent: String,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            indent: JavaDecompilerConfig::default().indent,
        }
    }
}

impl WorkerOptions {
    /// Reads the `initialize` `options` table. Unknown keys are refused, so a
    /// misspelt option is noticed instead of silently ignored.
    pub(crate) fn from_table(table: &Map<String, Value>) -> Result<Self, ErrorObject> {
        let mut options = Self::default();
        for (key, value) in table {
            match key.as_str() {
                "indent" => {
                    let indent = value
                        .as_str()
                        .ok_or_else(|| invalid_params("option `indent` must be a string"))?;
                    options.indent = indent.to_owned();
                }
                key => {
                    return Err(invalid_params(format!(
                        "unknown option `{key}` (known: indent)"
                    )));
                }
            }
        }
        Ok(options)
    }

    /// dexdec's options for these settings; requests set language and nesting.
    pub(crate) fn to_decompile_options(&self) -> DecompileOptions {
        let mut java = JavaDecompilerConfig::default();
        java.indent.clone_from(&self.indent);
        let mut kotlin = KotlinDecompilerConfig::default();
        kotlin.indent.clone_from(&self.indent);
        DecompileOptions::default()
            .with_java(java)
            .with_kotlin(kotlin)
    }

    /// `optionsHash`: the effective options and this build of the worker.
    ///
    /// The build is the worker version plus the size and modification time of
    /// the running executable: the version does not change between commits,
    /// and a rebuilt worker must never replay the output of the old one from
    /// asc's cache. A rebuild that changes nothing costs one cold cache.
    pub(crate) fn hash(&self) -> String {
        let mut hash = Fnv::default();
        hash.write(BACKEND_NAME.as_bytes());
        hash.write(VERSION.as_bytes());
        hash.write(executable_stamp().as_bytes());
        hash.write(b"indent");
        hash.write(self.indent.as_bytes());
        format!("{:016x}", hash.0)
    }
}

/// `<size>.<mtime ns>` of the running executable, empty if unknown.
fn executable_stamp() -> String {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|metadata| {
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            format!("{}.{mtime}", metadata.len())
        })
        .unwrap_or_default()
}

/// 64-bit FNV-1a; each write is terminated so fields cannot run together.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes.iter().chain(&[0]) {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

pub(crate) fn invalid_params(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(ErrorCode::INVALID_PARAMS, message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn table(value: Value) -> Map<String, Value> {
        let Value::Object(table) = value else {
            panic!("{value} is not an object")
        };
        table
    }

    #[test]
    fn options_are_checked_and_hashed() {
        let tabbed = WorkerOptions::from_table(&table(json!({"indent": "\t"}))).unwrap();
        assert_eq!(tabbed.to_decompile_options().java.indent, "\t");
        assert_eq!(tabbed.to_decompile_options().kotlin.indent, "\t");
        assert_ne!(tabbed.hash(), WorkerOptions::default().hash());
        assert_eq!(
            WorkerOptions::default().hash(),
            WorkerOptions::from_table(&Map::new()).unwrap().hash()
        );

        let unknown = WorkerOptions::from_table(&table(json!({"indnet": "\t"}))).unwrap_err();
        assert_eq!(unknown.code, ErrorCode::INVALID_PARAMS);
        let wrong_type = WorkerOptions::from_table(&table(json!({"indent": 4}))).unwrap_err();
        assert_eq!(wrong_type.code, ErrorCode::INVALID_PARAMS);
    }
}
