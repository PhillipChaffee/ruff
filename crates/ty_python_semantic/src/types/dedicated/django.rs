use ruff_db::files::File;
use ruff_db::parsed::parsed_module;
use ruff_python_ast::name::Name;
use rustc_hash::FxHashSet;
use ty_module_resolver::{
    ImportingFile, KnownModule, Module, ModuleName, ResolverEnvironment, SearchPath,
    file_to_module, list_modules, resolve_module,
};
use ty_python_core::{
    ProgramFile,
    definition::{Definition, DefinitionKind},
    global_scope, place_table, semantic_index, use_def_map,
};

use crate::place::{global_symbol, known_module_symbol, place_from_bindings};
use crate::types::call::Binding;
use crate::types::class::{ClassLiteral, DynamicClassAnchor, DynamicClassLiteral};
use crate::types::infer::nearest_enclosing_class;
use crate::types::member::class_member;
use crate::types::typed_dict::{TypedDictFieldBuilder, TypedDictType};
use crate::types::{
    BindingContext, BoundTypeVarInstance, ClassBase, ClassType, FunctionType, GenericContext,
    IntersectionType, KnownClass, Parameter, Parameters, SelfTypeVarOrigin, Signature,
    Specialization, StaticClassLiteral, Type, UnionType, binding_type, definition_expression_type,
};
use crate::{Db, FxIndexSet, ProgramEnvironment};

/// Returns whether `class` is a subclass of Django's `Model`.
pub(in crate::types) fn is_model<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> bool {
    class
        .iter_mro(db, None)
        .skip(1)
        .filter_map(ClassBase::into_class)
        .any(|base| base.is_known(db, KnownClass::DjangoModel))
}

/// Returns whether `class` is Django's `ForeignKey` or one of its subclasses.
///
/// `OneToOneField` is a subclass of `ForeignKey`, and so is a user-defined relation field:
/// ```py
/// class TrackedForeignKey(models.ForeignKey): ...
/// ```
pub(in crate::types) fn is_foreign_key<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> bool {
    class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .any(|base| base.is_known(db, KnownClass::DjangoForeignKey))
}

/// The type of a Django model field, and metadata extracted from the field construction.
///
/// For example, `isbn` is a `CharField` with `metadata.primary_key` set to `true`.
/// ```py
/// class Book(models.Model):
///     isbn = CharField(max_length=13, primary_key=True)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct Field<'db> {
    /// The name of the attribute that Django creates for the field.
    name: Name,
    /// The name that the field is assigned to in the class body. It differs from `name` if the
    /// field construction has a `name` argument.
    symbol: Name,
    ty: Type<'db>,
    /// The type of the first argument of the field construction, which names the related model
    /// of a relation field: `Author` or `"Author"` in `ForeignKey(Author, ...)`.
    to: Option<Type<'db>>,
    /// The type of the `through` argument of a `ManyToManyField`, which names the model of the
    /// intermediate table.
    through: Option<Type<'db>>,
    metadata: FieldMetadata,
}

impl<'db> Field<'db> {
    /// Create a field by extracting the metadata from its definition.
    fn from_definition(
        db: &'db dyn Db,
        name: Name,
        ty: Type<'db>,
        definition: Definition<'db>,
    ) -> Self {
        let mut field = Field {
            symbol: name.clone(),
            name,
            ty,
            to: None,
            through: None,
            metadata: FieldMetadata::default(),
        };
        let module = parsed_module(db, definition.python_file(db)).load(db);
        let kind = definition.kind(db);
        let value = match &kind {
            DefinitionKind::Assignment(assignment) => assignment.value(&module),
            DefinitionKind::AnnotatedAssignment(assignment) => {
                let Some(value) = assignment.value(&module) else {
                    return field;
                };
                value
            }
            _ => return field,
        };
        let Some(call) = value.as_call_expr() else {
            return field;
        };

        field.to = call
            .arguments
            .find_argument_value("to", 0)
            .map(|to| definition_expression_type(db, definition, to));
        field.through = call
            .arguments
            .find_keyword("through")
            .map(|through| definition_expression_type(db, definition, &through.value));

        if let Some(name) = call.arguments.find_keyword("name")
            && let Some(name) =
                definition_expression_type(db, definition, &name.value).as_string_literal()
            && !name.value(db).is_empty()
        {
            field.name = Name::new(name.value(db));
        }

        field.metadata.primary_key =
            call.arguments
                .find_keyword("primary_key")
                .is_some_and(|primary_key| {
                    definition_expression_type(db, definition, &primary_key.value).as_bool_literal()
                        == Some(true)
                });
        field.metadata.has_choices =
            call.arguments
                .find_keyword("choices")
                .is_some_and(|choices| {
                    !definition_expression_type(db, definition, &choices.value).is_none(db)
                });
        field.metadata.null = call.arguments.find_keyword("null").is_some_and(|null| {
            definition_expression_type(db, definition, &null.value).as_bool_literal() == Some(true)
        });
        field.metadata.to_field = call
            .arguments
            .find_keyword("to_field")
            .and_then(|to_field| {
                definition_expression_type(db, definition, &to_field.value)
                    .as_string_literal()
                    .map(|name| Name::new(name.value(db)))
            });
        field.metadata.related_name =
            call.arguments
                .find_keyword("related_name")
                .and_then(|related_name| {
                    definition_expression_type(db, definition, &related_name.value)
                        .as_string_literal()
                        .map(|name| Name::new(name.value(db)))
                });
        field.metadata.parent_link =
            call.arguments
                .find_keyword("parent_link")
                .is_some_and(|parent_link| {
                    definition_expression_type(db, definition, &parent_link.value).as_bool_literal()
                        == Some(true)
                });
        field
    }
}

/// Metadata extracted from a Django field's construction.
#[derive(Debug, Clone, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
#[expect(clippy::struct_excessive_bools)]
struct FieldMetadata {
    primary_key: bool,
    has_choices: bool,
    null: bool,
    to_field: Option<Name>,
    parent_link: bool,
    related_name: Option<Name>,
}

/// All fields defined on this Django model (not including base class fields).
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
fn own_model_fields<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> Box<[Field<'db>]> {
    let env = ProgramEnvironment::from_scope(class.body_scope(db));
    let table = place_table(db, class.body_scope(db));
    use_def_map(db, class.body_scope(db))
        .all_end_of_scope_symbol_bindings()
        .flat_map(|(symbol_id, bindings)| {
            let name = table.symbol(symbol_id).name();
            bindings.filter_map(move |binding| {
                binding
                    .binding
                    .definition()
                    .map(|definition| (name, definition))
            })
        })
        .filter_map(|(name, definition)| {
            let field_ty = binding_type(db, definition);
            let is_field = field_ty.nominal_class(db, &env).is_some_and(|class| {
                class
                    .iter_mro(db)
                    .filter_map(ClassBase::into_class)
                    .any(|base| base.is_known(db, KnownClass::DjangoField))
            });
            if !is_field {
                return None;
            }
            Some(Field::from_definition(
                db,
                name.clone(),
                field_ty,
                definition,
            ))
        })
        .collect()
}

/// Metadata extracted from a Django model's `Meta` configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct ModelMetadata {
    is_abstract: bool,
    is_proxy: bool,
}

#[salsa::tracked(returns(copy), heap_size=ruff_memory_usage::heap_size)]
fn model_metadata<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> ModelMetadata {
    let env = ProgramEnvironment::from_scope(class.body_scope(db));
    // Subclasses do not inherit `abstract = True`, so only inspect this model's own Meta.
    let Some(meta) = own_meta(db, class) else {
        return ModelMetadata::default();
    };
    ModelMetadata {
        is_abstract: is_abstract(db, &env, class, meta),
        // Unlike `abstract`, Django never resets `proxy`.
        is_proxy: meta
            .iter_mro(db, None)
            .filter_map(ClassBase::into_class)
            .map_while(|meta_base| meta_base.static_class_literal(db))
            .find_map(|(meta_base, _)| meta_setting(db, &env, meta_base, "proxy"))
            .unwrap_or(false),
    }
}

