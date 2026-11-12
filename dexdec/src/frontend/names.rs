//! User-supplied names (an R8 / ProGuard `mapping.txt`, translated by asc)
//! applied to a whole program before anything else reads it.
//!
//! The renaming is applied to the parsed DEX files right after they are
//! opened (see [`dex_file`]): every class descriptor, member reference,
//! generic signature and annotation the frontend reads already carries the
//! new names, so declarations and every reference to them (imports,
//! packages, nested names, member accesses, invocations) agree by
//! construction, and the backends escape illegal identifiers the way they
//! always do. String constants keep their text. Without mappings nothing is
//! rewritten.
//!
//! Member mappings are keyed by the class that declares the member. A
//! reference through a subclass or an interface (`invoke-virtual
//! LSub;->a()V` for a method `LBase;` declares) is resolved to its
//! declaring class within the program first; R8 names overriding methods
//! alike, so the declaration decides for all of them.
//!
//! A mapping that would make two things indistinguishable is dropped
//! instead of producing a wrong reference: two classes mapped to one name
//! (or a program class to the name of a platform class, such as R8's
//! desugared `java.lang.Record`), or two members of a class mapped to the same Java name (fields by name,
//! methods by name and parameters). Each dropped entry leaves a warning.

pub(crate) mod dex_file;
mod signature;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

/// Renames to apply to a program, in asc's member notation: keys are the
/// names in the DEX (obfuscated, if the program is), values the names the
/// source shows. Classes map to whole descriptors (the package may change;
/// a nested class maps to its full binary name, `Lpkg/Outer$Inner;`),
/// fields and methods to simple names. Constructors are never renamed.
///
/// The empty value renames nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct NameMappings {
    classes: BTreeMap<String, String>,
    fields: BTreeMap<String, String>,
    methods: BTreeMap<String, String>,
}

/// A mapping entry [`NameMappings`] refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NameMappingError {
    /// The key or value of a class entry is not a class descriptor.
    Class {
        /// The DEX descriptor.
        descriptor: String,
        /// The new descriptor.
        new_descriptor: String,
    },
    /// The key is not `Lowner;->name:Type`, or the new name is not one name.
    Field {
        /// The field signature.
        signature: String,
        /// The new name.
        new_name: String,
    },
    /// The key is not `Lowner;->name(args)Ret` of a method other than a
    /// constructor, or the new name is not one name.
    Method {
        /// The method signature.
        signature: String,
        /// The new name.
        new_name: String,
    },
}

impl fmt::Display for NameMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Class {
                descriptor,
                new_descriptor,
            } => {
                write!(f, "class mapping {descriptor:?} -> {new_descriptor:?}: want Lpkg/Name; on both sides")
            }
            Self::Field {
                signature,
                new_name,
            } => {
                write!(f, "field mapping {signature:?} -> {new_name:?}: want Lowner;->name:Type and a simple name")
            }
            Self::Method {
                signature,
                new_name,
            } => {
                write!(f, "method mapping {signature:?} -> {new_name:?}: want Lowner;->name(args)Ret (no constructor) and a simple name")
            }
        }
    }
}

impl std::error::Error for NameMappingError {}

