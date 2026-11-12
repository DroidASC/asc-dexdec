use super::*;

/// A program of declarations only: classes and their member references.
#[derive(Default)]
struct Program {
    classes: Vec<(String, ProgramClass)>,
}

impl Program {
    fn class(
        mut self,
        descriptor: &str,
        superclass: &str,
        interfaces: &[&str],
        fields: &[&str],
        methods: &[&str],
    ) -> Self {
        let class = ProgramClass {
            superclass: Some(superclass.into()),
            interfaces: interfaces
                .iter()
                .map(|&interface| interface.into())
                .collect(),
            fields: fields
                .iter()
                .map(|field| format!("{descriptor}->{field}"))
                .collect(),
            methods: methods
                .iter()
                .map(|method| format!("{descriptor}->{method}"))
                .collect(),
        };
        self.classes.push((descriptor.into(), class));
        self
    }

    fn open(self, mappings: &NameMappings) -> ProgramNames {
        ProgramNames::new(self.classes, mappings)
    }
}

fn mappings(
    classes: &[(&str, &str)],
    fields: &[(&str, &str)],
    methods: &[(&str, &str)],
) -> NameMappings {
    let mut mappings = NameMappings::new();
    for (old, new) in classes {
        mappings.insert_class(*old, *new).unwrap();
    }
    for (old, new) in fields {
        mappings.insert_field(*old, *new).unwrap();
    }
    for (old, new) in methods {
        mappings.insert_method(*old, *new).unwrap();
    }
    mappings
}

const OBJECT: &str = "Ljava/lang/Object;";

#[test]
fn classes_move_package_and_every_descriptor_follows() {
    let program = Program::default().class("La/a;", OBJECT, &[], &["b:La/a;"], &["c([La/a;)La/a;"]);
    let names = program.open(&mappings(&[("La/a;", "Lcom/example/Cart;")], &[], &[]));

    assert_eq!(names.descriptors_in("La/a;").unwrap(), "Lcom/example/Cart;");
    assert_eq!(
        names
            .descriptors_in("([[La/a;ILjava/lang/String;)La/a;")
            .unwrap(),
        "([[Lcom/example/Cart;ILjava/lang/String;)Lcom/example/Cart;"
    );
    assert_eq!(names.descriptors_in("Ljava/lang/String;"), None);
    assert_eq!(
        names.field_reference("La/a;->b:La/a;").unwrap(),
        "Lcom/example/Cart;->b:Lcom/example/Cart;"
    );
    assert_eq!(
        names.method_reference("La/a;->c([La/a;)La/a;").unwrap(),
        "Lcom/example/Cart;->c([Lcom/example/Cart;)Lcom/example/Cart;"
    );
    assert_eq!(
        names
            .method_reference("[La/a;->clone()Ljava/lang/Object;")
            .unwrap(),
        "[Lcom/example/Cart;->clone()Ljava/lang/Object;"
    );
    assert_eq!(names.dex_class("Lcom/example/Cart;"), "La/a;");
    assert_eq!(names.source_class("La/a;"), "Lcom/example/Cart;");
}

#[test]
fn fields_and_methods_take_their_new_names_at_declaration() {
    let program = Program::default().class(
        "La;",
        OBJECT,
        &[],
        &["a:I", "b:I"],
        &["a()V", "a(I)V", "<init>()V"],
    );
    let mapping = mappings(
        &[],
        &[("La;->a:I", "count")],
        &[("La;->a()V", "reset"), ("La;->a(I)V", "add")],
    );
    let names = program.open(&mapping);

    assert_eq!(names.field_reference("La;->a:I").unwrap(), "La;->count:I");
    assert_eq!(names.field_reference("La;->b:I"), None);
    assert_eq!(
        names.method_reference("La;->a()V").unwrap(),
        "La;->reset()V"
    );
    assert_eq!(
        names.method_reference("La;->a(I)V").unwrap(),
        "La;->add(I)V"
    );
    assert_eq!(names.method_reference("La;-><init>()V"), None);
    assert_eq!(
        names.dex_method("La;", "add", "(I)V"),
        Some(("a".into(), "(I)V".into()))
    );
    assert_eq!(names.dex_method("La;", "b", "()V"), None);
}

#[test]
fn methods_report_their_dex_names_under_renamed_classes() {
    let program = Program::default().class("La;", OBJECT, &[], &[], &["a(La;)La;"]);
    let names = program.open(&mappings(
        &[("La;", "Lp/Node;")],
        &[],
        &[("La;->a(La;)La;", "next")],
    ));
    assert_eq!(
        names.dex_method("Lp/Node;", "next", "(Lp/Node;)Lp/Node;"),
        Some(("a".into(), "(La;)La;".into()))
    );
}

