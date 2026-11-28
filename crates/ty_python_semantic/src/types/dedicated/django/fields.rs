//! The fields of a model and its `Meta` options, read from the model's class body.

use ruff_db::parsed::{ParsedModuleRef, parsed_module};
use ruff_python_ast::{self as ast, name::Name};
use ty_module_resolver::{KnownModule, file_to_module};
use ty_python_core::{
    definition::{Definition, DefinitionKind},
    place_table, semantic_index, use_def_map,
};

use crate::place::place_from_bindings;
use crate::types::call::{Argument, Binding, CallArguments};
use crate::types::infer::nearest_enclosing_class;
use crate::types::member::class_member;
use crate::types::{
    ClassBase, KnownClass, Specialization, StaticClassLiteral, Type, UnionType, binding_type,
    definition_expression_type,
};
use crate::{Db, ProgramEnvironment};

use super::relations::{is_foreign_key, refined_field_type};
use super::{inherits_from, is_django_file, is_model};

/// The type of a Django model field, and metadata extracted from the field construction.
///
/// For example, `isbn` is a `CharField` with `metadata.primary_key` set to `true`.
/// ```py
/// class Book(models.Model):
///     isbn = CharField(max_length=13, primary_key=True)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct Field<'db> {
    /// The name of the attribute that Django creates for the field.
    pub(super) name: Name,
    /// The name that the field is assigned to in the class body. It differs from `name` if the
    /// field construction has a `name` argument.
    pub(super) symbol: Name,
    pub(super) ty: Type<'db>,
    /// The first argument of the field construction, which names the related model of a relation
    /// field: `Author` or `"Author"` in `ForeignKey(Author, ...)`.
    pub(super) to: Option<ModelReference<'db>>,
    /// The type of the `through` argument of a `ManyToManyField`, which names the model of the
    /// intermediate table.
    pub(super) through: Option<Type<'db>>,
    pub(super) metadata: FieldMetadata,
}

impl<'db> Field<'db> {
    /// Create a field from its definition in the body of a model, or return `None` if the
    /// definition does not assign a Django field.
    ///
    /// The type of the field is the binding type, which follows the attribute's annotation,
    /// unless that is not the field the assigned value constructs. An annotation can narrow the
    /// types of a field that matches it, and then decides them:
    /// ```py
    /// class Leaflet(models.Model):
    ///     title: models.CharField[str, str] = char_field()  # returns `CharField[Any, Any]`
    /// ```
    /// A value that does not match the annotation reports `invalid-assignment`, and its binding
    /// type is still the annotation. Django creates the field that the value constructs, so the
    /// type of the value is the type of the field, and the mistake is reported only once:
    /// ```py
    /// class Shelf(models.Model):
    ///     limit: models.IntegerField | None = models.IntegerField(null=True)
    ///     label: models.CharField = models.CharField(max_length=20, null=True)
    /// ```
    /// Both fields are nullable, so `Shelf(limit=None, label=None)` is fine. Reading the attribute
    /// follows its annotation, through the class member, not through this type.
    ///
    /// If the value is not a field, as for `email: models.EmailField = ...` in a stub file, the
    /// binding type decides.
    fn from_definition(
        db: &'db dyn Db,
        env: &ProgramEnvironment<'db>,
        module: &ParsedModuleRef,
        name: Name,
        definition: Definition<'db>,
    ) -> Option<Self> {
        let value = match definition.kind(db) {
            DefinitionKind::Assignment(assignment) if assignment.unpack().is_none() => {
                Some(assignment.value(module))
            }
            DefinitionKind::AnnotatedAssignment(assignment) => assignment.value(module),
            _ => None,
        };
        let binding_ty = binding_type(db, definition);
        let binding_is_field = is_field_instance(db, env, binding_ty);
        let ty = value
            .map(|value| definition_expression_type(db, definition, value))
            .filter(|&value_ty| {
                is_field_instance(db, env, value_ty)
                    && !(binding_is_field && value_ty.is_assignable_to(db, env, binding_ty))
            })
            .or_else(|| binding_is_field.then_some(binding_ty))?;
        let mut field = Field {
            symbol: name.clone(),
            name,
            ty,
            to: None,
            through: None,
            metadata: FieldMetadata::default(),
        };
        let Some(call) = value.and_then(ast::Expr::as_call_expr) else {
            return Some(field);
        };

        field.to = call
            .arguments
            .find_argument_value("to", 0)
            .map(|to| ModelReference::from_expression(db, definition, to));
        // The type of the keyword argument `name`, if the call passes it.
        let keyword = |name: &str| {
            call.arguments
                .find_keyword(name)
                .map(|keyword| definition_expression_type(db, definition, &keyword.value))
        };
        let is_true = |name: &str| keyword(name).and_then(Type::as_bool_literal) == Some(true);
        let string = |name: &str| {
            keyword(name)
                .and_then(Type::as_string_literal)
                .map(|value| Name::new(value.value(db)))
        };

        field.through = keyword("through");
        if let Some(name) = string("name")
            && !name.is_empty()
        {
            field.name = name;
        }
        field.metadata.primary_key = is_true("primary_key");
        field.metadata.has_choices = keyword("choices").is_some_and(|choices| !choices.is_none(db));
        field.metadata.null = is_true("null");
        field.metadata.to_field = string("to_field");
        field.metadata.related_name = string("related_name");
        field.metadata.related_query_name = string("related_query_name");
        field.metadata.parent_link = is_true("parent_link");
        field.metadata.symmetrical = keyword("symmetrical").and_then(Type::as_bool_literal);
        Some(field)
    }
}