impl NameMappings {
    /// No renames.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing is renamed.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.fields.is_empty() && self.methods.is_empty()
    }

    /// Renames the class `descriptor` (`Lz/c;`) to `new_descriptor`
    /// (`Lcom/example/Cart;`).
    ///
    /// # Errors
    ///
    /// [`NameMappingError::Class`] when either side is not a class descriptor.
    pub fn insert_class(
        &mut self,
        descriptor: impl Into<String>,
        new_descriptor: impl Into<String>,
    ) -> Result<(), NameMappingError> {
        let (descriptor, new_descriptor) = (descriptor.into(), new_descriptor.into());
        if !(is_class_descriptor(&descriptor) && is_class_descriptor(&new_descriptor)) {
            return Err(NameMappingError::Class {
                descriptor,
                new_descriptor,
            });
        }
        self.classes.insert(descriptor, new_descriptor);
        Ok(())
    }

    /// Renames the field `Lowner;->name:Type` to `new_name`.
    ///
    /// # Errors
    ///
    /// [`NameMappingError::Field`] when `signature` is not a field signature
    /// or `new_name` is not a simple name.
    pub fn insert_field(
        &mut self,
        signature: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Result<(), NameMappingError> {
        let (signature, new_name) = (signature.into(), new_name.into());
        let valid = MemberRef::split(&signature)
            .and_then(|member| member.tail.split_once(':'))
            .is_some_and(|(name, ty)| is_simple_name(name) && is_type_list(ty) && !ty.is_empty());
        if !(valid && is_simple_name(&new_name)) {
            return Err(NameMappingError::Field {
                signature,
                new_name,
            });
        }
        self.fields.insert(signature, new_name);
        Ok(())
    }

    /// Renames the method `Lowner;->name(args)Ret` to `new_name`.
    ///
    /// # Errors
    ///
    /// [`NameMappingError::Method`] when `signature` is not the signature of
    /// a method other than a constructor, or `new_name` is not a simple name.
    pub fn insert_method(
        &mut self,
        signature: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Result<(), NameMappingError> {
        let (signature, new_name) = (signature.into(), new_name.into());
        let valid = MemberRef::split(&signature).is_some_and(|member| {
            member.tail.split_once('(').is_some_and(|(name, proto)| {
                is_simple_name(name)
                    && proto
                        .split_once(')')
                        .is_some_and(|(params, ret)| is_type_list(params) && !ret.is_empty())
            })
        });
        if !(valid && is_simple_name(&new_name)) {
            return Err(NameMappingError::Method {
                signature,
                new_name,
            });
        }
        self.methods.insert(signature, new_name);
        Ok(())
    }
}

/// `Lpkg/Name;`: a class descriptor whose segments are non-empty names.
fn is_class_descriptor(descriptor: &str) -> bool {
    descriptor
        .strip_prefix('L')
        .and_then(|inner| inner.strip_suffix(';'))
        .is_some_and(|inner| inner.split('/').all(is_simple_name))
}

/// A name free of the characters that delimit descriptors and signatures,
/// and of whitespace; the backends escape whatever else Java rejects.
fn is_simple_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', ';', '.', '[', '(', ')', ':', '<', '>'])
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Zero or more type descriptors, back to back.
fn is_type_list(mut types: &str) -> bool {
    while !types.is_empty() {
        let element = types.trim_start_matches('[');
        let Some(first) = element.chars().next() else {
            return false;
        };
        let length = match first {
            'Z' | 'B' | 'S' | 'C' | 'I' | 'J' | 'F' | 'D' | 'V' => 1,
            'L' => match element.find(';') {
                Some(end) if is_class_descriptor(&element[..=end]) => end + 1,
                _ => return false,
            },
            _ => return false,
        };
        types = &element[length..];
    }
    true
}

/// `Lowner;->tail`, split at the arrow.
#[derive(Debug, Clone, Copy)]
struct MemberRef<'a> {
    owner: &'a str,
    /// `name:Type` or `name(params)ret`.
    tail: &'a str,
}

impl<'a> MemberRef<'a> {
    fn split(reference: &'a str) -> Option<Self> {
        let (owner, tail) = reference.split_once("->")?;
        is_class_descriptor(owner).then_some(Self { owner, tail })
    }
}

/// Which kind of member a reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    Field,
    Method,
}

/// A class the program defines, under its DEX names: what it extends and
/// the members it declares.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProgramClass {
    pub(crate) superclass: Option<String>,
    pub(crate) interfaces: Vec<String>,
    /// `Lowner;->name:Type` of every field.
    pub(crate) fields: Vec<String>,
    /// `Lowner;->name(params)ret` of every method.
    pub(crate) methods: Vec<String>,
}

/// The members one class declares, keyed by their tail (`name:Type`,
/// `name(params)ret`), each with its new name if it gets one.
#[derive(Debug, Default)]
struct DeclaredMembers {
    fields: HashMap<String, Option<String>>,
    methods: HashMap<String, Option<String>>,
}

impl DeclaredMembers {
    fn get(&self, kind: MemberKind) -> &HashMap<String, Option<String>> {
        match kind {
            MemberKind::Field => &self.fields,
            MemberKind::Method => &self.methods,
        }
    }
}

