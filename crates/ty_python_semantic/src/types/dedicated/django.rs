use ruff_db::parsed::parsed_module;
use ruff_python_ast::name::Name;
use ty_module_resolver::{KnownModule, file_to_module};
use ty_python_core::{
    ProgramFile,
    definition::{Definition, DefinitionKind},
    global_scope, place_table, semantic_index, use_def_map,
};

use crate::place::{global_symbol, known_module_symbol, place_from_bindings};
use crate::types::call::Binding;
use crate::types::infer::nearest_enclosing_class;
use crate::types::member::class_member;
use crate::types::{
    BindingContext, BoundTypeVarInstance, ClassBase, ClassType, GenericContext, KnownClass,
    Parameter, Parameters, SelfTypeVarOrigin, Signature, Specialization, StaticClassLiteral, Type,
    UnionType, binding_type, definition_expression_type,
};
use crate::{Db, ProgramEnvironment};

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
    name: Name,
    ty: Type<'db>,
    /// The type of the first argument of the field construction, which names the related model
    /// of a relation field: `Author` or `"Author"` in `ForeignKey(Author, ...)`.
    to: Option<Type<'db>>,
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
            name,
            ty,
            to: None,
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

    // Django does not create members with a private name.
    if name.starts_with('_') || !is_model(db, class) {
        return None;
    }
    // A quick filter on the name of the members that depend on the fields of the model. A reverse
    // accessor can have any name.
    if !(matches!(name, "id" | "pk")
        || name.ends_with("_id")
        || name.ends_with("_ptr")
        || name.starts_with("get_") && name.ends_with("_display")
        || name.starts_with("get_next_by_")
        || name.starts_with("get_previous_by_"))
    {
        return renamed_field(db, class, specialization, name)
            .or_else(|| reverse_accessor(db, class, name));
    }
    match name {
        "id" | "pk" => primary_key_member(db, env, class, specialization, name),
        _ => display_method(db, env, class, specialization, name)
            .or_else(|| date_navigation_method(db, env, class, specialization, name))
            .or_else(|| foreign_key_id_member(db, env, class, specialization, name))
            .or_else(|| parent_link_member(db, env, class, specialization, name))
            .or_else(|| reverse_accessor(db, class, name)),
    }
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
/// We only find relation fields of the models in the module that defines the related model.
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

/// All reverse accessors that the models in the module of `class` add to it.
#[salsa::tracked(returns(deref), cycle_initial=|_, _, _| Box::default(), heap_size=ruff_memory_usage::heap_size)]
fn reverse_accessors<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Box<[ReverseAccessor<'db>]> {
    let scope = global_scope(db, class.body_scope(db).program_file(db));
    let env = ProgramEnvironment::from_scope(scope);
    let target = Type::instance(db, &env, class.default_specialization(db));

    let sources = use_def_map(db, scope)
        .all_end_of_scope_symbol_bindings()
        .flat_map(|(_, bindings)| bindings.filter_map(|binding| binding.binding.definition()))
        .filter_map(|definition| binding_type(db, definition).as_class_literal()?.as_static())
        // Abstract models have no table, so nothing can refer to their rows.
        .filter(|source| is_model(db, *source) && !model_metadata(db, *source).is_abstract);

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
) -> Option<Type<'db>> {
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
                foreign_key_id_type(db, env, base, field, specialization)
                    .unwrap_or_else(|| field.ty.apply_optional_specialization(db, specialization))
            });
        }
    }

    // Abstract models do not have an automatic primary key.
    if model_metadata(db, class).is_abstract {
        return None;
    }

    known_module_symbol(db, env, KnownModule::DjangoDbModelsFields, "AutoField")
        .place
        .ignore_possibly_undefined()?
        .to_instance_approximation(db, env)
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
/// A reference with an app label, such as `"library.Author"`, is not resolved.
fn related_model<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    declaring: StaticClassLiteral<'db>,
    model: StaticClassLiteral<'db>,
    field: &Field<'db>,
) -> Option<Type<'db>> {
    let to = field.to?;
    let related = match to.as_string_literal() {
        Some(reference) if reference.value(db) == "self" => {
            return Some(Type::instance(db, env, model.default_specialization(db)));
        }
        Some(reference) => global_symbol(
            db,
            declaring.body_scope(db).program_file(db),
            reference.value(db),
        )
        .place
        .ignore_possibly_undefined()?,
        None => to,
    }
    .to_instance_approximation(db, env)?;

    let (related_class, _) = related.nominal_class(db, env)?.static_class_literal(db)?;
    is_model(db, related_class).then_some(related)
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