/// The argument of a relation field that names its related model, or of a `ManyToManyField` that
/// names the model of its intermediate table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) enum ModelReference<'db> {
    /// The type of the argument: a model class, or a string such as `"Author"`,
    /// `"library.Author"` or `"self"`.
    Type(Type<'db>),
    /// `settings.AUTH_USER_MODEL`, which names the project's user model. Its type is only `str`,
    /// so we recognize the attribute itself. Relations to the user model are usually written this
    /// way, in the project and in installed apps:
    /// ```py
    /// from django.conf import settings
    ///
    /// class Token(models.Model):
    ///     user = models.OneToOneField(settings.AUTH_USER_MODEL, on_delete=models.CASCADE)
    /// ```
    UserModelSetting,
}

impl<'db> ModelReference<'db> {
    fn from_expression(db: &'db dyn Db, definition: Definition<'db>, expr: &ast::Expr) -> Self {
        if let ast::Expr::Attribute(attribute) = expr
            && attribute.attr.as_str() == "AUTH_USER_MODEL"
            && definition_expression_type(db, definition, &attribute.value)
                .is_instance_of(db, KnownClass::DjangoLazySettings)
        {
            return ModelReference::UserModelSetting;
        }
        ModelReference::Type(definition_expression_type(db, definition, expr))
    }

    /// The value of a reference given as a string literal.
    pub(super) fn as_str(self, db: &'db dyn Db) -> Option<&'db str> {
        match self {
            ModelReference::Type(ty) => Some(ty.as_string_literal()?.value(db)),
            ModelReference::UserModelSetting => None,
        }
    }
}

/// Metadata extracted from a Django field's construction.
#[derive(Debug, Clone, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
#[expect(clippy::struct_excessive_bools)]
pub(super) struct FieldMetadata {
    pub(super) primary_key: bool,
    pub(super) has_choices: bool,
    pub(super) null: bool,
    pub(super) to_field: Option<Name>,
    pub(super) parent_link: bool,
    pub(super) related_name: Option<Name>,
    pub(super) related_query_name: Option<Name>,
    /// The `symmetrical` argument of a `ManyToManyField`, if it is a bool literal.
    pub(super) symmetrical: Option<bool>,
}

/// All fields defined on this Django model (not including base class fields).
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn own_model_fields<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> Box<[Field<'db>]> {
    let env = ProgramEnvironment::from_scope(class.body_scope(db));
    let table = place_table(db, class.body_scope(db));
    let module = parsed_module(db, class.body_scope(db).python_file(db)).load(db);
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
            Field::from_definition(db, &env, &module, name.clone(), definition)
        })
        .collect()
}