/// Returns whether `meta`, the `Meta` configuration of the model `class`, makes it abstract.
fn is_abstract<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    meta: StaticClassLiteral<'db>,
) -> bool {
    for meta_base in meta.iter_mro(db, None).filter_map(ClassBase::into_class) {
        let Some((meta_base, _)) = meta_base.static_class_literal(db) else {
            return false;
        };
        // Django resets an abstract model's Meta.abstract to False during model creation.
        // A child can inherit other options from that Meta, but must set abstract=True itself.
        let enclosing_scope = meta_base.definition(db).scope(db);
        if let Some(owner) = nearest_enclosing_class(
            db,
            semantic_index(db, enclosing_scope.program_file(db)),
            enclosing_scope,
        ) && owner != class
            && is_model(db, owner)
            && own_meta(db, owner) == Some(meta_base)
        {
            return false;
        }

        if let Some(is_abstract) = meta_setting(db, env, meta_base, "abstract") {
            return is_abstract;
        }
    }
    false
}

/// Returns whether the body of the `Meta` class `meta` sets `name` to `True`, or `None` if it has
/// no such setting.
fn meta_setting<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    meta: StaticClassLiteral<'db>,
    name: &str,
) -> Option<bool> {
    let scope = meta.body_scope(db);
    let symbol = place_table(db, scope).symbol_id(name)?;
    // The public type is widened to `bool`, so read the bindings' literal values instead.
    let value = place_from_bindings(
        db,
        env,
        use_def_map(db, scope).end_of_scope_symbol_bindings(symbol),
    )
    .place
    .ignore_possibly_undefined()?;
    Some(value == Type::bool_literal(true))
}

fn own_meta<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Option<StaticClassLiteral<'db>> {
    let meta = class_member(db, class.body_scope(db), "Meta")
        .ignore_possibly_undefined()?
        .as_class_literal()?
        .as_static()?;
    (meta.name(db).as_str() == "Meta" && meta.definition(db).scope(db) == class.body_scope(db))
        .then_some(meta)
}

/// Provides synthesized attributes and methods for Django models.
pub(in crate::types) fn synthesized_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    if class.is_known(db, KnownClass::DjangoBaseManager) {
        return manager_queryset_method(db, env, specialization?, name);
    }
    if let Some(setting) = setting(db, env, class, name) {
        return Some(setting);
    }

    if name == "__init__" && is_model(db, class) {
        return init_method(db, env, class, specialization);
    }
    // Django does not create other members with a private name.
    if name.starts_with('_') || !is_model(db, class) {
        return None;
    }
    // A quick filter on the name of the members that depend on the fields of the model. A reverse
    // accessor can have any name.
    if !(matches!(
        name,
        "id" | "pk" | "DoesNotExist" | "MultipleObjectsReturned"
    ) || name.ends_with("_id")
        || name.ends_with("_ptr")
        || name.starts_with("get_") && name.ends_with("_display")
        || name.starts_with("get_next_by_")
        || name.starts_with("get_previous_by_"))
    {
        return renamed_field(db, class, specialization, name)
            .or_else(|| reverse_accessor(db, class, name));
    }
    match name {
        "DoesNotExist" | "MultipleObjectsReturned" => model_exception(db, env, class, name),
        "id" | "pk" => match primary_key_member(db, env, class, specialization, name)? {
            PrimaryKeyMember::Automatic(ty) | PrimaryKeyMember::Explicit(ty) => Some(ty),
        },
        _ => display_method(db, env, class, specialization, name)
            .or_else(|| date_navigation_method(db, env, class, specialization, name))
            .or_else(|| foreign_key_id_member(db, env, class, specialization, name))
            .or_else(|| parent_link_member(db, env, class, specialization, name))
            .or_else(|| reverse_accessor(db, class, name)),
    }
}

/// Synthesize the `__init__` method of a model, with one keyword parameter per name that Django's
/// `Model.__init__` accepts.
///
/// The stubs declare `Model.__init__(self, *args: Any, **kwargs: Any)`. Django itself rejects a
/// keyword that names neither a field nor a property, so `Book(title="Dune")` raises a `TypeError`
/// here:
/// ```py
/// class Book(models.Model):
///     title = models.CharField(max_length=200)
///     author = models.ForeignKey(Author, on_delete=models.CASCADE)
/// ```
///
/// For this model, we construct
/// `def __init__(self, *args: Any, title: str | int | Combinable = ..., author: ... = ...,
/// author_id: Any = ..., id: Any = ..., pk: Any = ...) -> None`. A field's parameter has the type
/// that an assignment to the field accepts. Every keyword is optional because fields can be
/// assigned after construction, and positional arguments are not checked.
///
/// Returns `None`, which keeps the declared `__init__`, if we cannot see everything that the model
/// inherits or if a base class between the model and `Model` defines its own `__init__`.
fn init_method<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
) -> Option<Type<'db>> {
    let any = Type::any();
    let mut keywords: Vec<(Name, Type<'db>)> = vec![(Name::new_static("pk"), any)];
    let mut has_primary_key = false;
    let mut reached_model = false;

    for base in class.iter_mro(db, specialization) {
        let base = match base {
            ClassBase::Class(base) => base,
            // An unknown base class can add fields and properties that we cannot see.
            ClassBase::Dynamic(_) => return None,
            _ => continue,
        };
        let (base, base_specialization) = base.static_class_literal(db)?;
        if base.is_known(db, KnownClass::DjangoModel) {
            reached_model = true;
        } else if !reached_model
            && base != class
            && class_member(db, base.body_scope(db), "__init__")
                .ignore_possibly_undefined()
                .is_some()
        {
            return None;
        }

        // Django also accepts a keyword that names a property, and assigns the value to it.
        keywords.extend(
            own_property_names(db, base)
                .iter()
                .map(|name| (name.clone(), any)),
        );
        if !is_model(db, base) {
            continue;
        }

        for field in own_model_fields(db, base) {
            has_primary_key |= field.metadata.primary_key;
            let field_ty = refined_field_type(db, env, base, &field.symbol, field.ty)
                .unwrap_or(field.ty)
                .apply_optional_specialization(db, base_specialization);
            let Some(field_class) = field_ty.nominal_class(db, env) else {
                continue;
            };
            // Django prohibits assigning to the forward side of a many-to-many relation.
            if field_class.name(db) == "ManyToManyField" {
                continue;
            }
            let set_ty = field_class
                .iter_mro(db)
                .filter_map(ClassBase::into_class)
                .filter_map(|field_base| field_base.static_class_literal(db))
                .find(|(field_base, _)| field_base.is_known(db, KnownClass::DjangoField))
                .and_then(|(_, specialization)| specialization?.types(db).first().copied())
                .unwrap_or(any);
            keywords.push((field.name.clone(), set_ty));
            if field_class
                .static_class_literal(db)
                .is_some_and(|(field_class, _)| is_foreign_key(db, field_class))
            {
                keywords.push((Name::new(format!("{}_id", field.name)), any));
            }
        }

        // A model that inherits from a concrete model has an automatic link to that parent.
        if base != class && !model_metadata(db, base).is_abstract {
            let parent = base.name(db).to_lowercase();
            keywords.push((Name::new(format!("{parent}_ptr")), any));
            keywords.push((Name::new(format!("{parent}_ptr_id")), any));
        }
    }
    if !has_primary_key {
        keywords.push((Name::new_static("id"), any));
    }

    let self_ty = Type::instance(
        db,
        env,
        class.apply_optional_specialization(db, specialization),
    );
    let mut parameters = vec![
        Parameter::positional_or_keyword(Name::new_static("self")).with_annotated_type(self_ty),
        Parameter::variadic(Name::new_static("args")).with_annotated_type(any),
    ];
    // The first class in the method resolution order that defines a name determines its type.
    let mut seen = FxHashSet::default();
    parameters.extend(
        keywords
            .into_iter()
            .filter(|(name, _)| seen.insert(name.clone()))
            .map(|(name, ty)| {
                Parameter::keyword_only(name)
                    .with_annotated_type(ty)
                    .with_default_type(ty)
            }),
    );
    Some(Type::function_like_callable(
        db,
        Signature::new(Parameters::standard(parameters), Type::none(db, env)),
    ))
}