#[test]
fn references_resolve_to_the_declaring_class() {
    // B extends A implements I; I extends J. A declares field `f` and method
    // `m`; J declares the constant `k` and the method `n`.
    let program = Program::default()
        .class(
            "LA;",
            OBJECT,
            &[],
            &["f:I"],
            &["m()V", "toString()Ljava/lang/String;"],
        )
        .class("LJ;", OBJECT, &[], &["k:I"], &["n()V"])
        .class("LI;", OBJECT, &["LJ;"], &[], &[])
        .class("LB;", "LA;", &["LI;"], &[], &["m()V"]);
    let mapping = mappings(
        &[("LA;", "Lp/Base;"), ("LB;", "Lp/Derived;")],
        &[("LA;->f:I", "size"), ("LJ;->k:I", "LIMIT")],
        &[
            ("LA;->m()V", "run"),
            ("LB;->m()V", "run"),
            ("LJ;->n()V", "next"),
        ],
    );
    let names = program.open(&mapping);

    // Inherited from the superclass, overridden, through interfaces.
    assert_eq!(
        names.field_reference("LB;->f:I").unwrap(),
        "Lp/Derived;->size:I"
    );
    assert_eq!(
        names.method_reference("LB;->m()V").unwrap(),
        "Lp/Derived;->run()V"
    );
    assert_eq!(
        names.field_reference("LB;->k:I").unwrap(),
        "Lp/Derived;->LIMIT:I"
    );
    assert_eq!(names.method_reference("LI;->n()V").unwrap(), "LI;->next()V");
    assert_eq!(
        names.method_reference("LB;->n()V").unwrap(),
        "Lp/Derived;->next()V"
    );
    // A library method keeps its name; so does a member nobody declares.
    assert_eq!(
        names
            .method_reference("LB;->toString()Ljava/lang/String;")
            .unwrap(),
        "Lp/Derived;->toString()Ljava/lang/String;"
    );
    assert_eq!(
        names.method_reference("LB;->hashCode()I").unwrap(),
        "Lp/Derived;->hashCode()I"
    );
    assert_eq!(
        names.method_reference("Ljava/lang/Object;->hashCode()I"),
        None
    );
    // Constructors are never renamed.
    assert_eq!(
        names.method_reference("LB;-><init>()V").unwrap(),
        "Lp/Derived;-><init>()V"
    );
}

#[test]
fn a_member_declared_again_below_keeps_the_lower_declaration() {
    // B redeclares A's field `f` (hiding it) and has no mapping for it.
    let program = Program::default()
        .class("LA;", OBJECT, &[], &["f:I"], &[])
        .class("LB;", "LA;", &[], &["f:I"], &[]);
    let names = program.open(&mappings(&[], &[("LA;->f:I", "size")], &[]));
    assert_eq!(names.field_reference("LB;->f:I"), None);
    assert_eq!(names.field_reference("LA;->f:I").unwrap(), "LA;->size:I");
}

#[test]
fn the_first_definition_of_a_class_wins() {
    // Under the second definition, `x` would clash with `z`.
    let program = Program::default()
        .class("La;", OBJECT, &[], &["x:I"], &[])
        .class("La;", OBJECT, &[], &["x:I", "z:I"], &[]);
    let names = program.open(&mappings(&[], &[("La;->x:I", "z")], &[]));
    assert_eq!(names.field_reference("La;->x:I").unwrap(), "La;->z:I");
    assert!(names.take_warnings().is_empty());
}

#[test]
fn members_of_classes_outside_the_program_use_their_own_entries() {
    let names = Program::default().open(&mappings(&[], &[("Lx/Lib;->a:I", "value")], &[]));
    assert_eq!(
        names.field_reference("Lx/Lib;->a:I").unwrap(),
        "Lx/Lib;->value:I"
    );
}

#[test]
fn clashing_class_names_keep_the_dex_names() {
    let program = Program::default()
        .class("La;", OBJECT, &[], &[], &[])
        .class("Lb;", OBJECT, &[], &[], &[])
        .class("Lp/Kept;", OBJECT, &[], &[], &[]);
    // a and b both to Same; c (not in the program) onto a class that keeps its name.
    let mapping = mappings(
        &[
            ("La;", "Lp/Same;"),
            ("Lb;", "Lp/Same;"),
            ("Lc;", "Lp/Kept;"),
        ],
        &[],
        &[],
    );
    let names = program.open(&mapping);
    assert_eq!(names.renamed_class("La;"), None);
    assert_eq!(names.renamed_class("Lb;"), None);
    assert_eq!(names.renamed_class("Lc;"), None);
    let warnings = names.take_warnings();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert_eq!(
        warnings[0],
        "class La; keeps its name: Lp/Same; names another class"
    );
    assert!(names.take_warnings().is_empty());
}