/// Whether `ty` is an instance of a Django field class.
fn is_field_instance<'db>(db: &'db dyn Db, env: &ProgramEnvironment<'db>, ty: Type<'db>) -> bool {
    ty.nominal_class(db, env).is_some_and(|class| {
        class
            .iter_mro(db)
            .filter_map(ClassBase::into_class)
            .any(|base| base.is_known(db, KnownClass::DjangoField))
    })
}

/// Metadata extracted from a Django model's `Meta` configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, get_size2::GetSize, salsa::SalsaValue)]
pub(super) struct ModelMetadata {
    pub(super) is_abstract: bool,
    pub(super) is_proxy: bool,
}

#[salsa::tracked(returns(copy), heap_size=ruff_memory_usage::heap_size)]
pub(super) fn model_metadata<'db>(
    db: &'db dyn Db,
    class: StaticClassLiteral<'db>,
) -> ModelMetadata {
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
pub(super) fn is_abstract<'db>(
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
    class_body_value(db, env, meta, name).map(|value| value == Type::bool_literal(true))
}

/// Return the name of the manager that `Meta.default_manager_name` of the model `class` names.
///
/// Django reads the option from the model's own `Meta` class, or else from the `Meta` class that
/// the model inherits from an abstract base class, and a `Meta` class can inherit the option from
/// its own base classes:
/// ```py
/// class Book(models.Model):
///     objects = models.Manager()
///     published = PublishedManager()
///
///     class Meta:
///         default_manager_name = "published"
/// ```
pub(super) fn default_manager_name<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
) -> Option<&'db str> {
    // Django removes the `Meta` class from a model that is not abstract, so a model only inherits
    // the `Meta` class of an abstract model.
    let meta = class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .map(|(base, _)| base)
        .filter(|base| {
            *base == class || (is_model(db, *base) && model_metadata(db, *base).is_abstract)
        })
        .find_map(|base| own_meta(db, base))?;
    let name = meta
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .map_while(|meta_base| meta_base.static_class_literal(db))
        .find_map(|(meta_base, _)| class_body_value(db, env, meta_base, "default_manager_name"))?;
    Some(name.as_string_literal()?.value(db))
}

/// Return the type of the value that the body of the class `class` assigns to `name`, such as an
/// option of a `Meta` class or the `name` of an `AppConfig` class.
pub(super) fn class_body_value<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
) -> Option<Type<'db>> {
    let scope = class.body_scope(db);
    let symbol = place_table(db, scope).symbol_id(name)?;
    // The public type of a literal is widened, such as `True` to `bool`, so read the bindings'
    // literal values instead.
    place_from_bindings(
        db,
        env,
        use_def_map(db, scope).end_of_scope_symbol_bindings(symbol),
    )
    .place
    .ignore_possibly_undefined()
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

/// Synthesize the attribute of a field whose `name` argument differs from the name it is assigned
/// to. Django creates the attribute under the name given by the argument:
/// ```py
/// class Article(models.Model):
///     views = models.IntegerField(name="view_count")
///
/// # Article().view_count has type int.
/// ```
pub(super) fn renamed_field<'db>(
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

/// Returns whether `name` is the name that a field of the model `class` is assigned to, but not
/// the name of the attribute that Django creates for it.
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
    class: StaticClassLiteral<'db>,
    name: &str,
) -> bool {
    is_model(db, class)
        && own_model_fields(db, class)
            .iter()
            .any(|field| field.symbol == name && field.name != name)
}

/// Return a more precise type for the field `name` of the model `class`, whose type according to
/// the stubs is `ty`: a field that converts its values (see [`converted_field_type`]), or a
/// relation field whose related model the stubs could not determine.
pub(super) fn model_field_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    class: StaticClassLiteral<'db>,
    name: &str,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    converted_field_type(db, env, ty).or_else(|| refined_field_type(db, env, class, name, ty))
}