/// The names of the properties defined in the body of `class`.
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
fn own_property_names<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> Box<[Name]> {
    let scope = class.body_scope(db);
    place_table(db, scope)
        .symbols()
        .filter(|symbol| {
            matches!(
                class_member(db, scope, symbol.name()).ignore_possibly_undefined(),
                Some(Type::PropertyInstance(_))
            )
        })
        .map(|symbol| symbol.name().clone())
        .collect()
}

/// Return the type of the setting `name` on `django.conf.settings`, an instance of `LazySettings`.
///
/// The stubs give every setting the type `Any`, through a `__getattr__` method. Django's own
/// settings have default values in `django.conf.global_settings`, which the stubs declare with
/// their types, such as `DEBUG: bool`. We use those types. We do not read the project's settings
/// module, so a setting that the project adds keeps the type `Any`.
fn setting<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    // Settings are uppercase. Check the names first to avoid a module-resolution query for every
    // class with an unresolved attribute.
    if class.name(db) != "LazySettings"
        || !name.starts_with(|c: char| c.is_ascii_uppercase())
        || name.contains(|c: char| c.is_ascii_lowercase())
        || !file_to_module(db, class.body_scope(db).program_file(db).resolver_file(db))
            .is_some_and(|module| module.is_known(db, KnownModule::DjangoConf))
    {
        return None;
    }
    known_module_symbol(db, env, KnownModule::DjangoConfGlobalSettings, name)
        .place
        .ignore_possibly_undefined()
}

/// Synthesize the `DoesNotExist` and `MultipleObjectsReturned` exception classes of a model.
///
/// Django creates both classes for every model that is not abstract. They inherit from the
/// exceptions of the model's concrete parents, or from `ObjectDoesNotExist` and
/// `MultipleObjectsReturned` in `django.core.exceptions` for a model without one:
/// ```py
/// class Book(models.Model): ...
/// class Ebook(Book): ...
///
/// # Ebook.DoesNotExist is a subclass of Book.DoesNotExist, which is a subclass of
/// # ObjectDoesNotExist.
/// ```
///
/// The stubs declare the attributes on `Model` as `type[ObjectDoesNotExist]`. With a class for
/// each model, `except Book.DoesNotExist` does not also catch the exception of another model.
fn model_exception<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    if model_metadata(db, class).is_abstract {
        return None;
    }

    let parent_exceptions: Box<[Type<'db>]> = class
        .explicit_bases(db)
        .iter()
        .filter(|base| {
            base.to_class_type(db)
                .and_then(|base| base.static_class_literal(db))
                .is_some_and(|(base, _)| {
                    is_model(db, base) && !model_metadata(db, base).is_abstract
                })
        })
        .map(|base| base.member(db, env, name).place.ignore_possibly_undefined())
        .collect::<Option<_>>()?;
    let explicit_bases = if parent_exceptions.is_empty() {
        // The declaration on `Model` names the exception that the classes of a model without a
        // concrete parent inherit from.
        let Type::SubclassOf(declared) = KnownClass::DjangoModel
            .to_class_literal(db, env)
            .member(db, env, name)
            .place
            .ignore_possibly_undefined()?
        else {
            return None;
        };
        Box::new([Type::from(declared.subclass_of().into_class(db, env)?)])
    } else {
        parent_exceptions
    };

    Some(Type::ClassLiteral(ClassLiteral::Dynamic(
        DynamicClassLiteral::new(
            db,
            Name::new(name),
            DynamicClassAnchor::Implicit {
                owner: class,
                explicit_bases,
            },
            Box::default(),
            false,
            None,
        ),
    )))
}

/// Synthesize the attribute of a field whose `name` argument differs from the name it is assigned
/// to. Django creates the attribute under the name given by the argument:
/// ```py
/// class Article(models.Model):
///     views = models.IntegerField(name="view_count")
///
/// # Article().view_count has type int.
/// ```
fn renamed_field<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    own_model_fields(db, class)
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.ty.apply_optional_specialization(db, specialization))
}

/// An attribute that a relation field adds to the model it refers to.
#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
struct ReverseAccessor<'db> {
    name: Name,
    ty: Type<'db>,
}

/// Synthesize the reverse accessors of a model.
///
/// A `ForeignKey` adds a manager for the source model to the related model, and a
/// `OneToOneField` adds the source model instance. The default name is the lowercase name of the
/// source model, followed by `_set` for a `ForeignKey`:
/// ```py
/// class Author(models.Model):
///     pass
///
/// class Book(models.Model):
///     author = models.ForeignKey(Author, on_delete=models.CASCADE)
///
/// class Profile(models.Model):
///     author = models.OneToOneField(Author, on_delete=models.CASCADE)
///
/// # Author().book_set is a manager for Book; Author().profile has type Profile.
/// ```
///
/// We find the relation fields of the models in the module that defines the related model, and in
/// the project's `models` modules (see [`models_modules`]).
fn reverse_accessor<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    reverse_accessors(db, class)
        .iter()
        .find(|accessor| accessor.name == name)
        .map(|accessor| accessor.ty)
}

/// All reverse accessors that the project's models add to `class`.
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn reverse_accessors<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Box<[ReverseAccessor<'db>]> {
    let file = class.body_scope(db).program_file(db);
    let env = ProgramEnvironment::from_file(file);
    let target = Type::instance(db, &env, class.default_specialization(db));

    let sources = project_models(db, file)
        .iter()
        .copied()
        // Abstract models have no table, so nothing can refer to their rows.
        .filter(|source| !model_metadata(db, *source).is_abstract);

    let mut accessors = Vec::new();
    for source in sources {
        let source_name = source.name(db).to_lowercase();
        let source_ty = Type::instance(db, &env, source.default_specialization(db));

        // A model that inherits from a concrete model is reachable from its parent.
        if source != class
            && !model_metadata(db, source).is_proxy
            && source
                .explicit_bases(db)
                .iter()
                .filter_map(|base| concrete_model(db, base.to_class_type(db)?))
                .any(|parent| {
                    parent.static_class_literal(db).map(|(parent, _)| parent) == Some(class)
                })
        {
            accessors.push(ReverseAccessor {
                name: Name::new(&source_name),
                ty: source_ty,
            });
        }

        // The fields of the model itself, and the ones it inherits from abstract models.
        let declaring_classes = source
            .iter_mro(db, None)
            .filter_map(ClassBase::into_class)
            .filter_map(|base| base.static_class_literal(db))
            .map(|(base, _)| base)
            .filter(|base| {
                *base == source || (is_model(db, *base) && model_metadata(db, *base).is_abstract)
            });
        for declaring in declaring_classes {
            for field in own_model_fields(db, declaring) {
                let Some((field_class, _)) = field
                    .ty
                    .nominal_class(db, &env)
                    .and_then(|field_class| field_class.static_class_literal(db))
                else {
                    continue;
                };
                if !is_foreign_key(db, field_class)
                    || related_model(db, &env, declaring, source, field) != Some(target)
                {
                    continue;
                }
                let is_one_to_one = field_class
                    .iter_mro(db, None)
                    .filter_map(ClassBase::into_class)
                    .any(|base| base.name(db) == "OneToOneField");
                let name = match field.metadata.related_name.as_deref() {
                    // A name that ends with `+` disables the reverse accessor. We do not know the
                    // app label of a model.
                    Some(related_name)
                        if related_name.ends_with('+')
                            || related_name.contains("%(app_label)s") =>
                    {
                        continue;
                    }
                    Some(related_name) => related_name.replace("%(class)s", &source_name),
                    None if is_one_to_one => source_name.clone(),
                    None => format!("{source_name}_set"),
                };
                let ty = if is_one_to_one {
                    Some(source_ty)
                } else {
                    related_manager(db, &env, source_ty)
                };
                if let Some(ty) = ty {
                    accessors.push(ReverseAccessor {
                        name: Name::new(name),
                        ty,
                    });
                }
            }
        }
    }
    accessors.into_boxed_slice()
}