#[test]
fn platform_class_names_are_not_taken() {
    // R8 desugars records into a class its mapping calls `java.lang.Record`.
    let program = Program::default()
        .class("La;", OBJECT, &[], &[], &[])
        .class("Lb;", "La;", &[], &[], &[]);
    let names = program.open(&mappings(
        &[("La;", "Ljava/lang/Record;"), ("Lb;", "LFoo;")],
        &[],
        &[],
    ));
    assert_eq!(names.renamed_class("La;"), None);
    assert_eq!(names.renamed_class("Lb;"), Some("LFoo;"));
    assert_eq!(
        names.take_warnings(),
        ["class La; keeps its name: Ljava/lang/Record; names a platform class"]
    );
}

#[test]
fn a_swap_is_not_a_clash() {
    let program = Program::default()
        .class("La;", OBJECT, &[], &["x:I", "y:I"], &[])
        .class("Lb;", OBJECT, &[], &[], &[]);
    let mapping = mappings(
        &[("La;", "Lb;"), ("Lb;", "La;")],
        &[("La;->x:I", "y"), ("La;->y:I", "x")],
        &[],
    );
    let names = program.open(&mapping);
    assert_eq!(names.renamed_class("La;"), Some("Lb;"));
    assert_eq!(names.field_reference("La;->x:I").unwrap(), "Lb;->y:I");
    assert!(names.take_warnings().is_empty());
}

#[test]
fn clashing_member_names_keep_the_dex_names() {
    // Fields clash by name whatever their type; methods by name and
    // parameters whatever their return type; overloads do not clash.
    let program = Program::default().class(
        "La;",
        OBJECT,
        &[],
        &["a:I", "b:J", "c:I"],
        &["a()V", "b()I", "c(I)V", "d()Ljava/lang/String;"],
    );
    let mapping = mappings(
        &[],
        &[
            ("La;->a:I", "value"),
            ("La;->b:J", "value"),
            ("La;->c:I", "b"),
        ],
        &[
            ("La;->a()V", "get"),
            ("La;->b()I", "get"),
            ("La;->c(I)V", "get"),
            ("La;->d()Ljava/lang/String;", "a"),
        ],
    );
    let names = program.open(&mapping);
    assert_eq!(names.field_reference("La;->a:I"), None);
    assert_eq!(names.field_reference("La;->b:J"), None);
    // `c` would become `b`, which `b:J` now keeps.
    assert_eq!(names.field_reference("La;->c:I"), None);
    assert_eq!(names.method_reference("La;->a()V"), None);
    assert_eq!(names.method_reference("La;->b()I"), None);
    assert_eq!(
        names.method_reference("La;->c(I)V").unwrap(),
        "La;->get(I)V"
    );
    // `a()V` keeps `a`, so `d()` cannot take it.
    assert_eq!(names.method_reference("La;->d()Ljava/lang/String;"), None);
    assert_eq!(names.take_warnings().len(), 6);
}

#[test]
fn nested_classes_move_with_their_outer_class() {
    let program = Program::default()
        .class("La;", OBJECT, &[], &[], &[])
        .class("La$b;", OBJECT, &[], &[], &[])
        .class("La$c;", OBJECT, &[], &[], &[])
        .class("La$1;", OBJECT, &[], &[], &[]);
    let mapping = mappings(
        &[("La;", "Lp/Outer;"), ("La$b;", "Lp/Outer$Inner;")],
        &[],
        &[],
    );
    let names = program.open(&mapping);

    assert_eq!(names.renamed_class("La$b;"), Some("Lp/Outer$Inner;"));
    assert_eq!(
        names.nested_simple_name("La$b;", Some("La;")).as_deref(),
        Some("Inner")
    );
    // Unmapped nested classes follow their outer class's new name.
    assert_eq!(names.renamed_class("La$c;"), Some("Lp/Outer$c;"));
    assert_eq!(
        names.nested_simple_name("La$c;", Some("La;")).as_deref(),
        Some("c")
    );
    assert_eq!(names.renamed_class("La$1;"), Some("Lp/Outer$1;"));
}

