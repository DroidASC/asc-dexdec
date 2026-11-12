//! Renaming the classes of a JVM generic signature (JVMS §4.7.9.1): class,
//! method and field signatures alike.
//!
//! A class type nested in a generic one is written `LOuter<TT;>.Inner;`;
//! its binary name is `Outer$Inner`. When the new name of `Outer$Inner`
//! still extends the new name of `Outer`, the dotted form is kept; when it
//! does not (the mapping moved the nested class out), the type is written
//! by its whole new name, which loses only the outer type's arguments.

/// `signature` with each class descriptor `rename` maps replaced; `None`
/// if nothing changes or `signature` does not parse.
pub(super) fn rename<'a>(
    signature: &str,
    rename: impl Fn(&str) -> Option<&'a str>,
) -> Option<String> {
    let mut parser = Parser {
        text: signature,
        at: 0,
        out: String::with_capacity(signature.len()),
        changed: false,
        rename: &rename,
    };
    parser.signature()?;
    parser.changed.then_some(parser.out)
}

struct Parser<'s, 'r, F> {
    text: &'s str,
    at: usize,
    out: String,
    changed: bool,
    rename: &'r F,
}

impl<'s, 'a, F: Fn(&str) -> Option<&'a str>> Parser<'s, '_, F> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn copy(&mut self, length: usize) {
        self.out.push_str(&self.text[self.at..self.at + length]);
        self.at += length;
    }

    /// The text up to (not including) the first of `ends`; every end is ASCII.
    fn until(&mut self, ends: &[u8]) -> Option<&'s str> {
        let rest = &self.text[self.at..];
        let length = rest.bytes().position(|byte| ends.contains(&byte))?;
        self.at += length;
        Some(&rest[..length])
    }

    /// `[TypeParameters] ( (Types) Result {^Throws} | SuperclassSignature {SuperinterfaceSignature} | FieldSignature )`.
    fn signature(&mut self) -> Option<()> {
        if self.peek() == Some(b'<') {
            self.type_parameters()?;
        }
        if self.peek() == Some(b'(') {
            self.copy(1);
            while self.peek()? != b')' {
                self.java_type()?;
            }
            self.copy(1);
            self.java_type()?;
            while self.peek() == Some(b'^') {
                self.copy(1);
                self.java_type()?;
            }
        } else {
            while self.peek().is_some() {
                self.java_type()?;
            }
        }
        (self.at == self.text.len()).then_some(())
    }

    /// `< { Identifier : [ClassBound] { : InterfaceBound } } >`.
    fn type_parameters(&mut self) -> Option<()> {
        self.copy(1);
        while self.peek()? != b'>' {
            let name = self.until(b":>")?;
            self.out.push_str(name);
            while self.peek() == Some(b':') {
                self.copy(1);
                // The class bound may be empty (`T::Ljava/lang/Comparable;`).
                if !matches!(self.peek()?, b':' | b'>') && !self.at_identifier_end() {
                    self.java_type()?;
                }
            }
        }
        self.copy(1);
        Some(())
    }

    /// After an empty class bound, the next parameter's name starts; a
    /// reference type starts with `L`, `T` or `[` followed by more.
    fn at_identifier_end(&self) -> bool {
        // A bound is a reference type signature; anything else is a new
        // identifier, which `type_parameters` reads next.
        !matches!(self.peek(), Some(b'L' | b'T' | b'['))
    }

    fn java_type(&mut self) -> Option<()> {
        match self.peek()? {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V' => {
                self.copy(1);
                Some(())
            }
            b'[' => {
                self.copy(1);
                self.java_type()
            }
            b'T' => {
                let length = self.text[self.at..].find(';')? + 1;
                self.copy(length);
                Some(())
            }
            b'L' => self.class_type(),
            _ => None,
        }
    }

    /// `L Name [TypeArguments] { . Name [TypeArguments] } ;`.
    fn class_type(&mut self) -> Option<()> {
        self.at += 1;
        let mut binary = self.until(b"<.;")?.to_string();
        let mut segments = Vec::new();
        loop {
            let arguments = if self.peek()? == b'<' {
                Some(self.type_arguments()?)
            } else {
                None
            };
            segments.push((binary.clone(), arguments));
            match self.peek()? {
                b'.' => {
                    self.at += 1;
                    let name = self.until(b"<.;")?;
                    binary = format!("{binary}${name}");
                }
                b';' => {
                    self.at += 1;
                    break;
                }
                _ => return None,
            }
        }
        self.write_class(&segments);
        Some(())
    }

    /// `< { * | [+-] ReferenceTypeSignature } >`, rendered on its own.
    fn type_arguments(&mut self) -> Option<String> {
        let outer = std::mem::take(&mut self.out);
        let parsed = (|| {
            self.copy(1);
            while self.peek()? != b'>' {
                match self.peek()? {
                    b'*' => self.copy(1),
                    b'+' | b'-' => {
                        self.copy(1);
                        self.java_type()?;
                    }
                    _ => self.java_type()?,
                }
            }
            self.copy(1);
            Some(())
        })();
        let arguments = std::mem::replace(&mut self.out, outer);
        parsed.map(|()| arguments)
    }

    /// A class type whose segments have the binary names given (outermost
    /// first), each with its type arguments.
    fn write_class(&mut self, segments: &[(String, Option<String>)]) {
        let names = segments
            .iter()
            .map(|(binary, _)| {
                let descriptor = format!("L{binary};");
                match (self.rename)(&descriptor) {
                    Some(new) => {
                        self.changed = true;
                        new[1..new.len() - 1].to_string()
                    }
                    None => binary.clone(),
                }
            })
            .collect::<Vec<_>>();
        // From the innermost segment that cannot be written relative to its
        // outer one on, the type is written by its whole name.
        let start = (1..segments.len())
            .rev()
            .find(|&at| !nested_in(&names[at], &names[at - 1]))
            .unwrap_or(0);
        self.out.push('L');
        self.out.push_str(&names[start]);
        if let Some(arguments) = &segments[start].1 {
            self.out.push_str(arguments);
        }
        for at in start + 1..segments.len() {
            self.out.push('.');
            self.out.push_str(&names[at][names[at - 1].len() + 1..]);
            if let Some(arguments) = &segments[at].1 {
                self.out.push_str(arguments);
            }
        }
        self.out.push(';');
    }
}

/// Whether `inner` is `outer$Something`.
fn nested_in(inner: &str, outer: &str) -> bool {
    inner
        .strip_prefix(outer)
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with('$'))
}