/// The models that Django knows about when it uses a model defined in `file`: the models in the
/// global scope of `file` itself and of the project's `models` modules (see [`models_modules`]).
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn project_models<'db>(db: &'db dyn Db, file: ProgramFile<'db>) -> Box<[StaticClassLiteral<'db>]> {
    let env = ProgramEnvironment::from_file(file);
    // A module that several `models` modules import from contributes each model only once.
    let models: FxIndexSet<_> = std::iter::once(file)
        .chain(
            models_modules(db, env.resolver_environment(db))
                .iter()
                .map(|module| ProgramFile::new(db, *module, env.program(db))),
        )
        .flat_map(|file| {
            use_def_map(db, global_scope(db, file))
                .all_end_of_scope_symbol_bindings()
                .flat_map(|(_, bindings)| {
                    bindings.filter_map(|binding| binding.binding.definition())
                })
        })
        .filter_map(|definition| binding_type(db, definition).as_class_literal()?.as_static())
        .filter(|class| is_model(db, *class))
        .collect();
    models.into_iter().collect()
}

/// The files of the project's `models` modules, including the submodules of a `models` package.
///
/// Django imports the `models` module of each installed app when it starts, and a model only
/// takes part in relations once its module is imported. We do not read the project's settings to
/// find the installed apps, so we treat every first-party module with a `models` component in its
/// name as the models of an app: `library.models` and `library.models.book`, for example.
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
fn models_modules<'db>(
    db: &'db dyn Db,
    resolver_environment: ResolverEnvironment<'db>,
) -> Box<[File]> {
    let mut files = Vec::new();
    let mut pending: Vec<Module<'db>> = list_modules(db, resolver_environment)
        .iter()
        .copied()
        .filter(|module| {
            module
                .search_path(db)
                .is_some_and(SearchPath::is_first_party)
        })
        .collect();
    while let Some(module) = pending.pop() {
        if module
            .name(db)
            .components()
            .any(|component| component == "models")
            && let Some(file) = module.file(db)
        {
            files.push(file);
        }
        pending.extend(module.all_submodules(db));
    }
    files.sort_unstable();
    files.into_boxed_slice()
}

/// Return the type of the manager that a `ForeignKey` adds to its related model.
fn related_manager<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: Type<'db>,
) -> Option<Type<'db>> {
    let manager = known_module_symbol(
        db,
        env,
        KnownModule::DjangoDbModelsFieldsRelatedDescriptors,
        "RelatedManager",
    )
    .place
    .ignore_possibly_undefined()?
    .as_class_literal()?
    .as_static()?;
    let generic_context = manager.generic_context(db)?;
    (generic_context.len(db) == 2).then(|| {
        Type::instance(
            db,
            env,
            manager.apply_specialization(db, |generic_context| {
                generic_context.specialize_partial(db, [Some(model), None])
            }),
        )
    })
}

/// Synthesize the methods that a manager copies from its `QuerySet` class.
///
/// A manager created with `QuerySet.as_manager()` or `Manager.from_queryset()` has a copy of every
/// public method that the custom `QuerySet` class adds. For example, `Book.objects.published()`
/// calls `BookQuerySet.published`:
/// ```py
/// class BookQuerySet(models.QuerySet["Book"]):
///     def published(self) -> "BookQuerySet": ...
///
/// class Book(models.Model):
///     objects = BookQuerySet.as_manager()
/// ```
///
/// django-stubs gives such a manager the type `Manager[Book, BookQuerySet]`, so we look the method
/// up on the second type argument.
fn manager_queryset_method<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    specialization: Specialization<'db>,
    name: &str,
) -> Option<Type<'db>> {
    // Django does not copy private methods.
    if name.starts_with('_') {
        return None;
    }
    let queryset_ty = *specialization.types(db).get(1)?;

    // The stubs already declare the methods of `QuerySet` itself on the manager, or leave them
    // out on purpose, so only consider the classes below `QuerySet`.
    queryset_ty
        .nominal_class(db, env)?
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .map_while(|base| base.static_class_literal(db))
        .take_while(|(base, _)| !base.is_known(db, KnownClass::DjangoQuerySet))
        .find(|(base, _)| !class_member(db, base.body_scope(db), name).is_undefined())?;

    let method = queryset_ty
        .member(db, env, name)
        .place
        .ignore_possibly_undefined()?;
    matches!(method, Type::BoundMethod(_)).then_some(method)
}

enum PrimaryKeyMember<'db> {
    Automatic(Type<'db>),
    Explicit(Type<'db>),
}

pub(in crate::types) fn is_automatic_primary_key_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    object_ty: Type<'db>,
    name: &str,
    member_ty: Type<'db>,
) -> bool {
    if matches!(name, "id" | "pk")
        && let Type::NominalInstance(instance) = object_ty
        && let Some((class, specialization)) = instance.class(db, env).static_class_literal(db)
        && is_model(db, class)
    {
        matches!(
            primary_key_member(db, env, class, specialization, name),
            Some(PrimaryKeyMember::Automatic(ty)) if ty == member_ty
        )
    } else {
        false
    }
}

/// Synthesize `id` and `pk` attributes for Django models.
///
/// Without an explicit primary key, both `id` and `pk` have type `int`:
/// ```py
/// class Book(models.Model):
///     title = models.CharField(max_length=200)
///
/// # Book().id and Book().pk have type int.
/// ```
///
/// With an explicit primary key, `pk` has that field's value type and no automatic `id` is added:
/// ```py
/// class Book(models.Model):
///     isbn = models.CharField(max_length=13, primary_key=True)
///
/// # Book().id does not exist; Book().pk has type str.
/// ```
fn primary_key_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<PrimaryKeyMember<'db>> {
    // Explicit `id` or `pk` attributes on base classes take precedence over a synthesized
    // attribute on this model.
    if class
        .iter_mro(db, specialization)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .filter(|(base, _)| !base.is_known(db, KnownClass::DjangoModel))
        .any(|(base, _)| !class_member(db, base.body_scope(db), name).is_undefined())
    {
        return None;
    }

    for (base, specialization) in class
        .iter_mro(db, specialization)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
    {
        if base.is_known(db, KnownClass::DjangoModel) {
            break;
        }
        if let Some(field) = own_primary_key(db, base) {
            return (name == "pk").then(|| {
                PrimaryKeyMember::Explicit(
                    foreign_key_id_type(db, env, base, field, specialization).unwrap_or_else(
                        || field.ty.apply_optional_specialization(db, specialization),
                    ),
                )
            });
        }
    }

    // An abstract model does not have an automatic primary key, but every instance of it is an
    // instance of a concrete subclass. Use the primary keys of the concrete subclasses that we
    // know about, if all of them have the attribute:
    // ```py
    // class Publication(models.Model):
    //     class Meta:
    //         abstract = True
    //
    // class Book(Publication): ...
    //
    // def f(publication: Publication):
    //     publication.pk  # `int`, the primary key of `Book`
    // ```
    if model_metadata(db, class).is_abstract {
        let inherits_from = |model: StaticClassLiteral<'db>, base: StaticClassLiteral<'db>| {
            model != base
                && model
                    .iter_mro(db, None)
                    .filter_map(ClassBase::into_class)
                    .filter_map(|model_base| model_base.static_class_literal(db))
                    .any(|(model_base, _)| model_base == base)
        };
        let (abstract_subclasses, concrete_subclasses): (Vec<_>, Vec<_>) =
            project_models(db, class.body_scope(db).program_file(db))
                .iter()
                .copied()
                .filter(|model| inherits_from(*model, class))
                .partition(|model| model_metadata(db, *model).is_abstract);
        // The attribute lookup on an abstract subclass also reaches this class. Such a subclass
        // without concrete subclasses of its own must not get the primary key of its siblings.
        if !abstract_subclasses.iter().all(|abstract_subclass| {
            concrete_subclasses
                .iter()
                .any(|concrete| inherits_from(*concrete, *abstract_subclass))
        }) {
            return None;
        }
        let mut members = concrete_subclasses
            .into_iter()
            .map(|subclass| primary_key_member(db, env, subclass, None, name))
            .collect::<Option<Vec<_>>>()?
            .into_iter();
        let first = members.next()?;
        return Some(
            members.fold(first, |primary_key, member| match (primary_key, member) {
                (PrimaryKeyMember::Automatic(ty), PrimaryKeyMember::Automatic(other))
                    if ty == other =>
                {
                    PrimaryKeyMember::Automatic(ty)
                }
                (
                    PrimaryKeyMember::Automatic(ty) | PrimaryKeyMember::Explicit(ty),
                    PrimaryKeyMember::Automatic(other) | PrimaryKeyMember::Explicit(other),
                ) => PrimaryKeyMember::Explicit(UnionType::from_two_elements(db, env, ty, other)),
            }),
        );
    }

    known_module_symbol(db, env, KnownModule::DjangoDbModelsFields, "AutoField")
        .place
        .ignore_possibly_undefined()?
        .to_instance_approximation(db, env)
        .map(PrimaryKeyMember::Automatic)
}

