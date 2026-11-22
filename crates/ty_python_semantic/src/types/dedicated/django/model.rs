//! Members that Django adds to every model: the primary key, `<field>_id` attributes, generated
//! methods, parent links, exceptions and `__init__`.

use ruff_python_ast::name::Name;
use rustc_hash::FxHashSet;
use ty_module_resolver::KnownModule;
use ty_python_core::place_table;

use crate::place::known_module_symbol;
use crate::types::class::{ClassLiteral, DynamicClassAnchor, DynamicClassLiteral};
use crate::types::member::class_member;
use crate::types::{
    BindingContext, BoundTypeVarInstance, ClassBase, ClassType, GenericContext, KnownClass,
    Parameter, Parameters, SelfTypeVarOrigin, Signature, Specialization, StaticClassLiteral, Type,
    UnionType,
};
use crate::{Db, ProgramEnvironment};

use super::fields::{Field, model_metadata, own_model_fields};
use super::project::project_models;
use super::relations::{is_foreign_key, refined_field_type, related_model};
use super::{inherits_from, is_model};

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
pub(super) fn init_method<'db>(
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
            if field_class
                .static_class_literal(db)
                .is_some_and(|(field_class, _)| {
                    inherits_from(db, field_class, KnownClass::DjangoManyToManyField)
                })
            {
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
pub(super) fn model_exception<'db>(
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

pub(super) enum PrimaryKeyMember<'db> {
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
pub(super) fn primary_key_member<'db>(
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
pub(super) fn foreign_key_id_member<'db>(
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
pub(super) fn parent_link_member<'db>(
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
pub(super) fn concrete_model<'db>(
    db: &'db dyn Db,
    model: ClassType<'db>,
) -> Option<ClassType<'db>> {
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
pub(super) fn display_method<'db>(
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
pub(super) fn date_navigation_method<'db>(
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