/// [`NameMappings`] resolved against one program: which classes really get
/// a new name, and (lazily, per class) which members do.
pub(crate) struct ProgramNames {
    /// Program classes by DEX descriptor; the first definition wins, as
    /// for the runtime.
    classes: HashMap<String, ProgramClass>,
    /// DEX descriptor → new descriptor, for each renamed class.
    renamed: HashMap<String, String>,
    /// New descriptor → DEX descriptor, the inverse of `renamed`.
    originals: HashMap<String, String>,
    fields: BTreeMap<String, String>,
    methods: BTreeMap<String, String>,
    members: RwLock<HashMap<String, Arc<DeclaredMembers>>>,
    warnings: Mutex<Vec<String>>,
}

impl fmt::Debug for ProgramNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgramNames")
            .field("renamed", &self.renamed.len())
            .finish_non_exhaustive()
    }
}

impl ProgramNames {
    /// `mappings` resolved against the program `classes` (DEX descriptor
    /// and declaration, in load order; the first definition of a
    /// descriptor wins, as for the runtime).
    pub(crate) fn new(
        classes: impl IntoIterator<Item = (String, ProgramClass)>,
        mappings: &NameMappings,
    ) -> Self {
        let mut program = HashMap::new();
        for (descriptor, class) in classes {
            program.entry(descriptor).or_insert(class);
        }
        let mut warnings = Vec::new();
        let renamed = Self::class_renames(&program, &mappings.classes, &mut warnings);
        let originals = renamed
            .iter()
            .map(|(old, new)| (new.clone(), old.clone()))
            .collect();
        Self {
            classes: program,
            renamed,
            originals,
            fields: mappings.fields.clone(),
            methods: mappings.methods.clone(),
            members: RwLock::default(),
            warnings: Mutex::new(warnings),
        }
    }

    /// The classes that get a new name: each mapped class, and each
    /// unmapped program class nested (by binary name) in a renamed one,
    /// which moves with it. A rename that would give two classes one name,
    /// or a program class the name of a platform class, is dropped, with a
    /// warning, until no two names clash.
    fn class_renames(
        classes: &HashMap<String, ProgramClass>,
        mapped: &BTreeMap<String, String>,
        warnings: &mut Vec<String>,
    ) -> HashMap<String, String> {
        let platform = crate::platform_symbols::default_platform_symbols().ok();
        let is_platform = |descriptor: &str| {
            platform
                .as_ref()
                .is_some_and(|platform| platform.class(descriptor).is_some())
        };
        let mut dropped = HashSet::<String>::new();
        // Outer classes before the classes nested in them.
        let mut program = classes.keys().map(String::as_str).collect::<Vec<_>>();
        program.sort_unstable_by_key(|descriptor| (descriptor.len(), *descriptor));
        loop {
            let mut renamed = mapped
                .iter()
                .filter(|(old, new)| old != new && !dropped.contains(*old))
                .map(|(old, new)| (old.clone(), new.clone()))
                .collect::<HashMap<_, _>>();
            for descriptor in &program {
                if mapped.contains_key(*descriptor) || dropped.contains(*descriptor) {
                    continue;
                }
                let binary = &descriptor[..descriptor.len() - 1];
                let derived = binary
                    .char_indices()
                    .rev()
                    .filter(|(_, c)| *c == '$')
                    .find_map(|(at, _)| {
                        let outer = format!("{};", &binary[..at]);
                        renamed
                            .get(&outer)
                            .map(|new| format!("{}{}", &new[..new.len() - 1], &descriptor[at..]))
                    });
                if let Some(new) = derived {
                    renamed.insert((*descriptor).to_string(), new);
                }
            }
            // Every name a class ends up with, renamed or not.
            let mut owners = HashMap::<&str, Vec<&str>>::new();
            for descriptor in classes.keys().chain(renamed.keys()) {
                let shown = renamed
                    .get(descriptor)
                    .map_or(descriptor.as_str(), String::as_str);
                let owners = owners.entry(shown).or_default();
                if !owners.contains(&descriptor.as_str()) {
                    owners.push(descriptor);
                }
            }
            let mut clashes = owners
                .iter()
                .filter(|(_, owners)| owners.len() > 1)
                .flat_map(|(shown, owners)| {
                    owners
                        .iter()
                        .filter(|owner| renamed.contains_key(**owner))
                        .map(move |owner| (*owner, *shown, "another class"))
                })
                // The runtime loads the platform's class of that name, never the program's.
                .chain(
                    renamed
                        .iter()
                        .filter(|(_, new)| is_platform(new))
                        .map(|(old, new)| (old.as_str(), new.as_str(), "a platform class")),
                )
                .collect::<Vec<_>>();
            if clashes.is_empty() {
                return renamed;
            }
            clashes.sort_unstable();
            clashes.dedup_by_key(|(owner, _, _)| *owner);
            for (owner, shown, other) in clashes {
                warnings.push(format!(
                    "class {owner} keeps its name: {shown} names {other}"
                ));
                dropped.insert(owner.to_string());
            }
        }
    }