/// Find a field constructed with `primary_key=True` in this class body.
fn own_primary_key<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Option<&'db Field<'db>> {
    own_model_fields(db, class)
        .iter()
        .find(|field| field.metadata.primary_key)
}

/// django-stubs annotates `ForeignKey.__init__`'s `to` parameter as `type[Model] | str`.
/// Use the supplied model class to fill missing read and write types on the descriptor.
pub(in crate::types) fn foreign_key_model_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    binding: &Binding<'db>,
) -> Option<Type<'db>> {
    let to = binding.parameter_type_by_name(db, "to", false).ok()??;
    let null = binding.parameter_type_by_name(db, "null", false).ok()?;
    let is_nullable = null.and_then(Type::as_bool_literal) == Some(true);
    let model = match to.to_instance_approximation(db, env) {
        Some(model) => model,
        // A string reference such as `ForeignKey("Author", null=True)`. We do not resolve
        // the string to a model, but the field is still nullable: use `Any | None` so that
        // `None` is not lost from the field's type.
        None if is_nullable && to.as_string_literal().is_some() => Type::any(),
        None => return None,
    };
    Some(if is_nullable {
        UnionType::from_two_elements(db, env, model, Type::none(db, env))
    } else {
        model
    })
}

/// Return the type that one of django-stubs' type aliases denotes, if we replace it.
///
/// `ty` is the type that the alias denotes according to the stubs.
///
/// `_User` in `django.contrib.auth.models` is the stubs' placeholder for the model named by the
/// `AUTH_USER_MODEL` setting. It is declared as `AbstractBaseUser` and used wherever Django
/// returns the user model, such as `get_user_model()` and `HttpRequest.user`. The stubs expect a
/// type checker to refine it. We do not read settings, so we use `AbstractUser`: it is the base
/// class of Django's default `User` model and of most custom user models, and it has the
/// attributes that `AbstractBaseUser` lacks, such as `username` and `is_superuser`.
///
/// `_ChoicesPrimitive` in `django.utils.choices` is the type of the `choices` argument of a
/// field. Its `type[Choices]` element is currently lost when we simplify the union (see
/// <https://github.com/astral-sh/ty/issues/4551>), which rejects `choices=SomeChoicesClass`. We
/// add the metaclass `ChoicesType`, whose instances are the same class objects.
pub(in crate::types) fn type_alias_override<'db>(
    db: &'db dyn Db,
    file: ProgramFile<'db>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    // Check the name first to avoid a module-resolution query for every type alias.
    let module = match name {
        "_User" => KnownModule::DjangoContribAuthModels,
        "_ChoicesPrimitive" => KnownModule::DjangoUtilsChoices,
        _ => return None,
    };
    if !file_to_module(db, file.resolver_file(db))
        .is_some_and(|file_module| file_module.is_known(db, module))
    {
        return None;
    }

    let env = ProgramEnvironment::from_file(file);
    let instance_of = |module, class| {
        known_module_symbol(db, &env, module, class)
            .place
            .ignore_possibly_undefined()?
            .to_instance_approximation(db, &env)
    };
    match module {
        KnownModule::DjangoContribAuthModels => instance_of(module, "AbstractUser"),
        _ => Some(UnionType::from_two_elements(
            db,
            &env,
            ty,
            instance_of(KnownModule::DjangoDbModelsEnums, "ChoicesType")?,
        )),
    }
}

/// Return the class that a call to `function` returns, if it is
/// `django.contrib.auth.get_user_model`.
///
/// The stubs declare the return type as `type[_User]`. A variable of that type is not a class
/// that can be used in a type expression, which rejects the common pattern of annotating with
/// the result:
/// ```py
/// User = get_user_model()
///
/// def greet(user: User) -> str: ...
/// ```
///
/// We return the class `AbstractUser` itself. See [`type_alias_override`] for why it stands in for
/// the user model.
pub(in crate::types) fn user_model_class<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    function: FunctionType<'db>,
) -> Option<Type<'db>> {
    if !is_get_user_model(db, function) {
        return None;
    }
    known_module_symbol(
        db,
        env,
        KnownModule::DjangoContribAuthModels,
        "AbstractUser",
    )
    .place
    .ignore_possibly_undefined()
}

/// Returns whether `function` is `django.contrib.auth.get_user_model`.
pub(in crate::types) fn is_get_user_model<'db>(
    db: &'db dyn Db,
    function: FunctionType<'db>,
) -> bool {
    // Check the name first to avoid a module-resolution query for every function.
    function.name(db) == "get_user_model"
        && file_to_module(
            db,
            function.definition(db).program_file(db).resolver_file(db),
        )
        .is_some_and(|module| module.name(db).as_str() == "django.contrib.auth")
}

/// Return the manager class that `Manager.from_queryset()` creates.
///
/// django-stubs declares the return type as `type[Self]`, which loses the `QuerySet` class. We
/// specialize the manager with the model and the `QuerySet` class instead, so that the manager
/// has the methods of the custom `QuerySet`:
/// ```py
/// class BookQuerySet(models.QuerySet["Book"]): ...
///
/// # BookManager is Manager[Book, BookQuerySet]
/// BookManager = models.Manager.from_queryset(BookQuerySet)
/// ```
pub(in crate::types) fn from_queryset_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    manager: Type<'db>,
    binding: &Binding<'db>,
) -> Option<Type<'db>> {
    let (manager, _) = manager.to_class_type(db)?.static_class_literal(db)?;
    // A manager class that fixes its own type arguments cannot be specialized again.
    if manager.generic_context(db)?.len(db) != 2
        || !manager
            .iter_mro(db, None)
            .filter_map(ClassBase::into_class)
            .any(|base| base.is_known(db, KnownClass::DjangoBaseManager))
    {
        return None;
    }

    let queryset = binding
        .parameter_type_by_name(db, "queryset_class", false)
        .ok()??
        .to_instance_approximation(db, env)?;
    let model = queryset
        .nominal_class(db, env)?
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .find(|(base, _)| base.is_known(db, KnownClass::DjangoQuerySet))
        .and_then(|(_, specialization)| specialization?.types(db).first().copied())?;

    Some(Type::from(
        manager.apply_specialization(db, |generic_context| {
            generic_context.specialize(db, vec![model, queryset])
        }),
    ))
}