#[test]
fn local_classes_lose_the_digits_of_their_binary_name() {
    let program = Program::default()
        .class("La;", OBJECT, &[], &[], &[])
        .class("Lb;", OBJECT, &[], &[], &[]);
    let names = program.open(&mappings(
        &[("La;", "Lp/Outer;"), ("Lb;", "Lp/Outer$1Local;")],
        &[],
        &[],
    ));
    assert_eq!(
        names.nested_simple_name("Lb;", Some("La;")).as_deref(),
        Some("Local")
    );
    assert_eq!(
        names.nested_simple_name("Lb;", None).as_deref(),
        Some("Local")
    );
    assert_eq!(
        names.nested_simple_name("La;", None).as_deref(),
        Some("Outer")
    );
}

#[test]
fn annotations_rename_their_elements_and_values() {
    let program = Program::default()
        .class(
            "LAnno;",
            OBJECT,
            &[],
            &[],
            &["a()I", "b()Ljava/lang/Class;"],
        )
        .class("LE;", OBJECT, &[], &["A:LE;"], &[]);
    let mapping = mappings(
        &[("LAnno;", "Lp/Tag;"), ("LE;", "Lp/Color;")],
        &[("LE;->A:LE;", "RED")],
        &[
            ("LAnno;->a()I", "level"),
            ("LAnno;->b()Ljava/lang/Class;", "kind"),
        ],
    );
    let names = program.open(&mapping);

    assert_eq!(
        names.annotation_element("LAnno;", "a").as_deref(),
        Some("level")
    );
    assert_eq!(
        names.annotation_element("LAnno;", "b").as_deref(),
        Some("kind")
    );
    assert_eq!(names.annotation_element("LAnno;", "value"), None);
    assert_eq!(
        names.annotation_element("Ldalvik/annotation/Signature;", "value"),
        None
    );
    assert_eq!(
        names.field_reference("LE;->A:LE;").unwrap(),
        "Lp/Color;->RED:Lp/Color;"
    );
    assert_eq!(
        names.signature("Ljava/util/List<LE;>;").unwrap(),
        "Ljava/util/List<Lp/Color;>;"
    );
}

#[test]
fn generic_signatures_rename_their_classes() {
    let rename = |descriptor: &str| match descriptor {
        "La;" => Some("Lp/Outer;"),
        "La$b;" => Some("Lp/Outer$Inner;"),
        "Lc;" => Some("Lq/Moved;"),
        "La$c;" => Some("Lq/Elsewhere;"),
        _ => None,
    };
    let rename_all = |text: &str| signature::rename(text, rename);
    assert_eq!(
        rename_all("<T:La;>La<TT;>;Ljava/lang/Comparable<La;>;").unwrap(),
        "<T:Lp/Outer;>Lp/Outer<TT;>;Ljava/lang/Comparable<Lp/Outer;>;"
    );
    assert_eq!(
        rename_all("<T::Ljava/lang/Comparable<-TT;>;U:Lc;>(TT;[La<*>.b<+Lc;>;)La$b;^Lc;").unwrap(),
        "<T::Ljava/lang/Comparable<-TT;>;U:Lq/Moved;>(TT;[Lp/Outer<*>.Inner<+Lq/Moved;>;)Lp/Outer$Inner;^Lq/Moved;"
    );
    // A nested class moved out of its outer class loses the outer type's arguments.
    assert_eq!(
        rename_all("La<TT;>.c<Ljava/lang/String;>;").unwrap(),
        "Lq/Elsewhere<Ljava/lang/String;>;"
    );
    // Type variables named like classes stay; untouched and malformed signatures give `None`.
    assert_eq!(rename_all("TLc;"), None);
    assert_eq!(rename_all("Ljava/util/List<Ljava/lang/String;>;"), None);
    assert_eq!(rename_all("La<"), None);
}

#[test]
fn malformed_entries_are_refused() {
    let mut mappings = NameMappings::new();
    assert!(mappings.insert_class("La;", "Lb").is_err());
    assert!(mappings.insert_class("[La;", "Lb;").is_err());
    assert!(mappings.insert_field("La;->a", "b").is_err());
    assert!(mappings.insert_field("La;->a:I", "b c").is_err());
    assert!(mappings.insert_method("La;-><init>()V", "make").is_err());
    assert!(mappings.insert_method("La;->a(Q)V", "b").is_err());
    assert!(mappings.insert_method("La;->a(I)V", "b.c").is_err());
    assert!(mappings.is_empty());
    mappings.insert_method("La;->a([ILb;)V", "class").unwrap();
    assert!(!mappings.is_empty());
}