/// Return the type of an instance of the field class `field_class` whose values are assigned as
/// `write` and read as `read`.
///
/// The stubs give each field class two type parameters, for the values that can be assigned to
/// the field and for the values that it holds. A subclass that does not declare them is
/// specialized as its nearest generic base class.
pub(super) fn specialized_field_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    field_class: StaticClassLiteral<'db>,
    write: Type<'db>,
    read: Type<'db>,
) -> Option<Type<'db>> {
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
            generic_context.specialize(db, vec![write, read])
        }),
    ))
}

/// Add `None` to the write and read types of `instance`, a Django field that a constructor call
/// with the matching overloads `overloads` and the arguments `arguments` creates, if the call
/// passes `null=True`. Returns `None` if the call creates no such field.
///
/// django-stubs gives `BooleanField(null=True)` the type `BooleanField[bool | None, bool | None]`
/// through the `self` annotation of an `__init__` overload. A declared type is the type context
/// of the call and can select the non-nullable types instead, and so can explicit type arguments:
/// ```py
/// class Settings(models.Model):
///     enabled: models.BooleanField = models.BooleanField(null=True)
///     confirmed: models.BooleanField = models.BooleanField[bool, bool](null=True)
/// ```
/// A `NULL` column reads as `None` whatever the call says, so `null=True` decides the types, as
/// in the django-stubs mypy plugin. Both assignments above then report `invalid-assignment`.
///
/// The type of the `null` parameter of a matching overload decides. The call's `null` keyword
/// argument is not enough on its own: it is inferred against the `null` parameter of each
/// overload, and when those inferences differ it has no single type. The automatic fields,
/// such as `AutoField`, take `null` through the `**kwargs` of `AutoFieldMixin.__init__`, which has
/// no `null` parameter, so for an overload that takes `**kwargs` the keyword argument decides:
/// ```py
/// models.AutoField(primary_key=True, null=True)  # AutoField[int | str | None, int | None]
/// ```
///
/// Only the fields defined in `django.db.models.fields` and Django's foreign keys use their two
/// type arguments for the values that `null=True` makes nullable. A `FileField`, for example,
/// still reads as a `FieldFile` for a `NULL` column, and a project's field class declares its
/// own type arguments.
pub(in crate::types) fn nullable_field_type<'db, 'a>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    instance: Type<'db>,
    overloads: impl IntoIterator<Item = &'a Binding<'db>>,
    arguments: &CallArguments<'_, 'db>,
) -> Option<Type<'db>>
where
    'db: 'a,
{
    let (class, specialization) = instance.class_specialization(db, env)?;
    let &[write, read] = specialization.types(db) else {
        return None;
    };
    let null_argument = || {
        arguments
            .iter()
            .find_map(|argument| match argument.kind() {
                Argument::Keyword("null") => Some(argument.source_type()),
                _ => None,
            })
            .flatten()
    };
    let is_nullable_field = is_nullable_field_class(db, class)
        && overloads.into_iter().any(|overload| {
            let null = match overload.parameter_type_by_name(db, "null", false) {
                Ok(null) => null,
                Err(_) if overload.signature.parameters().keyword_variadic().is_some() => {
                    null_argument()
                }
                Err(_) => None,
            };
            null.and_then(Type::as_bool_literal) == Some(true)
        });
    if !is_nullable_field {
        return None;
    }
    let none = Type::none(db, env);
    let types: Box<[Type<'db>]> = Box::new([
        UnionType::from_two_elements(db, env, write, none),
        UnionType::from_two_elements(db, env, read, none),
    ]);
    // Keep the materialization kind of the specialization. A specialization only has tuple
    // element types for `tuple`, which is not a field class.
    let nullable = Specialization::new(
        db,
        specialization.generic_context(db),
        types,
        specialization.materialization_kind(db),
        None,
    );
    Some(Type::instance(
        db,
        env,
        class.apply_specialization(db, |_| nullable),
    ))
}