/// Return the model whose fields the keyword arguments of `manager.filter()` name, if we check
/// them.
///
/// We only check calls on Django's own `Manager` class for a model of the project. A custom
/// manager can add annotations to its `QuerySet`, and a model of another package can be the
/// target of relations that we do not look for.
pub(in crate::types) fn lookup_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    manager: Type<'db>,
) -> Option<Type<'db>> {
    let module_of = |class: StaticClassLiteral<'db>| {
        file_to_module(db, class.body_scope(db).program_file(db).resolver_file(db))
    };

    let (manager, specialization) = manager.nominal_class(db, env)?.static_class_literal(db)?;
    if !module_of(manager)?.is_known(db, KnownModule::DjangoDbModelsManager) {
        return None;
    }
    let model = specialization?.types(db).first().copied()?;
    let (model_class, _) = model.nominal_class(db, env)?.static_class_literal(db)?;
    let is_first_party = module_of(model_class)?
        .search_path(db)
        .is_some_and(SearchPath::is_first_party);
    // A base class that we cannot resolve can add fields that we cannot see.
    let has_dynamic_base = model_class
        .iter_mro(db, None)
        .any(|base| matches!(base, ClassBase::Dynamic(_)));
    (is_first_party && is_model(db, model_class) && !has_dynamic_base).then_some(model)
}

/// Returns whether `keyword` can start a lookup on `model` in a call such as
/// `Book.objects.filter(title__startswith="D")`.
///
/// A lookup starts with the name of a field, of `pk`, or of a relation from another model. The
/// names that follow a `__` are not checked. A relation from another model is named after its
/// `related_name` or the lowercase name of the other model:
/// ```py
/// class Review(models.Model):
///     book = models.ForeignKey(Book, on_delete=models.CASCADE)
///
/// Book.objects.filter(review__rating=5)
/// ```
///
/// We accept the name of any model and any `related_name` of the project, without checking that
/// the relation refers to `model`.
pub(in crate::types) fn is_lookup_name<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    model: Type<'db>,
    keyword: &str,
) -> bool {
    let name = keyword.split_once("__").map_or(keyword, |(name, _)| name);
    if name == "pk" || !model.member(db, env, name).place.is_undefined() {
        return true;
    }
    let Some((model_class, _)) = model
        .nominal_class(db, env)
        .and_then(|class| class.static_class_literal(db))
    else {
        return true;
    };
    project_models(db, model_class.body_scope(db).program_file(db))
        .iter()
        .any(|other| {
            other.name(db).to_lowercase() == name
                || own_model_fields(db, *other)
                    .iter()
                    .any(|field| field.metadata.related_name.as_deref() == Some(name))
        })
}

/// Add the attributes that `QuerySet.annotate()` creates to the model of the `QuerySet` it
/// returns.
///
/// `annotate()` sets an attribute on each instance for every keyword argument:
/// ```py
/// article = Article.objects.annotate(title_length=Length("title")).get()
/// article.title_length
/// ```
///
/// `declared` is the return type according to the stubs, the same `QuerySet[Article, Article]`
/// that the method is called on. We intersect the model with a protocol that has one member per
/// keyword. The members have the type `Any`, because we do not infer the type of an expression's
/// result.
///
/// Returns `None`, which keeps the declared type, for a `QuerySet` class that fixes its model.
pub(in crate::types) fn annotate_return_type<'db, 'a>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declared: Type<'db>,
    keywords: impl IntoIterator<Item = &'a str>,
) -> Option<Type<'db>> {
    let (queryset, specialization) = declared.nominal_class(db, env)?.static_class_literal(db)?;
    if !queryset.is_known(db, KnownClass::DjangoQuerySet) {
        return None;
    }
    let &[model, row] = specialization?.types(db) else {
        return None;
    };

    let mut annotations = keywords
        .into_iter()
        .map(|keyword| (keyword, Type::any()))
        .peekable();
    annotations.peek()?;
    let annotated = IntersectionType::from_two_elements(
        db,
        env,
        model,
        Type::protocol_with_readonly_members(db, env, annotations),
    );

    Some(Type::instance(
        db,
        env,
        queryset.apply_specialization(db, |generic_context| {
            // Rows are model instances unless `values()` or `values_list()` changed their shape.
            let row = if row == model { annotated } else { row };
            generic_context.specialize(db, vec![annotated, row])
        }),
    ))
}

/// The shape of the rows that a `QuerySet` method returns for the fields named in its call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::types) enum QuerySetRows {
    /// `values("title")`: a dictionary keyed by field name.
    Dict,
    /// `values_list("title", "rating")`: a tuple with one element per field.
    Tuple,
    /// `values_list("title", flat=True)`: the value of the single field.
    Flat,
}

/// Infer the row type of `QuerySet.values()` and `QuerySet.values_list()` from the fields named
/// in the call.
///
/// The stubs declare the rows as `dict[str, Any]` and `Any`. When every field is named by a
/// string literal, the rows follow those fields' types:
/// ```py
/// class Article(models.Model):
///     title = models.CharField(max_length=100)
///     rating = models.IntegerField()
///
/// Article.objects.values("title")  # rows are a `TypedDict` with a `title` key of type `str`
/// Article.objects.values_list("title", "rating")  # rows are `tuple[str, int]`
/// Article.objects.values_list("title", flat=True)  # rows are `str`
/// ```
///
/// Returns `None`, which keeps the declared type, for calls that this does not model: a call
/// without fields, a field that is not a string literal, and a lookup that spans a relation such
/// as `"author__name"`.
pub(in crate::types) fn queryset_rows_return_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declared: Type<'db>,
    fields: impl IntoIterator<Item = Type<'db>>,
    rows: QuerySetRows,
) -> Option<Type<'db>> {
    let (queryset, specialization) = declared.nominal_class(db, env)?.static_class_literal(db)?;
    if !queryset.is_known(db, KnownClass::DjangoQuerySet) {
        return None;
    }
    let model = specialization?.types(db).first().copied()?;

    let columns = fields
        .into_iter()
        .map(|field| {
            let name = field.as_string_literal()?.value(db);
            let ty = model
                .member(db, env, name)
                .place
                .ignore_possibly_undefined()?;
            Some((Name::new(name), ty))
        })
        .collect::<Option<Vec<_>>>()?;
    let row = match (rows, columns.as_slice()) {
        (_, []) => return None,
        (QuerySetRows::Dict, _) => Type::TypedDict(TypedDictType::from_schema_items(
            db,
            columns
                .into_iter()
                .map(|(name, ty)| (name, TypedDictFieldBuilder::new(ty).required(true).build()))
                .collect(),
        )),
        (QuerySetRows::Tuple, _) => {
            Type::heterogeneous_tuple(db, env, columns.into_iter().map(|(_, ty)| ty))
        }
        (QuerySetRows::Flat, [(_, ty)]) => *ty,
        (QuerySetRows::Flat, _) => return None,
    };

    Some(Type::instance(
        db,
        env,
        queryset.apply_specialization(db, |generic_context| {
            generic_context.specialize(db, vec![model, row])
        }),
    ))
}

/// Synthesize `<field>_id` attributes for foreign keys using the referenced field's type.
///
/// For example, `Book().author_id` has type `int`, matching `Author`'s automatic primary key:
/// ```py
/// class Author(models.Model):
///     pass
///
/// class Book(models.Model):
///     author = models.ForeignKey(Author, on_delete=models.CASCADE)
/// ```
fn foreign_key_id_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    let field_name = name.strip_suffix("_id")?;
    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == field_name)?;
    foreign_key_id_type(db, env, class, field, specialization)
}