    /// Warnings about dropped mappings since the last call.
    pub(crate) fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().expect("name warnings mutex poisoned"))
    }

    /// The new descriptor of a class (`Lz/c;`), `None` if it keeps its name.
    pub(crate) fn renamed_class(&self, descriptor: &str) -> Option<&str> {
        self.renamed.get(descriptor).map(String::as_str)
    }

    /// The descriptor the source shows for the DEX class `descriptor`.
    pub(crate) fn source_class<'a>(&'a self, descriptor: &'a str) -> &'a str {
        self.renamed_class(descriptor).unwrap_or(descriptor)
    }

    /// The DEX descriptor of the class the source calls `descriptor`.
    pub(crate) fn dex_class<'a>(&'a self, descriptor: &'a str) -> &'a str {
        self.originals
            .get(descriptor)
            .map_or(descriptor, String::as_str)
    }

    /// `name` and `descriptor` (`(args)Ret`), as the DEX has them, of the
    /// method the source shows as `class->name descriptor`; `None` if the
    /// class declares no such method.
    pub(crate) fn dex_method(
        &self,
        class: &str,
        name: &str,
        descriptor: &str,
    ) -> Option<(String, String)> {
        let class = self.dex_class(class);
        let members = self.declared(class)?;
        members.methods.iter().find_map(|(tail, new)| {
            let (old_name, proto) = tail.split_at(tail.find('(')?);
            let shown_name = new.as_deref().unwrap_or(old_name);
            let shown_proto = self.descriptors_in(proto);
            (shown_name == name && shown_proto.as_deref().unwrap_or(proto) == descriptor)
                .then(|| (old_name.to_string(), proto.to_string()))
        })
    }

    /// `text` (a type descriptor, a list of them or a method prototype) with
    /// every renamed class replaced; `None` if nothing changes.
    pub(crate) fn descriptors_in(&self, text: &str) -> Option<String> {
        if self.renamed.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        let mut rest = text;
        // Outside class descriptors, `L` never occurs in descriptors.
        while let Some(start) = rest.find('L') {
            let Some(length) = rest[start..].find(';') else {
                break;
            };
            let class = &rest[start..=start + length];
            out.push_str(&rest[..start]);
            match self.renamed.get(class) {
                Some(new) => {
                    out.push_str(new);
                    changed = true;
                }
                None => out.push_str(class),
            }
            rest = &rest[start + length + 1..];
        }
        out.push_str(rest);
        changed.then_some(out)
    }

    /// `Lowner;->name:Type` with the new names; `None` if nothing changes.
    pub(crate) fn field_reference(&self, reference: &str) -> Option<String> {
        self.member_reference(reference, MemberKind::Field)
    }

    /// `Lowner;->name(args)Ret` with the new names; `None` if nothing changes.
    pub(crate) fn method_reference(&self, reference: &str) -> Option<String> {
        self.member_reference(reference, MemberKind::Method)
    }

    fn member_reference(&self, reference: &str, kind: MemberKind) -> Option<String> {
        // Not `MemberRef::split`: the owner may be an array (`[I->clone()…`).
        let (owner, tail) = reference.split_once("->")?;
        let member = MemberRef { owner, tail };
        let split = match kind {
            MemberKind::Field => member.tail.find(':')?,
            MemberKind::Method => member.tail.find('(')?,
        };
        let (name, rest) = member.tail.split_at(split);
        let new_name = self.member_name(member, kind);
        let owner = self.descriptors_in(member.owner);
        let rest_renamed = self.descriptors_in(rest);
        if new_name.is_none() && owner.is_none() && rest_renamed.is_none() {
            return None;
        }
        Some(format!(
            "{}->{}{}",
            owner.as_deref().unwrap_or(member.owner),
            new_name.as_deref().unwrap_or(name),
            rest_renamed.as_deref().unwrap_or(rest)
        ))
    }

    /// The new name of the member `member` refers to, found on the class
    /// that declares it.
    fn member_name(&self, member: MemberRef<'_>, kind: MemberKind) -> Option<String> {
        if member.tail.starts_with('<') || (self.fields.is_empty() && self.methods.is_empty()) {
            return None;
        }
        match self.declaring_class(member.owner, member.tail, kind) {
            Some(declarer) => self
                .declared(&declarer)?
                .get(kind)
                .get(member.tail)
                .cloned()
                .flatten(),
            // Not declared in the program: a member of a class asc was not
            // given, which only its own mapping entry can name.
            None => {
                let table = match kind {
                    MemberKind::Field => &self.fields,
                    MemberKind::Method => &self.methods,
                };
                table
                    .get(&format!("{}->{}", member.owner, member.tail))
                    .cloned()
            }
        }
    }

    /// The program class that declares the member `tail` looked up on
    /// `owner`, by the JVM's resolution order: fields on the class, then
    /// its interfaces, then its superclass; methods on the class and its
    /// superclasses, then on their interfaces.
    fn declaring_class(&self, owner: &str, tail: &str, kind: MemberKind) -> Option<String> {
        let declares = |class: &str| {
            self.declared(class)
                .is_some_and(|members| members.get(kind).contains_key(tail))
        };
        let mut visited = HashSet::new();
        match kind {
            MemberKind::Field => {
                let mut pending = vec![owner.to_string()];
                while let Some(class) = pending.pop() {
                    if !visited.insert(class.clone()) {
                        continue;
                    }
                    if declares(&class) {
                        return Some(class);
                    }
                    let Some(program) = self.classes.get(&class) else {
                        continue;
                    };
                    // Popped last to first: interfaces before the superclass.
                    pending.extend(program.superclass.iter().cloned());
                    pending.extend(program.interfaces.iter().rev().cloned());
                }
                None
            }
            MemberKind::Method => {
                let mut interfaces = VecDeque::new();
                let mut class = Some(owner.to_string());
                while let Some(current) = class.take() {
                    if !visited.insert(current.clone()) {
                        break;
                    }
                    if declares(&current) {
                        return Some(current);
                    }
                    let Some(program) = self.classes.get(&current) else {
                        break;
                    };
                    interfaces.extend(program.interfaces.iter().cloned());
                    class = program.superclass.clone();
                }
                while let Some(interface) = interfaces.pop_front() {
                    if !visited.insert(interface.clone()) {
                        continue;
                    }
                    if declares(&interface) {
                        return Some(interface);
                    }
                    if let Some(program) = self.classes.get(&interface) {
                        interfaces.extend(program.interfaces.iter().cloned());
                    }
                }
                None
            }
        }
    }

    /// The members the program class `class` declares, read once.
    fn declared(&self, class: &str) -> Option<Arc<DeclaredMembers>> {
        if let Some(members) = self
            .members
            .read()
            .expect("declared members lock poisoned")
            .get(class)
        {
            return Some(Arc::clone(members));
        }
        let program = self.classes.get(class)?;
        let members = Arc::new(self.member_renames(class, &program.fields, &program.methods));
        let mut cache = self
            .members
            .write()
            .expect("declared members lock poisoned");
        Some(Arc::clone(
            cache.entry(class.to_string()).or_insert(members),
        ))
    }

    /// The new names of the members `class` declares. Java tells fields
    /// apart by name and methods by name and parameters; a mapping that
    /// would make two members of the class one is dropped, with a warning,
    /// until none would.
    fn member_renames(
        &self,
        class: &str,
        fields: &[String],
        methods: &[String],
    ) -> DeclaredMembers {
        let tails = |references: &[String], table: &BTreeMap<String, String>| {
            references
                .iter()
                .filter_map(|reference| {
                    let tail = reference.split_once("->")?.1.to_string();
                    let new = table.get(reference).cloned();
                    Some((tail, new))
                })
                .collect::<Vec<_>>()
        };
        let mut fields = tails(fields, &self.fields);
        let mut methods = tails(methods, &self.methods);
        self.drop_member_clashes(
            class,
            &mut fields,
            |tail| tail.split_once(':').map_or(tail, |(name, _)| name),
            |_| "",
        );
        self.drop_member_clashes(
            class,
            &mut methods,
            |tail| tail.split_once('(').map_or(tail, |(name, _)| name),
            |tail| {
                tail.split_once('(')
                    .and_then(|(_, proto)| proto.split_once(')'))
                    .map_or("", |(params, _)| params)
            },
        );
        let keep = |members: Vec<(String, Option<String>)>| {
            members
                .into_iter()
                .map(|(tail, new)| {
                    (
                        tail.clone(),
                        new.filter(|new| Some(new.as_str()) != old_name(&tail)),
                    )
                })
                .collect()
        };
        DeclaredMembers {
            fields: keep(fields),
            methods: keep(methods),
        }
    }

    /// Drops the new names of `members` that would share a Java identity
    /// (`name` plus `params`) with another member of the class.
    fn drop_member_clashes(
        &self,
        class: &str,
        members: &mut [(String, Option<String>)],
        name: impl Fn(&str) -> &str,
        params: impl Fn(&str) -> &str,
    ) {
        loop {
            let mut groups = HashMap::<(&str, &str), Vec<usize>>::new();
            for (at, (tail, new)) in members.iter().enumerate() {
                // Constructors are named by their class.
                if tail.starts_with('<') {
                    continue;
                }
                let shown = new.as_deref().unwrap_or_else(|| name(tail));
                groups.entry((shown, params(tail))).or_default().push(at);
            }
            let mut clashes = groups
                .values()
                .filter(|group| group.len() > 1)
                .flatten()
                .copied()
                .filter(|&at| {
                    members[at]
                        .1
                        .as_ref()
                        .is_some_and(|new| new != name(&members[at].0))
                })
                .collect::<Vec<_>>();
            if clashes.is_empty() {
                return;
            }
            clashes.sort_unstable();
            for at in clashes {
                let (tail, new) = &mut members[at];
                let new = new.take().unwrap_or_default();
                self.warn(format!("{class}->{tail} keeps its name: {new} would name another member of the class too"));
            }
        }
    }

    fn warn(&self, warning: String) {
        self.warnings
            .lock()
            .expect("name warnings mutex poisoned")
            .push(warning);
    }

    /// The new simple name of a nested class (`InnerClass.name`), taken
    /// from its new binary name: what follows its outer class's new name,
    /// or else its last `$` segment, without the digits a local class's
    /// name starts with. `None` if the class keeps its name. `class` and
    /// `outer` are DEX descriptors.
    pub(crate) fn nested_simple_name<'a>(
        &'a self,
        class: &'a str,
        outer: Option<&'a str>,
    ) -> Option<String> {
        let binary = |descriptor: &'a str| {
            descriptor
                .strip_prefix('L')
                .and_then(|inner| inner.strip_suffix(';'))
        };
        let binary_name = binary(self.renamed_class(class)?)?;
        let in_outer = outer.and_then(|outer| {
            binary_name
                .strip_prefix(binary(self.source_class(outer))?)?
                .strip_prefix('$')
        });
        let binary = binary_name;
        let tail = in_outer.unwrap_or_else(|| binary.rsplit(['$', '/']).next().unwrap_or(binary));
        let name = tail.trim_start_matches(|c: char| c.is_ascii_digit());
        (!name.is_empty()).then(|| name.to_string())
    }

    /// The new name of the annotation element `element` of the annotation
    /// type `annotation` (a method of that interface, by DEX descriptor).
    pub(crate) fn annotation_element(&self, annotation: &str, element: &str) -> Option<String> {
        let members = self.declared(annotation)?;
        members.methods.iter().find_map(|(tail, new)| {
            tail.strip_prefix(element)?
                .starts_with("()")
                .then(|| new.clone())
                .flatten()
        })
    }

    /// A generic signature with every renamed class replaced; `None` if
    /// nothing changes or the signature does not parse.
    pub(crate) fn signature(&self, signature: &str) -> Option<String> {
        if self.renamed.is_empty() {
            return None;
        }
        signature::rename(signature, |descriptor| self.renamed_class(descriptor))
    }
}

/// The member name of a tail (`name:Type`, `name(params)ret`).
fn old_name(tail: &str) -> Option<&str> {
    tail.split(['(', ':']).next()
}