/// Whether `instance` is an instance of a field class whose values [`nullable_field_type`] can
/// make nullable. This is a cheap check that a constructor call can make before it computes its
/// return type for [`nullable_field_type`].
pub(in crate::types) fn is_nullable_field_instance<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    instance: Type<'db>,
) -> bool {
    instance
        .nominal_class(db, env)
        .and_then(|class| class.static_class_literal(db))
        .is_some_and(|(class, _)| is_nullable_field_class(db, class))
}

/// Whether `class` is one of the fields defined in `django.db.models.fields` or one of Django's
/// foreign keys, which use their two type arguments for the values that `null=True` makes
/// nullable.
fn is_nullable_field_class<'db>(db: &'db dyn Db, class: StaticClassLiteral<'db>) -> bool {
    file_to_module(db, class.program_file(db).resolver_file(db)).is_some_and(|module| {
        module.is_known(db, KnownModule::DjangoDbModelsFields)
            || (module.name(db).first_component() == "django" && is_foreign_key(db, class))
    })
}

/// Return the type of a field `ty` whose class converts its values, and declares their types only
/// in its conversion methods.
///
/// Django converts a value read from the database with `from_db_value()`, and a value assigned to
/// the field with `to_python()` when the model is validated. A field class outside Django that
/// overrides them without type arguments for the field class it extends gets the value types of
/// that class, which are usually wrong:
/// ```py
/// class ListField(models.CharField):
///     def from_db_value(self, value, expression, connection) -> list[str]:
///         return value.split(",")
/// ```
///
/// The values of such a field have the declared return type of `from_db_value()`, or else of
/// `to_python()`, or `Any` if neither method has a return annotation. Any value can be assigned,
/// because the field converts it when it is saved. A field class that declares its type
/// arguments, as in `class ListField(models.CharField[list[str], list[str]])`, keeps them.
pub(super) fn converted_field_type<'db>(
    db: &'db dyn Db,
    env: &ProgramEnvironment<'db>,
    ty: Type<'db>,
) -> Option<Type<'db>> {
    let (field_class, _) = ty.nominal_class(db, env)?.static_class_literal(db)?;
    // The values of relation fields are typed separately, from their related model.
    if !inherits_from(db, field_class, KnownClass::DjangoField)
        || is_foreign_key(db, field_class)
        || inherits_from(db, field_class, KnownClass::DjangoManyToManyField)
    {
        return None;
    }
    // The stubs declare the value types of Django's own fields, so only the methods and bases of
    // the classes outside Django matter.
    let own_classes: Vec<StaticClassLiteral<'db>> = field_class
        .iter_mro(db, None)
        .filter_map(ClassBase::into_class)
        .filter_map(|base| base.static_class_literal(db))
        .map(|(base, _)| base)
        .filter(|base| !is_django_file(db, base.program_file(db)))
        .collect();
    let declares_value_types = own_classes.iter().any(|class| {
        class.explicit_bases(db).iter().any(|base| {
            matches!(base, Type::GenericAlias(alias)
                if inherits_from(db, alias.origin(db), KnownClass::DjangoField))
        })
    });
    if declares_value_types {
        return None;
    }
    // The first definition of the method `name` in a class outside Django.
    let conversion_method = |name: &str| {
        own_classes.iter().find_map(|class| {
            class_member(db, class.body_scope(db), name).ignore_possibly_undefined()
        })
    };
    let from_db_value = conversion_method("from_db_value");
    let to_python = conversion_method("to_python");
    if from_db_value.is_none() && to_python.is_none() {
        return None;
    }
    // The annotated return type of a conversion method. An unannotated method has no declared
    // return type, and we do not use its inferred one. An overloaded method, and a method whose
    // decorator makes it something other than a plain function, have none either.
    let declared_return_type = |method: Type<'db>| {
        if let Some(function) = method.as_function_literal()
            && function.has_explicit_return_annotation(db)
            && let [signature] = function.signature(db).overloads.as_slice()
        {
            Some(signature.return_ty)
        } else {
            None
        }
    };
    let read = from_db_value
        .and_then(declared_return_type)
        .or_else(|| to_python.and_then(declared_return_type))
        .unwrap_or_else(Type::any);
    specialized_field_type(db, env, field_class, Type::any(), read)
}