/// Return a relation field's stored ID type, used by both `<field>_id` and `pk`.
fn foreign_key_id_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    field: &Field<'db>,
    specialization: Option<Specialization<'db>>,
) -> Option<Type<'db>> {
    let field_ty = refined_field_type(db, env, class, &field.name, field.ty)
        .unwrap_or(field.ty)
        .apply_optional_specialization(db, specialization);
    let foreign_key = KnownClass::DjangoForeignKey.try_to_class_literal(db, env)?;
    if !field_ty
        .nominal_class(db, env)?
        .is_subtype_of_class_literal(db, foreign_key.into())
    {
        return None;
    }

    // The descriptor returns the related model instance. For a nullable foreign key,
    // look up the target field on the model and add `None` to the resulting ID type.
    let instance = Type::instance(
        db,
        env,
        class.apply_optional_specialization(db, specialization),
    );
    let related_model = field_ty
        .try_call_dunder_get(db, env, Some(instance), instance.to_meta_type(db, env))
        .ok()??
        .return_type
        .filter_union(db, env, |ty| !ty.is_none(db));
    let target_name = field.metadata.to_field.as_deref().unwrap_or("pk");
    let id_ty = related_model
        .member(db, env, target_name)
        .place
        .ignore_possibly_undefined()?;
    Some(if field.metadata.null {
        UnionType::from_elements(db, env, [id_ty, Type::none(db, env)])
    } else {
        id_ty
    })
}

/// Return the model that a relation field of `model` refers to. `declaring` is the class whose
/// body declares the field: `model` itself or one of its abstract base classes.
///
/// The related model is the first argument of the field construction. It can be a class, the
/// name of a model in the same module, or `"self"` for the model that declares the field:
/// ```py
/// class Book(models.Model):
///     author = models.ForeignKey("Author", on_delete=models.CASCADE)
///     sequel = models.ForeignKey("self", null=True, on_delete=models.SET_NULL)
/// ```
///
/// A reference with an app label, such as `"library.Author"`, names a model of another app.
fn related_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declaring: StaticClassLiteral<'db>,
    model: StaticClassLiteral<'db>,
    field: &Field<'db>,
) -> Option<Type<'db>> {
    model_reference(db, env, declaring, model, field.to?)
}

/// Return the model that `reference` denotes: a model class, the name of a model in the module of
/// `declaring`, or `"self"` for `model`.
fn model_reference<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declaring: StaticClassLiteral<'db>,
    model: StaticClassLiteral<'db>,
    reference: Type<'db>,
) -> Option<Type<'db>> {
    let to = reference;
    let related = match to.as_string_literal() {
        Some(reference) if reference.value(db) == "self" => {
            return Some(Type::instance(db, env, model.default_specialization(db)));
        }
        Some(reference) => {
            let file = declaring.body_scope(db).program_file(db);
            match reference.value(db).split_once('.') {
                Some((app_label, name)) => app_model_symbol(db, env, file, app_label, name)?,
                None => global_symbol(db, file, reference.value(db))
                    .place
                    .ignore_possibly_undefined()?,
            }
        }
        None => to,
    }
    .to_instance_approximation(db, env)?;

    let (related_class, _) = related.nominal_class(db, env)?.static_class_literal(db)?;
    is_model(db, related_class).then_some(related)
}

/// Look up `name` in the `models` module of the app with the label `app_label`, for a reference
/// such as `"library.Author"` in `file`.
///
/// An app's label is by default the last component of its package name, and its models are
/// importable from the `models` module of that package. We do not read the project's settings to
/// find the installed apps. Instead, we look for a package named `app_label` next to each package
/// that contains `file`, starting with the innermost. For a reference in `shop/reviews/models.py`,
/// we try `shop.reviews.library.models`, `shop.library.models` and then `library.models`.
///
/// An app that sets a different label in its `AppConfig` is not found.
fn app_model_symbol<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    file: ProgramFile<'db>,
    app_label: &str,
    name: &str,
) -> Option<Type<'db>> {
    let models = ModuleName::from_components([app_label, "models"])?;
    let importing_file = ImportingFile::File(file.file(db), env.resolver_environment(db));
    let containing_packages = file_to_module(db, file.resolver_file(db))?
        .name(db)
        .ancestors()
        .skip(1);

    containing_packages
        .map(|mut package| {
            package.extend(&models);
            package
        })
        .chain(std::iter::once(models.clone()))
        .find_map(|candidate| {
            let module = resolve_module(db, importing_file, &candidate)?.file(db)?;
            global_symbol(db, ProgramFile::new(db, module, env.program(db)), name)
                .place
                .ignore_possibly_undefined()
        })
}

/// Return a more precise type for the relation field `name` of the model `class`, if the stubs
/// could not determine its related model.
///
/// `ty` is the type of the field according to the stubs. django-stubs infers the related model
/// from the type arguments of the field class, which are unknown for a string reference and do
/// not exist for a subclass that is not generic:
/// ```py
/// class TrackedForeignKey(models.ForeignKey): ...
///
/// class Book(models.Model):
///     author = models.ForeignKey("Author", on_delete=models.CASCADE)
///     editor = TrackedForeignKey(Author, on_delete=models.CASCADE)
/// ```
///
/// Both fields get the type `ForeignKey[Author, Author]`.
pub(in crate::types) fn refined_field_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    let (field_class, specialization) = ty.nominal_class(db, env)?.static_class_literal(db)?;
    if field_class.name(db) == "ManyToManyField" {
        return refined_many_to_many_type(db, env, class, name, field_class, specialization?);
    }
    // A related model that the stubs or the constructor already determined takes precedence.
    if specialization.is_some_and(|specialization| {
        specialization.types(db).iter().any(|ty| {
            !ty.filter_union(db, env, |element| !element.is_none(db))
                .is_dynamic()
        })
    }) || !is_foreign_key(db, field_class)
        || !is_model(db, class)
    {
        return None;
    }

    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == name)?;
    let model = related_model(db, env, class, class, field)?;
    let value = if field.metadata.null {
        UnionType::from_two_elements(db, env, model, Type::none(db, env))
    } else {
        model
    };

    // Specialize the field class itself, or its nearest generic base class if it is not generic.
    let generic_class = field_class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .map(|(base, _)| base)
        .find(|base| {
            base.generic_context(db)
                .is_some_and(|generic_context| generic_context.len(db) == 2)
        })?;
    Some(Type::instance(
        db,
        env,
        generic_class.apply_specialization(db, |generic_context| {
            generic_context.specialize(db, vec![value, value])
        }),
    ))
}

/// Return a more precise type for the `ManyToManyField` `name` of the model `class`, if the
/// related model or the model of the intermediate table is given as a string reference:
/// ```py
/// class Book(models.Model):
///     tags = models.ManyToManyField("Tag", through="Tagging")
/// ```
fn refined_many_to_many_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    field_class: StaticClassLiteral<'db>,
    specialization: Specialization<'db>,
) -> Option<Type<'db>> {
    let &[to, through] = specialization.types(db) else {
        return None;
    };
    if !(to.is_dynamic() || through.is_dynamic()) || !is_model(db, class) {
        return None;
    }
    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == name)?;
    let resolve = |current: Type<'db>, reference: Option<Type<'db>>| {
        if current.is_dynamic() {
            reference.and_then(|reference| model_reference(db, env, class, class, reference))
        } else {
            None
        }
    };
    let refined_to = resolve(to, field.to);
    let refined_through = match field.through {
        None if through.is_dynamic() => {
            automatic_through_model(db, env, class, field, refined_to.unwrap_or(to))
        }
        reference => resolve(through, reference),
    };
    if refined_to.is_none() && refined_through.is_none() {
        return None;
    }
    Some(Type::instance(
        db,
        env,
        field_class.apply_specialization(db, |generic_context| {
            generic_context.specialize(
                db,
                vec![refined_to.unwrap_or(to), refined_through.unwrap_or(through)],
            )
        }),
    ))
}

/// Return the model of the intermediate table that Django creates for the `ManyToManyField`
/// `field` of the model `class` without a `through` argument. `to` is the related model.
///
/// The model is named after the model and the field, and has a foreign key to each of the two
/// models, named after them:
/// ```py
/// class Book(models.Model):
///     tags = models.ManyToManyField(Tag)
///
/// # Book.tags.through is the model `Book_tags`, with the foreign keys `book` and `tag`.
/// ```
///
/// For a relation from a model to itself, the foreign keys are named `from_<model>` and
/// `to_<model>`. The model has the attributes of the foreign keys and of the primary key, with
/// the types that they have on an instance.
fn automatic_through_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    field: &Field<'db>,
    to: Type<'db>,
) -> Option<Type<'db>> {
    // A field of an abstract model creates a separate model for each concrete subclass.
    if model_metadata(db, class).is_abstract {
        return None;
    }
    let (to_class, _) = to.nominal_class(db, env)?.static_class_literal(db)?;
    let source = Type::instance(db, env, class.default_specialization(db));
    let primary_key = |model: Type<'db>| {
        model
            .member(db, env, "pk")
            .place
            .ignore_possibly_undefined()
    };

    let source_name = class.name(db).to_lowercase();
    let to_name = to_class.name(db).to_lowercase();
    let (source_name, to_name) = if source_name == to_name {
        (format!("from_{source_name}"), format!("to_{to_name}"))
    } else {
        (source_name, to_name)
    };
    let id = KnownClass::Int.to_instance(db, env);
    let members: Box<[(Name, Type<'db>)]> = Box::new([
        (Name::new_static("id"), id),
        (Name::new_static("pk"), id),
        (Name::new(format!("{source_name}_id")), primary_key(source)?),
        (Name::new(format!("{to_name}_id")), primary_key(to)?),
        (Name::new(source_name), source),
        (Name::new(to_name), to),
    ]);

    let through = DynamicClassLiteral::new(
        db,
        Name::new(format!("{}_{}", class.name(db), field.name)),
        DynamicClassAnchor::Implicit {
            owner: class,
            explicit_bases: Box::new([KnownClass::DjangoModel.to_class_literal(db, env)]),
        },
        members,
        false,
        None,
    );
    Some(Type::instance(
        db,
        env,
        ClassLiteral::Dynamic(through).default_specialization(db),
    ))
}

/// Returns whether `name` is the name that a field of the model `class` is assigned to, but not
/// the name of the attribute that Django creates for it. `ty` is the type of the class member.
///
/// Django removes the original attribute of a field whose `name` argument renames it:
/// ```py
/// class Article(models.Model):
///     views = models.IntegerField(name="view_count")
///
/// # Article().views does not exist.
/// ```
pub(in crate::types) fn is_renamed_field<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    ty: Type<'db>,
) -> bool {
    ty.nominal_class(db, env).is_some_and(|field_class| {
        field_class
            .iter_mro(db)
            .filter_map(ClassBase::into_class)
            .any(|base| base.is_known(db, KnownClass::DjangoField))
    }) && is_model(db, class)
        && own_model_fields(db, class)
            .iter()
            .any(|field| field.symbol == name && field.name != name)
}

/// Synthesize `<parent>_ptr` and `<parent>_ptr_id` attributes for multi-table inheritance.
///
/// A model that inherits from a concrete model links to it through an automatic `OneToOneField`
/// named after the parent model. For example, `Ebook().book_ptr` has type `Book`, and
/// `Ebook().book_ptr_id` has type `int`, matching `Book`'s automatic primary key:
/// ```py
/// class Book(models.Model):
///     pass
///
/// class Ebook(Book):
///     pass
/// ```
///
/// This only covers the links from `class` to its direct bases. Subclasses of `class` find them
/// through the regular attribute lookup on their base classes.
fn parent_link_member<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    let (parent_name, is_id) = match name.strip_suffix("_ptr_id") {
        Some(parent_name) => (parent_name, true),
        None => (name.strip_suffix("_ptr")?, false),
    };

    // A proxy model uses the database table of its parent, so there is nothing to link.
    if model_metadata(db, class).is_proxy {
        return None;
    }

    let parent = class
        .explicit_bases(db)
        .iter()
        .filter_map(|base| concrete_model(db, base.to_class_type(db)?))
        .find(|parent| parent.name(db).to_lowercase() == parent_name)?;
    let parent_ty =
        Type::instance(db, env, parent).apply_optional_specialization(db, specialization);

    // A field declared with `parent_link=True` replaces the automatic link to its related model.
    // If we cannot tell which model that is, we do not synthesize a link.
    if own_model_fields(db, class)
        .iter()
        .filter(|field| field.metadata.parent_link)
        .any(|field| {
            related_model(db, env, class, class, field).is_none_or(|related| related == parent_ty)
        })
    {
        return None;
    }

    if is_id {
        parent_ty
            .member(db, env, "pk")
            .place
            .ignore_possibly_undefined()
    } else {
        Some(parent_ty)
    }
}

/// Returns the model whose database table stores the rows of `model`: a concrete model itself, or
/// the nearest concrete parent of a proxy model. Abstract models have no database table.
fn concrete_model<'db>(db: &'db dyn Db, model: ClassType<'db>) -> Option<ClassType<'db>> {
    let metadata = |class: ClassType<'db>| {
        let (class, _) = class.static_class_literal(db)?;
        is_model(db, class).then(|| model_metadata(db, class))
    };
    if metadata(model)?.is_abstract {
        return None;
    }
    model
        .iter_mro(db)
        .filter_map(ClassBase::into_class)
        .find(|base| metadata(*base).is_some_and(|base| !base.is_abstract && !base.is_proxy))
}

/// Synthesize `get_<field>_display()` for fields with `choices` defined.
fn display_method<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    let field_name = name.strip_circumfix("get_", "_display")?;
    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == field_name)?;
    if !field.metadata.has_choices {
        return None;
    }

    // Construct `def get_<field>_display(self: MyModel) -> str`.
    let self_ty = Type::instance(
        db,
        env,
        class.apply_optional_specialization(db, specialization),
    );
    let signature = Signature::new(
        Parameters::standard([
            Parameter::positional_or_keyword(Name::new_static("self")).with_annotated_type(self_ty)
        ]),
        KnownClass::Str.to_instance(db, env),
    );
    Some(Type::function_like_callable(db, signature))
}

/// Synthesize `get_next_by_<field>()` and `get_previous_by_<field>()` for non-nullable date fields.
fn date_navigation_method<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    specialization: Option<Specialization<'db>>,
    name: &str,
) -> Option<Type<'db>> {
    let field_name = name
        .strip_prefix("get_next_by_")
        .or_else(|| name.strip_prefix("get_previous_by_"))?;
    let field = own_model_fields(db, class)
        .iter()
        .find(|field| field.name == field_name)?;
    if field.metadata.null {
        return None;
    }
    // We add these methods for all field types that inherit from `DateField`, like `DateTimeField`.
    let date_field = KnownClass::DjangoDateField.try_to_class_literal(db, env)?;
    let field_class = field.ty.nominal_class(db, env)?;
    if !field_class.is_subtype_of_class_literal(db, date_field.into()) {
        return None;
    }

    // Construct `def get_{next,previous}_by_<field>(self: Self, /, **kwargs: Any) -> Self`.
    let self_typevar = BoundTypeVarInstance::synthetic_self(
        db,
        SelfTypeVarOrigin::Class(class.into()),
        Type::instance(
            db,
            env,
            class.apply_optional_specialization(db, specialization),
        ),
        BindingContext::Synthetic(env.program(db)),
    );
    let self_ty = Type::TypeVar(self_typevar);
    let generic_context = GenericContext::from_typevar_instances(db, env, [self_typevar]);
    let signature = Signature::new_generic(
        Some(generic_context),
        Parameters::standard([
            Parameter::positional_only(Some(Name::new_static("self"))).with_annotated_type(self_ty),
            Parameter::keyword_variadic(Name::new_static("kwargs"))
                .with_annotated_type(Type::any()),
        ]),
        self_ty,
    );
    Some(Type::function_like_callable(db, signature))
}
